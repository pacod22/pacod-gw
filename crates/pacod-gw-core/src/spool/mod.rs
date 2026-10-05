//! The durable event spool (concept section 14.3).
//!
//! A request task hands its event to a bounded channel and returns. A dedicated thread appends
//! events to segment files and syncs them every 100 ms. A shipper task sends synced records to
//! the analytics service in batches and advances a checkpoint only after the service has
//! confirmed the insert. Delivery is therefore at least once; the analytics store removes
//! duplicates by event id.
//!
//! Segment format: a 16-byte header (`PGWSPOOL`, format version, reserved), then records of
//! `[length u32 LE][crc32 u32 LE][payload]`. The payload is one JSON document, which is also
//! what the analytics service ingests, so batches are sent without re-encoding.

mod shipper;

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use tokio::sync::watch;

pub use shipper::{Shipper, ShipperConfig};

const MAGIC: &[u8; 8] = b"PGWSPOOL";
const FORMAT_VERSION: u32 = 1;
const HEADER_LEN: u64 = 16;
/// Records larger than this are refused: nothing legitimate is this big, and the limit stops a
/// corrupt length field from allocating memory.
const MAX_RECORD: usize = 256 << 20;

/// A position in the spool: a segment and a byte offset in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub segment: u64,
    pub offset: u64,
}

#[derive(Debug, Clone)]
pub struct SpoolConfig {
    pub dir: PathBuf,
    /// Total size of all segments on disk above which new records are dropped.
    pub max_bytes: u64,
    /// A segment is closed and a new one started at this size.
    pub segment_bytes: u64,
    pub sync_interval: Duration,
    /// When the spool is full: refuse new requests instead of only dropping their events.
    pub fail_closed: bool,
    /// Capacity of the channel between request tasks and the writer thread.
    pub channel_capacity: usize,
}

impl SpoolConfig {
    pub fn new(dir: impl Into<PathBuf>, max_bytes: u64, fail_closed: bool) -> Self {
        Self {
            dir: dir.into(),
            max_bytes,
            segment_bytes: 64 << 20,
            sync_interval: Duration::from_millis(100),
            fail_closed,
            channel_capacity: 16_384,
        }
    }
}

#[derive(Debug, Default)]
struct Shared {
    /// Bytes of all segments currently on disk.
    bytes: AtomicU64,
    /// How much of the oldest segment on disk has been delivered. Delivered segments are
    /// removed, so this and `bytes` together give what still waits.
    shipped: AtomicU64,
    dropped: AtomicU64,
    written: AtomicU64,
    full: AtomicBool,
}

/// The handle request tasks write to. Cheap to clone.
#[derive(Clone)]
pub struct Spool {
    tx: Option<SyncSender<Vec<u8>>>,
    shared: Arc<Shared>,
    fail_closed: bool,
}

/// What the shipper needs from the writer.
pub struct SpoolReader {
    pub dir: PathBuf,
    /// A random id created with the spool directory. Batch ids are built from it, so a batch
    /// id can never be reused for other content, not even by a replacement instance that
    /// starts with an empty directory under the same name.
    pub spool_id: String,
    /// The position up to which records are synced to disk.
    pub durable: watch::Receiver<Position>,
    shared: Arc<Shared>,
}

fn segment_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(format!("seg-{seq:010}.log"))
}

/// The sequence numbers of the segments in `dir`, ascending.
fn list_segments(dir: &Path) -> io::Result<Vec<u64>> {
    let mut seqs: Vec<u64> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            name.to_str()?.strip_prefix("seg-")?.strip_suffix(".log")?.parse().ok()
        })
        .collect();
    seqs.sort_unstable();
    Ok(seqs)
}

fn write_header(f: &mut File) -> io::Result<()> {
    let mut h = [0u8; HEADER_LEN as usize];
    h[..8].copy_from_slice(MAGIC);
    h[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    f.write_all(&h)
}

impl Spool {
    /// A spool that accepts and discards everything, for installations without analytics.
    pub fn disabled() -> Self {
        Self { tx: None, shared: Arc::new(Shared::default()), fail_closed: false }
    }

    /// Opens the spool directory and starts the writer thread. Existing segments are kept for
    /// the shipper; writing always starts in a new segment, so a segment whose tail was torn
    /// by a crash is never appended to.
    pub fn open(cfg: SpoolConfig) -> io::Result<(Self, SpoolReader)> {
        std::fs::create_dir_all(&cfg.dir)?;
        let id_path = cfg.dir.join("spool.id");
        let spool_id = match std::fs::read_to_string(&id_path) {
            Ok(id) if !id.trim().is_empty() => id.trim().to_owned(),
            _ => {
                let id = uuid::Uuid::new_v4().simple().to_string();
                std::fs::write(&id_path, &id)?;
                id
            }
        };
        let existing = list_segments(&cfg.dir)?;
        let mut bytes = 0;
        for seq in &existing {
            bytes += std::fs::metadata(segment_path(&cfg.dir, *seq))?.len();
        }
        let first = existing.last().map_or(1, |s| s + 1);
        let shared = Arc::new(Shared { bytes: AtomicU64::new(bytes), ..Default::default() });
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(cfg.channel_capacity);
        let (durable_tx, durable_rx) = watch::channel(Position { segment: first, offset: HEADER_LEN });

        let mut writer = Writer::start(&cfg, first, shared.clone(), durable_tx)?;
        std::thread::Builder::new().name("spool-writer".into()).spawn(move || writer.run(rx))?;

        Ok((
            Self { tx: Some(tx), shared: shared.clone(), fail_closed: cfg.fail_closed },
            SpoolReader { dir: cfg.dir, spool_id, durable: durable_rx, shared },
        ))
    }

    /// Hands a record to the writer without blocking. Returns false when it was dropped,
    /// because the spool is full or the writer is behind.
    pub fn try_write(&self, record: Vec<u8>) -> bool {
        let Some(tx) = &self.tx else { return true };
        if self.shared.full.load(Ordering::Relaxed) {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        match tx.try_send(record) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// True when new requests must be refused: the spool is full and the installation chose
    /// to fail closed.
    pub fn refuses_requests(&self) -> bool {
        self.fail_closed && self.shared.full.load(Ordering::Relaxed)
    }

    /// Whether a record written now would be kept: the spool exists and has room.
    pub fn can_take(&self) -> bool {
        self.tx.is_some() && !self.shared.full.load(Ordering::Relaxed)
    }

    /// What the spool takes on disk. Its size limit applies to this figure.
    pub fn bytes(&self) -> u64 {
        self.shared.bytes.load(Ordering::Relaxed)
    }

    /// What waits to be delivered. A segment stays on disk until all of it is delivered, so
    /// this is less than `bytes` whenever delivery keeps up.
    pub fn pending_bytes(&self) -> u64 {
        self.bytes().saturating_sub(self.shared.shipped.load(Ordering::Relaxed))
    }

    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    pub fn written(&self) -> u64 {
        self.shared.written.load(Ordering::Relaxed)
    }

    pub fn enabled(&self) -> bool {
        self.tx.is_some()
    }
}

struct Writer {
    dir: PathBuf,
    file: File,
    segment: u64,
    offset: u64,
    max_bytes: u64,
    segment_bytes: u64,
    sync_interval: Duration,
    shared: Arc<Shared>,
    durable: watch::Sender<Position>,
    dirty: bool,
}

impl Writer {
    fn start(
        cfg: &SpoolConfig,
        segment: u64,
        shared: Arc<Shared>,
        durable: watch::Sender<Position>,
    ) -> io::Result<Self> {
        let mut file = OpenOptions::new().create_new(true).append(true).open(segment_path(&cfg.dir, segment))?;
        write_header(&mut file)?;
        file.sync_data()?;
        shared.bytes.fetch_add(HEADER_LEN, Ordering::Relaxed);
        Ok(Self {
            dir: cfg.dir.clone(),
            file,
            segment,
            offset: HEADER_LEN,
            max_bytes: cfg.max_bytes,
            segment_bytes: cfg.segment_bytes,
            sync_interval: cfg.sync_interval,
            shared,
            durable,
            dirty: false,
        })
    }

    fn run(&mut self, rx: mpsc::Receiver<Vec<u8>>) {
        let mut last_sync = Instant::now();
        loop {
            let wait = self.sync_interval.saturating_sub(last_sync.elapsed());
            match rx.recv_timeout(wait) {
                Ok(record) => {
                    if let Err(e) = self.append(&record) {
                        tracing::error!(error = %e, "spool write failed; event dropped");
                        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.sync();
                    return;
                }
            }
            if last_sync.elapsed() >= self.sync_interval {
                self.sync();
                last_sync = Instant::now();
            }
        }
    }

    fn append(&mut self, record: &[u8]) -> io::Result<()> {
        if record.len() > MAX_RECORD {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "record too large for the spool"));
        }
        let framed = 8 + record.len() as u64;
        // The cap is re-evaluated on every record, so the flag clears as soon as the shipper
        // has freed space.
        if self.shared.bytes.load(Ordering::Relaxed) + framed > self.max_bytes {
            self.shared.full.store(true, Ordering::Relaxed);
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        self.shared.full.store(false, Ordering::Relaxed);
        if self.offset > HEADER_LEN && self.offset + framed > self.segment_bytes {
            self.rotate()?;
        }
        let mut head = [0u8; 8];
        head[..4].copy_from_slice(&(record.len() as u32).to_le_bytes());
        head[4..].copy_from_slice(&crc32fast::hash(record).to_le_bytes());
        self.file.write_all(&head)?;
        self.file.write_all(record)?;
        self.offset += framed;
        self.dirty = true;
        self.shared.bytes.fetch_add(framed, Ordering::Relaxed);
        self.shared.written.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.sync();
        let next = self.segment + 1;
        let mut file = OpenOptions::new().create_new(true).append(true).open(segment_path(&self.dir, next))?;
        write_header(&mut file)?;
        file.sync_data()?;
        self.shared.bytes.fetch_add(HEADER_LEN, Ordering::Relaxed);
        self.file = file;
        self.segment = next;
        self.offset = HEADER_LEN;
        let _ = self.durable.send(Position { segment: next, offset: HEADER_LEN });
        Ok(())
    }

    fn sync(&mut self) {
        // The space check also runs here, so a full flag clears without waiting for a write.
        if self.shared.bytes.load(Ordering::Relaxed) < self.max_bytes {
            self.shared.full.store(false, Ordering::Relaxed);
        }
        if !self.dirty {
            return;
        }
        match self.file.sync_data() {
            Ok(()) => {
                self.dirty = false;
                let _ = self.durable.send(Position { segment: self.segment, offset: self.offset });
            }
            Err(e) => tracing::error!(error = %e, "spool sync failed"),
        }
    }
}

/// Records read from one segment.
#[derive(Debug, Default, PartialEq)]
pub struct ReadBatch {
    pub records: Vec<Vec<u8>>,
    /// The offset just past the last record returned.
    pub end: u64,
    /// True when the segment has no more valid records after `end`: its end was reached, or
    /// its tail is torn.
    pub exhausted: bool,
}

/// Reads records of a segment from `start`, at most up to `limit_offset` (the synced position
/// for the segment being written, `u64::MAX` for closed segments), until `max_records` or
/// `max_bytes` is reached.
pub fn read_segment(
    dir: &Path,
    segment: u64,
    start: u64,
    limit_offset: u64,
    max_records: usize,
    max_bytes: usize,
) -> io::Result<ReadBatch> {
    let mut f = File::open(segment_path(dir, segment))?;
    let len = f.metadata()?.len().min(limit_offset);
    let mut header = [0u8; HEADER_LEN as usize];
    if f.read_exact(&mut header).is_err() || &header[..8] != MAGIC {
        // Not a segment this version wrote: nothing to ship from it.
        return Ok(ReadBatch { records: Vec::new(), end: start, exhausted: true });
    }
    let mut pos = start.max(HEADER_LEN);
    f.seek(SeekFrom::Start(pos))?;
    let mut out = ReadBatch { records: Vec::new(), end: pos, exhausted: false };
    let mut bytes = 0usize;
    loop {
        if pos + 8 > len {
            out.exhausted = true;
            break;
        }
        let mut head = [0u8; 8];
        f.read_exact(&mut head)?;
        let rec_len = u32::from_le_bytes(head[..4].try_into().expect("4 bytes")) as usize;
        let crc = u32::from_le_bytes(head[4..].try_into().expect("4 bytes"));
        if rec_len > MAX_RECORD || pos + 8 + rec_len as u64 > len {
            out.exhausted = true; // torn tail
            break;
        }
        let mut payload = vec![0u8; rec_len];
        f.read_exact(&mut payload)?;
        if crc32fast::hash(&payload) != crc {
            out.exhausted = true; // corrupt from here on
            break;
        }
        pos += 8 + rec_len as u64;
        bytes += rec_len;
        out.records.push(payload);
        out.end = pos;
        if out.records.len() >= max_records || bytes >= max_bytes {
            break;
        }
    }
    Ok(out)
}

impl SpoolReader {
    /// Deletes a segment that has been shipped completely and releases its space.
    pub fn remove_segment(&self, segment: u64) {
        let path = segment_path(&self.dir, segment);
        if let Ok(meta) = std::fs::metadata(&path)
            && std::fs::remove_file(&path).is_ok()
        {
            self.shared.bytes.fetch_sub(meta.len().min(self.shared.bytes.load(Ordering::Relaxed)), Ordering::Relaxed);
        }
        self.shared.shipped.store(0, Ordering::Relaxed);
    }

    /// Notes how far the oldest segment on disk has been delivered.
    pub fn delivered_up_to(&self, offset: u64) {
        self.shared.shipped.store(offset, Ordering::Relaxed);
    }

    pub fn segments(&self) -> io::Result<Vec<u64>> {
        list_segments(&self.dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pgw-spool-{name}-{}", uuid::Uuid::now_v7().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg(dir: &Path) -> SpoolConfig {
        SpoolConfig { sync_interval: Duration::from_millis(10), ..SpoolConfig::new(dir, 1 << 20, false) }
    }

    fn wait_durable(reader: &SpoolReader, at_least: Position) {
        let start = Instant::now();
        while *reader.durable.borrow() < at_least {
            assert!(start.elapsed() < Duration::from_secs(5), "writer did not sync in time");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn records_are_written_synced_and_read_back() {
        let dir = tmp("basic");
        let (spool, reader) = Spool::open(cfg(&dir)).unwrap();
        for i in 0..100 {
            assert!(spool.try_write(format!("{{\"n\":{i}}}").into_bytes()));
        }
        let expect_end = HEADER_LEN + (0..100).map(|i| 8 + format!("{{\"n\":{i}}}").len() as u64).sum::<u64>();
        wait_durable(&reader, Position { segment: 1, offset: expect_end });
        let durable = *reader.durable.borrow();
        let batch = read_segment(&dir, 1, HEADER_LEN, durable.offset, 1000, usize::MAX).unwrap();
        assert_eq!(batch.records.len(), 100);
        assert_eq!(batch.records[42], b"{\"n\":42}");
        assert_eq!(batch.end, expect_end);
        assert_eq!(spool.bytes(), expect_end);
        assert_eq!((spool.written(), spool.dropped()), (100, 0));
    }

    #[test]
    fn reading_resumes_from_an_offset_and_respects_batch_limits() {
        let dir = tmp("resume");
        let (spool, reader) = Spool::open(cfg(&dir)).unwrap();
        for i in 0..10 {
            spool.try_write(vec![b'a' + i; 10]);
        }
        wait_durable(&reader, Position { segment: 1, offset: HEADER_LEN + 10 * 18 });
        let first = read_segment(&dir, 1, HEADER_LEN, u64::MAX, 4, usize::MAX).unwrap();
        assert_eq!(first.records.len(), 4);
        assert!(!first.exhausted);
        let rest = read_segment(&dir, 1, first.end, u64::MAX, 100, usize::MAX).unwrap();
        assert_eq!(rest.records.len(), 6);
        assert_eq!(rest.records[0], vec![b'e'; 10]);
        assert!(rest.exhausted);
        // Nothing beyond the synced position is returned.
        let limited = read_segment(&dir, 1, HEADER_LEN, HEADER_LEN + 18 * 3, 100, usize::MAX).unwrap();
        assert_eq!(limited.records.len(), 3);
    }

    #[test]
    fn segments_rotate_and_restart_begins_a_new_segment() {
        let dir = tmp("rotate");
        let mut c = cfg(&dir);
        c.segment_bytes = 200;
        let (spool, reader) = Spool::open(c.clone()).unwrap();
        for _ in 0..12 {
            spool.try_write(vec![b'x'; 40]);
        }
        let start = Instant::now();
        while spool.written() < 12 {
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(40));
        let segs = reader.segments().unwrap();
        assert!(segs.len() >= 3, "{segs:?}");
        let mut total = 0;
        for s in &segs {
            total += read_segment(&dir, *s, HEADER_LEN, u64::MAX, 100, usize::MAX).unwrap().records.len();
        }
        assert_eq!(total, 12);
        drop(spool);
        drop(reader);
        std::thread::sleep(Duration::from_millis(40));

        let last = *segs.last().unwrap();
        let (_spool2, reader2) = Spool::open(c).unwrap();
        assert_eq!(reader2.durable.borrow().segment, last + 1, "a restart never appends to an old segment");
    }

    #[test]
    fn a_torn_tail_ends_the_segment_without_losing_earlier_records() {
        let dir = tmp("torn");
        let (spool, reader) = Spool::open(cfg(&dir)).unwrap();
        for _ in 0..3 {
            spool.try_write(b"0123456789".to_vec());
        }
        wait_durable(&reader, Position { segment: 1, offset: HEADER_LEN + 3 * 18 });
        drop(spool);
        // A crash in the middle of a record: a length that promises more than is there.
        let mut f = OpenOptions::new().append(true).open(segment_path(&dir, 1)).unwrap();
        f.write_all(&500u32.to_le_bytes()).unwrap();
        f.write_all(&[1, 2, 3, 4, b'p', b'a', b'r']).unwrap();
        let b = read_segment(&dir, 1, HEADER_LEN, u64::MAX, 100, usize::MAX).unwrap();
        assert_eq!(b.records.len(), 3);
        assert!(b.exhausted);

        // A flipped bit in a payload: reading stops before the corrupt record.
        let mut raw = std::fs::read(segment_path(&dir, 1)).unwrap();
        raw[(HEADER_LEN + 18 + 8) as usize] ^= 0xff;
        std::fs::write(segment_path(&dir, 1), raw).unwrap();
        let b = read_segment(&dir, 1, HEADER_LEN, u64::MAX, 100, usize::MAX).unwrap();
        assert_eq!(b.records.len(), 1);
        assert!(b.exhausted);
    }

    #[test]
    fn a_full_spool_drops_counts_and_recovers() {
        let dir = tmp("full");
        let mut c = cfg(&dir);
        // Two 100-byte records per segment; room for five records in three segments.
        c.segment_bytes = 240;
        c.max_bytes = 640;
        c.fail_closed = true;
        let (spool, reader) = Spool::open(c).unwrap();
        for _ in 0..8 {
            spool.try_write(vec![b'x'; 100]);
        }
        let start = Instant::now();
        while spool.written() + spool.dropped() < 8 {
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(spool.written(), 5);
        assert_eq!(spool.dropped(), 3);
        assert!(spool.refuses_requests(), "fail-closed installations refuse requests while full");
        assert!(!spool.try_write(vec![b'y'; 10]), "writes are refused while the flag is set");

        // The shipper frees a shipped segment; the flag clears at the next sync.
        reader.remove_segment(1);
        let start = Instant::now();
        while spool.refuses_requests() {
            assert!(start.elapsed() < Duration::from_secs(5), "the full flag did not clear");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(spool.try_write(vec![b'z'; 10]));
    }

    #[test]
    fn a_disabled_spool_accepts_everything() {
        let s = Spool::disabled();
        assert!(s.try_write(vec![1, 2, 3]));
        assert!(!s.enabled() && !s.refuses_requests());
    }
}
