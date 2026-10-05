//! Sends synced spool records to the analytics service.

use std::path::PathBuf;
use std::time::Duration;

use pacod_gw_events::ingest::{BATCH_ID_HEADER, INTERNAL_TOKEN_HEADER, NDJSON};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use super::{HEADER_LEN, Position, SpoolReader, read_segment};

#[derive(Debug, Clone)]
pub struct ShipperConfig {
    /// The full ingest URL, for example `http://analytics:8090/ingest/usage`.
    pub url: String,
    pub token: String,
    pub instance_id: String,
    pub batch_records: usize,
    pub batch_bytes: usize,
}

/// A batch that has been read and may have been sent. It is stored before sending, so that a
/// retry after a restart sends exactly the same records under the same batch id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Pending {
    segment: u64,
    start: u64,
    end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Checkpoint {
    /// Everything before this position has been confirmed by the analytics service.
    position: Position,
    pending: Option<Pending>,
}

pub struct Shipper {
    cfg: ShipperConfig,
    reader: SpoolReader,
    http: reqwest::Client,
    checkpoint: Checkpoint,
    path: PathBuf,
}

enum Outcome {
    Delivered,
    /// The service refused the batch as malformed; sending it again cannot succeed.
    Rejected(String),
    /// Try again later.
    Failed(String),
}

impl Shipper {
    pub fn new(cfg: ShipperConfig, reader: SpoolReader) -> std::io::Result<Self> {
        let path = reader.dir.join("checkpoint.json");
        let first = reader.segments()?.first().copied().unwrap_or(reader.durable.borrow().segment);
        let checkpoint = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Checkpoint>(&raw).ok())
            .unwrap_or(Checkpoint { position: Position { segment: first, offset: HEADER_LEN }, pending: None });
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default();
        let s = Self { cfg, reader, http, checkpoint, path };
        // Segments before the checkpoint were shipped completely before the last shutdown.
        for seg in s.reader.segments()? {
            if seg < s.checkpoint.position.segment {
                s.reader.remove_segment(seg);
            }
        }
        Ok(s)
    }

    fn save(&self) {
        let tmp = self.path.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            std::fs::write(&tmp, serde_json::to_vec(&self.checkpoint).expect("checkpoint serialises"))?;
            std::fs::rename(&tmp, &self.path)
        };
        if let Err(e) = write() {
            tracing::error!(error = %e, "could not store the spool checkpoint");
        }
    }

    fn batch_id(&self, p: &Pending) -> String {
        format!("{}-{}-{}-{}", self.reader.spool_id, p.segment, p.start, p.end)
    }

    /// Chooses the next batch. `None` when there is nothing to ship right now.
    fn next_batch(&mut self) -> std::io::Result<Option<(Pending, Vec<Vec<u8>>)>> {
        if let Some(p) = self.checkpoint.pending.clone() {
            let batch = read_segment(&self.reader.dir, p.segment, p.start, p.end, usize::MAX, usize::MAX)?;
            return Ok(Some((p, batch.records)));
        }
        loop {
            let durable = *self.reader.durable.borrow();
            let cp = self.checkpoint.position;
            if cp.segment > durable.segment {
                return Ok(None);
            }
            let limit = if cp.segment == durable.segment { durable.offset } else { u64::MAX };
            let batch = match read_segment(
                &self.reader.dir,
                cp.segment,
                cp.offset,
                limit,
                self.cfg.batch_records,
                self.cfg.batch_bytes,
            ) {
                Ok(b) => b,
                // The segment is gone (removed by hand, for example): skip it.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && cp.segment < durable.segment => {
                    self.checkpoint.position = Position { segment: cp.segment + 1, offset: HEADER_LEN };
                    self.save();
                    continue;
                }
                Err(e) => return Err(e),
            };
            if !batch.records.is_empty() {
                let p = Pending { segment: cp.segment, start: cp.offset, end: batch.end };
                self.checkpoint.pending = Some(p.clone());
                self.save();
                return Ok(Some((p, batch.records)));
            }
            if cp.segment < durable.segment {
                // A closed segment with nothing left: it is fully shipped.
                self.reader.remove_segment(cp.segment);
                self.checkpoint.position = Position { segment: cp.segment + 1, offset: HEADER_LEN };
                self.save();
                continue;
            }
            return Ok(None);
        }
    }

    async fn send(&self, p: &Pending, records: &[Vec<u8>]) -> Outcome {
        let mut body = Vec::with_capacity(records.iter().map(|r| r.len() + 1).sum());
        for r in records {
            body.extend_from_slice(r);
            body.push(b'\n');
        }
        let resp = self
            .http
            .post(&self.cfg.url)
            .header(reqwest::header::CONTENT_TYPE, NDJSON)
            .header(BATCH_ID_HEADER, self.batch_id(p))
            .header(INTERNAL_TOKEN_HEADER, &self.cfg.token)
            .body(body)
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => Outcome::Delivered,
            Ok(r) if r.status() == reqwest::StatusCode::BAD_REQUEST => {
                Outcome::Rejected(r.text().await.unwrap_or_default())
            }
            Ok(r) => Outcome::Failed(format!("status {}", r.status())),
            Err(e) => Outcome::Failed(e.to_string()),
        }
    }

    fn advance(&mut self, p: &Pending) {
        self.checkpoint.position = Position { segment: p.segment, offset: p.end };
        self.checkpoint.pending = None;
        self.save();
    }

    /// A batch the service called malformed is set aside for inspection, so that one bad
    /// batch cannot block everything behind it.
    fn set_aside(&self, p: &Pending, records: &[Vec<u8>], reason: &str) {
        let path = self.reader.dir.join(format!("rejected-{}.ndjson", self.batch_id(p)));
        let mut body = Vec::new();
        for r in records {
            body.extend_from_slice(r);
            body.push(b'\n');
        }
        match std::fs::write(&path, body) {
            Ok(()) => tracing::error!(file = %path.display(), reason, records = records.len(),
                "the analytics service rejected a batch; it was set aside and shipping continues"),
            Err(e) => tracing::error!(error = %e, reason, "a rejected batch could not be set aside and is lost"),
        }
    }

    /// Ships one batch if there is one. Returns whether anything was delivered.
    pub async fn ship_once(&mut self) -> Result<bool, String> {
        let Some((p, records)) = self.next_batch().map_err(|e| e.to_string())? else { return Ok(false) };
        if records.is_empty() {
            // The pending range holds nothing readable any more.
            self.advance(&p);
            return Ok(false);
        }
        match self.send(&p, &records).await {
            Outcome::Delivered => {
                self.advance(&p);
                Ok(true)
            }
            Outcome::Rejected(reason) => {
                self.set_aside(&p, &records, &reason);
                self.advance(&p);
                Ok(true)
            }
            Outcome::Failed(e) => Err(e),
        }
    }

    /// Runs until `shutdown` fires, then tries for a few seconds to ship what is left.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        let mut backoff = Duration::from_millis(500);
        let mut durable = self.reader.durable.clone();
        loop {
            match self.ship_once().await {
                Ok(true) => {
                    backoff = Duration::from_millis(500);
                    continue;
                }
                Ok(false) => {
                    tokio::select! {
                        _ = durable.changed() => {}
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                        _ = shutdown.changed() => break,
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, url = %self.cfg.url, retry_in = ?backoff, "shipping events failed");
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = shutdown.changed() => break,
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match self.ship_once().await {
                Ok(true) => continue,
                _ => break,
            }
        }
    }
}
