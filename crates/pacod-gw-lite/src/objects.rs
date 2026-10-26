//! The record of files and batches without a database: one file of its own in
//! `PGW_OBJECTS_DIR`, so that files and batches made through the gateway survive a restart and
//! a batch is accounted when it ends, as in the full gateway.
//!
//! The record is an append-only log, `objects.jsonl`: each change writes the object's whole
//! row as one line, and the last line of an id wins. At start the log is read into memory and
//! written again with only what is still worth keeping, through a temporary file that replaces
//! it. A line cut short by a crash (the last one) is skipped. One process holds the directory
//! at a time (`objects.lock`): two gateways on one record would each keep their own copy in
//! memory and overwrite the other's lines.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use futures_util::future::BoxFuture;
use pacod_gw_core::store::StoreError;
use pacod_gw_core::store::objects::{ENDED, ObjectStore, ProviderObject};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long a deleted file stays in the record, for the log of what was done.
const KEEP_DELETED: i64 = 7;

/// One row as the log keeps it: the object with what only the record needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Row {
    id: String,
    provider_id: String,
    #[serde(default = "anthropic")]
    api: String,
    kind: String,
    organisation_id: String,
    workspace_id: String,
    key_id: String,
    credential_scope: String,
    status: String,
    object: Value,
    detail: Value,
    cost: Option<f64>,
    currency: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    settled_at: Option<DateTime<Utc>>,
    polled_at: Option<DateTime<Utc>>,
}

fn anthropic() -> String {
    "anthropic".into()
}

impl Row {
    fn of(o: &ProviderObject) -> Self {
        Row {
            id: o.id.clone(),
            provider_id: o.provider_id.clone(),
            api: o.api.clone(),
            kind: o.kind.clone(),
            organisation_id: o.organisation_id.clone(),
            workspace_id: o.workspace_id.clone(),
            key_id: o.key_id.clone(),
            credential_scope: o.credential_scope.clone(),
            status: o.status.clone(),
            object: o.object.clone(),
            detail: o.detail.clone(),
            cost: o.cost,
            currency: o.currency.clone(),
            created_at: o.created_at,
            updated_at: o.updated_at,
            settled_at: o.settled_at,
            polled_at: None,
        }
    }

    fn object(&self) -> ProviderObject {
        ProviderObject {
            id: self.id.clone(),
            provider_id: self.provider_id.clone(),
            api: self.api.clone(),
            kind: self.kind.clone(),
            organisation_id: self.organisation_id.clone(),
            workspace_id: self.workspace_id.clone(),
            key_id: self.key_id.clone(),
            credential_scope: self.credential_scope.clone(),
            status: self.status.clone(),
            object: self.object.clone(),
            detail: self.detail.clone(),
            cost: self.cost,
            currency: self.currency.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            settled_at: self.settled_at,
        }
    }

    /// Settled and deleted objects change no more.
    fn open(&self) -> bool {
        self.status != "settled" && self.status != "deleted"
    }
}

struct State {
    log: File,
    rows: BTreeMap<String, Row>,
}

/// The record of objects in a directory of its own.
pub struct FileObjects {
    state: tokio::sync::Mutex<State>,
    /// Held while the gateway runs: the lock on the directory.
    _lock: File,
}

fn io(e: impl std::fmt::Display) -> StoreError {
    StoreError::Io(format!("the record of files and batches: {e}"))
}

impl FileObjects {
    /// Opens the record in `dir`, creating it, and takes the directory's lock.
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("PGW_OBJECTS_DIR: creating {}", dir.display()))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("objects.lock"))
            .with_context(|| format!("PGW_OBJECTS_DIR: {} cannot be written", dir.display()))?;
        lock.try_lock().map_err(|_| {
            anyhow::anyhow!(
                "PGW_OBJECTS_DIR: {} is in use by another gateway; each gateway needs a directory of its own",
                dir.display()
            )
        })?;
        let path = dir.join("objects.jsonl");
        let rows = replay(&path)?;
        let log = compact(dir, &path, &rows)?;
        Ok(Self { state: tokio::sync::Mutex::new(State { log, rows }), _lock: lock })
    }
}

/// The rows of the log, the last line of each id winning. A line that does not read (the last
/// one, cut by a crash) is skipped.
fn replay(path: &Path) -> anyhow::Result<BTreeMap<String, Row>> {
    let mut rows = BTreeMap::new();
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(rows),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("reading {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Row>(&line) {
            Ok(row) => {
                rows.insert(row.id.clone(), row);
            }
            Err(e) => tracing::warn!(line = n + 1, error = %e, "a line of the record of objects was skipped"),
        }
    }
    Ok(rows)
}

/// Writes the log again with what is worth keeping, and returns it open for appending.
fn compact(dir: &Path, path: &Path, rows: &BTreeMap<String, Row>) -> anyhow::Result<File> {
    let tmp: PathBuf = dir.join("objects.jsonl.tmp");
    let cutoff = Utc::now() - Duration::days(KEEP_DELETED);
    {
        let mut out = File::create(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        for row in rows.values().filter(|r| !(r.kind == "file" && r.status == "deleted" && r.updated_at < cutoff)) {
            writeln!(out, "{}", serde_json::to_string(row)?)?;
        }
        out.sync_all()?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    // The rename itself is kept only once the directory is on disk too.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    OpenOptions::new().append(true).open(path).with_context(|| format!("opening {}", path.display()))
}

impl State {
    /// Writes a row to the log and keeps it.
    fn put(&mut self, row: Row) -> Result<(), StoreError> {
        let line = serde_json::to_string(&row).map_err(io)?;
        writeln!(self.log, "{line}").map_err(io)?;
        self.log.sync_data().map_err(io)?;
        self.rows.insert(row.id.clone(), row);
        Ok(())
    }
}

impl ObjectStore for FileObjects {
    fn object_insert<'a>(&'a self, o: &'a ProviderObject) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let mut s = self.state.lock().await;
            // A second record of the same id keeps the first owner, as in the database.
            if s.rows.contains_key(&o.id) {
                return Ok(());
            }
            s.put(Row::of(o))
        })
    }

    fn object<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<ProviderObject>, StoreError>> {
        Box::pin(async move { Ok(self.state.lock().await.rows.get(id).map(Row::object)) })
    }

    fn object_owners<'a>(&'a self, ids: &'a [String]) -> BoxFuture<'a, Result<HashMap<String, String>, StoreError>> {
        Box::pin(async move {
            let s = self.state.lock().await;
            Ok(ids
                .iter()
                .filter_map(|id| s.rows.get(id))
                .filter(|r| r.status != "deleted")
                .map(|r| (r.id.clone(), r.workspace_id.clone()))
                .collect())
        })
    }

    fn objects_of<'a>(
        &'a self,
        workspace_id: &'a str,
        api: Option<&'a str>,
        kind: &'a str,
        limit: i64,
        after_id: Option<&'a str>,
        before_id: Option<&'a str>,
        with_deleted: bool,
    ) -> BoxFuture<'a, Result<Vec<ProviderObject>, StoreError>> {
        Box::pin(async move {
            let s = self.state.lock().await;
            let at = |id: Option<&str>| id.and_then(|i| s.rows.get(i)).map(|r| (r.created_at, r.id.clone()));
            let mut rows: Vec<&Row> = s
                .rows
                .values()
                .filter(|r| r.workspace_id == workspace_id && r.kind == kind)
                .filter(|r| api.is_none_or(|a| r.api == a))
                .filter(|r| with_deleted || r.status != "deleted")
                .collect();
            // Newest first, as the database orders them.
            rows.sort_by(|a, b| (b.created_at, &b.id).cmp(&(a.created_at, &a.id)));
            let limit = limit.max(0) as usize;
            let out: Vec<ProviderObject> = match (after_id, before_id) {
                (Some(_), _) => match at(after_id) {
                    Some(mark) => rows
                        .into_iter()
                        .filter(|r| (r.created_at, r.id.clone()) < mark)
                        .take(limit)
                        .map(Row::object)
                        .collect(),
                    None => Vec::new(),
                },
                (None, Some(_)) => match at(before_id) {
                    Some(mark) => {
                        let newer: Vec<&Row> =
                            rows.into_iter().filter(|r| (r.created_at, r.id.clone()) > mark).collect();
                        // The `limit` newer ones nearest to the mark, newest first.
                        let skip = newer.len().saturating_sub(limit);
                        newer.into_iter().skip(skip).map(Row::object).collect()
                    }
                    None => Vec::new(),
                },
                (None, None) => rows.into_iter().take(limit).map(Row::object).collect(),
            };
            Ok(out)
        })
    }

    fn object_update<'a>(
        &'a self,
        id: &'a str,
        status: &'a str,
        object: &'a Value,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let mut s = self.state.lock().await;
            let Some(mut row) = s.rows.get(id).filter(|r| r.open()).cloned() else { return Ok(()) };
            row.status = status.to_owned();
            row.object = object.clone();
            row.updated_at = Utc::now();
            row.polled_at = Some(row.updated_at);
            s.put(row)
        })
    }

    fn object_deleted<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let mut s = self.state.lock().await;
            let Some(mut row) = s.rows.get(id).cloned() else { return Ok(()) };
            row.status = "deleted".into();
            row.updated_at = Utc::now();
            s.put(row)
        })
    }

    fn batches_to_poll(&self, limit: i64, min_seconds: i64) -> BoxFuture<'_, Result<Vec<ProviderObject>, StoreError>> {
        Box::pin(async move {
            let s = self.state.lock().await;
            let before = Utc::now() - Duration::seconds(min_seconds);
            let mut due: Vec<&Row> = s
                .rows
                .values()
                .filter(|r| r.kind == "batch" && r.open())
                .filter(|r| r.polled_at.is_none_or(|p| p < before) || ENDED.contains(&r.status.as_str()))
                .collect();
            // Never asked first, then those asked longest ago.
            due.sort_by_key(|r| r.polled_at);
            Ok(due.into_iter().take(limit.max(0) as usize).map(Row::object).collect())
        })
    }

    fn batch_settled<'a>(
        &'a self,
        id: &'a str,
        cost: f64,
        currency: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move {
            let mut s = self.state.lock().await;
            let Some(mut row) = s.rows.get(id).filter(|r| r.open()).cloned() else { return Ok(false) };
            let now = Utc::now();
            row.status = "settled".into();
            row.cost = Some(cost);
            row.currency = currency.to_owned();
            row.settled_at = Some(now);
            row.updated_at = now;
            s.put(row)?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("pgw-lite-objects-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn object(id: &str, kind: &str, ws: &str, minute: i64) -> ProviderObject {
        let at = DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z").unwrap().with_timezone(&Utc)
            + Duration::minutes(minute);
        ProviderObject {
            id: id.into(),
            provider_id: "prv_anthropic".into(),
            api: "anthropic".into(),
            kind: kind.into(),
            organisation_id: "org".into(),
            workspace_id: ws.into(),
            key_id: "app".into(),
            credential_scope: "organisation".into(),
            status: if kind == "batch" { "in_progress".into() } else { "active".into() },
            object: json!({ "id": id }),
            detail: json!({ "items": {} }),
            cost: None,
            currency: String::new(),
            created_at: at,
            updated_at: at,
            settled_at: None,
        }
    }

    #[tokio::test]
    async fn the_record_survives_a_restart_and_lists_as_the_database_does() {
        let d = dir();
        {
            let r = FileObjects::open(&d).unwrap();
            for (id, minute) in [("file_a", 1), ("file_b", 2), ("file_c", 3)] {
                r.object_insert(&object(id, "file", "ws", minute)).await.unwrap();
            }
            r.object_insert(&object("file_other", "file", "ws2", 4)).await.unwrap();
            r.object_insert(&object("msgbatch_1", "batch", "ws", 5)).await.unwrap();
            r.object_deleted("file_b").await.unwrap();
            // A second insert of an id keeps the first.
            let mut again = object("file_a", "file", "someone-else", 9);
            again.status = "active".into();
            r.object_insert(&again).await.unwrap();
        }
        let r = FileObjects::open(&d).unwrap();
        let ids = |v: Vec<ProviderObject>| v.into_iter().map(|o| o.id).collect::<Vec<_>>();
        assert_eq!(ids(r.objects_of("ws", None, "file", 10, None, None, false).await.unwrap()), ["file_c", "file_a"]);
        assert_eq!(
            ids(r.objects_of("ws", None, "file", 10, None, None, true).await.unwrap()),
            ["file_c", "file_b", "file_a"]
        );
        assert_eq!(ids(r.objects_of("ws", None, "file", 1, Some("file_c"), None, true).await.unwrap()), ["file_b"]);
        assert_eq!(ids(r.objects_of("ws", None, "file", 1, None, Some("file_a"), true).await.unwrap()), ["file_b"]);
        assert!(r.objects_of("ws", Some("openai"), "file", 10, None, None, false).await.unwrap().is_empty());
        assert_eq!(r.object("file_a").await.unwrap().unwrap().workspace_id, "ws");
        let owners = r.object_owners(&["file_a".into(), "file_b".into(), "file_other".into()]).await.unwrap();
        assert_eq!(owners.len(), 2, "a deleted file has no owner: {owners:?}");
    }

    #[tokio::test]
    async fn a_batch_is_polled_then_settled_once() {
        let d = dir();
        let r = FileObjects::open(&d).unwrap();
        r.object_insert(&object("msgbatch_1", "batch", "ws", 1)).await.unwrap();
        assert_eq!(r.batches_to_poll(10, 60).await.unwrap().len(), 1);
        r.object_update("msgbatch_1", "in_progress", &json!({ "id": "msgbatch_1" })).await.unwrap();
        assert!(r.batches_to_poll(10, 60).await.unwrap().is_empty(), "asked just now");
        r.object_update("msgbatch_1", "ended", &json!({ "id": "msgbatch_1" })).await.unwrap();
        assert_eq!(r.batches_to_poll(10, 60).await.unwrap().len(), 1, "ended: settled without waiting");
        assert!(r.batch_settled("msgbatch_1", 0.25, "USD").await.unwrap());
        assert!(!r.batch_settled("msgbatch_1", 0.25, "USD").await.unwrap(), "settled once");
        r.object_update("msgbatch_1", "ended", &json!({})).await.unwrap();
        drop(r);
        let r = FileObjects::open(&d).unwrap();
        let b = r.object("msgbatch_1").await.unwrap().unwrap();
        assert_eq!((b.status.as_str(), b.cost), ("settled", Some(0.25)));
    }

    #[tokio::test]
    async fn a_torn_last_line_is_skipped_and_a_second_gateway_is_refused() {
        let d = dir();
        {
            let r = FileObjects::open(&d).unwrap();
            r.object_insert(&object("file_a", "file", "ws", 1)).await.unwrap();
            let e = FileObjects::open(&d).err().expect("the directory is held");
            assert!(e.to_string().contains("in use by another gateway"), "{e}");
        }
        let mut log = OpenOptions::new().append(true).open(d.join("objects.jsonl")).unwrap();
        write!(log, "{{\"id\":\"file_b\",\"provid").unwrap();
        drop(log);
        let r = FileObjects::open(&d).unwrap();
        assert!(r.object("file_a").await.unwrap().is_some());
        assert!(r.object("file_b").await.unwrap().is_none());
        // Compacted: the torn line is gone from the file.
        let text = std::fs::read_to_string(d.join("objects.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
    }
}
