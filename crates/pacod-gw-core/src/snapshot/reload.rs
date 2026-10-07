//! Holds the current snapshot and keeps it up to date.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, broadcast, watch};

use super::{Snapshot, build_with};
use crate::crypto::envelope::Kek;
use crate::routing::RouteRegistry;
use crate::store::{Change, Heartbeat, Store, StoreError};

/// How often an instance checks the configuration version.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// What an instance records about itself.
#[derive(Debug, Clone)]
pub struct InstanceInfo {
    pub id: String,
    pub role: String,
    pub hostname: String,
    pub started_at: DateTime<Utc>,
    /// New for every process; see `cluster::identity`.
    pub boot_id: String,
    pub home_id: Option<String>,
    /// The heartbeat is recorded every this many polls.
    pub heartbeat_every_polls: u32,
}

impl InstanceInfo {
    pub fn heartbeat(&self, applied_version: i64) -> Heartbeat<'_> {
        Heartbeat {
            id: &self.id,
            role: &self.role,
            hostname: &self.hostname,
            software_version: env!("CARGO_PKG_VERSION"),
            applied_version,
            started_at: self.started_at,
            boot_id: &self.boot_id,
            home_id: self.home_id.as_deref(),
            alive_seconds: crate::cluster::ALIVE_SECONDS,
        }
    }
}

pub struct ConfigState {
    current: ArcSwap<Snapshot>,
    store: Store,
    keks: Vec<Kek>,
    /// Serialises reloads so that an older build cannot overwrite a newer one.
    reload_lock: Mutex<()>,
    changes: broadcast::Sender<Change>,
    /// The head version last seen in the database; for the lag metric.
    seen_head: AtomicI64,
    /// What this instance learned about each model it routes to. Outlives snapshots.
    routes: Arc<RouteRegistry>,
}

impl ConfigState {
    /// Loads the configuration once and returns the holder.
    pub async fn load(store: Store, keks: Vec<Kek>) -> Result<Arc<Self>, StoreError> {
        let (version, rows) = store.load_all().await?;
        let routes = Arc::new(RouteRegistry::default());
        let snapshot = build_with(version, rows, &keks, &routes);
        for p in &snapshot.problems {
            tracing::error!(problem = %p, "configuration object could not be used");
        }
        let (changes, _) = broadcast::channel(1024);
        Ok(Arc::new(Self {
            current: ArcSwap::from_pointee(snapshot),
            store,
            keks,
            reload_lock: Mutex::new(()),
            changes,
            seen_head: AtomicI64::new(version),
            routes,
        }))
    }

    /// A holder with a fixed snapshot and no database behind it, for tests of the data plane.
    pub fn fixed(snapshot: Snapshot, store: Store) -> Arc<Self> {
        let (changes, _) = broadcast::channel(16);
        let version = snapshot.version;
        Arc::new(Self {
            current: ArcSwap::from_pointee(snapshot),
            store,
            keks: Vec::new(),
            reload_lock: Mutex::new(()),
            changes,
            seen_head: AtomicI64::new(version),
            routes: Arc::new(RouteRegistry::default()),
        })
    }

    /// The current snapshot. This is what the request path calls; it is one atomic load.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.current.load_full()
    }

    pub fn version(&self) -> i64 {
        self.current.load().version
    }

    /// How many versions this instance is behind the database, as of the last poll.
    pub fn lag(&self) -> i64 {
        (self.seen_head.load(Ordering::Relaxed) - self.version()).max(0)
    }

    /// The routing state of every model this instance routes to.
    pub fn routes(&self) -> &RouteRegistry {
        &self.routes
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Configuration changes as this instance applies them.
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.changes.subscribe()
    }

    /// Makes sure this instance has applied at least `version`. The management API calls this
    /// after committing a change and before answering.
    pub async fn ensure_at_least(&self, version: i64) -> Result<(), StoreError> {
        if self.version() >= version {
            return Ok(());
        }
        self.reload().await.map(|_| ())
    }

    /// Reloads the configuration if the database holds another version: normally a newer
    /// one, an older one after a restore. Returns the applied version.
    pub async fn reload(&self) -> Result<i64, StoreError> {
        let _guard = self.reload_lock.lock().await;
        let before = self.version();
        let (version, rows) = self.store.load_all().await?;
        self.seen_head.store(version, Ordering::Relaxed);
        if version == before {
            return Ok(before);
        }
        let snapshot = build_with(version, rows, &self.keks, &self.routes);
        for p in &snapshot.problems {
            tracing::error!(problem = %p, "configuration object could not be used");
        }
        self.current.store(Arc::new(snapshot));
        tracing::debug!(from = before, to = version, "configuration applied");

        // Tell listeners what changed. If the log has a gap (it should not), they are told to
        // refresh everything.
        match self.store.changes_between(before, version).await {
            Ok(changes) if changes.len() as i64 == version - before => {
                for c in changes {
                    let _ = self.changes.send(c);
                }
            }
            _ => {
                let _ =
                    self.changes.send(Change { version, kind: "*".into(), object_id: "*".into(), op: "reload".into() });
            }
        }
        Ok(version)
    }

    /// Polls the configuration version and records the instance heartbeat until `shutdown`
    /// fires. Runs on the control runtime.
    pub async fn run(self: Arc<Self>, info: InstanceInfo, mut shutdown: watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(POLL_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut polls = 0u32;
        // The version last written to the instance row; -1 forces a first heartbeat.
        let mut reported = -1i64;
        let mut conflict_logged: Option<tokio::time::Instant> = None;
        let mut conflict_since: Option<tokio::time::Instant> = None;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = shutdown.changed() => break,
            }
            match self.store.head_version().await {
                Ok(head) => {
                    self.seen_head.store(head, Ordering::Relaxed);
                    if head != self.version()
                        && let Err(e) = self.reload().await
                    {
                        tracing::warn!(error = %e, "configuration reload failed; serving the last snapshot");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "configuration poll failed; serving the last snapshot"),
            }
            polls += 1;
            // The heartbeat is written on a slow cadence, and at once when a new version was
            // applied, so that "applied on n of m instances" is accurate within a poll.
            let applied = self.version();
            if (polls - 1).is_multiple_of(info.heartbeat_every_polls.max(1)) || applied != reported {
                match self.store.instance_heartbeat(&info.heartbeat(applied)).await {
                    Ok(true) => {
                        reported = applied;
                        if conflict_since.take().is_some() {
                            crate::telemetry::metrics::set_instance_conflict(&info.id, false);
                        }
                    }
                    Ok(false) => {
                        // Another running process uses this id. Keep serving; the console and
                        // the alert rules show the conflict.
                        let now = tokio::time::Instant::now();
                        if conflict_logged.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(60)) {
                            tracing::error!(instance = %info.id, "another running process uses this instance id; give every instance its own PGW_INSTANCE_ID and spool directory");
                            conflict_logged = Some(now);
                        }
                        conflict_since.get_or_insert(now);
                        crate::telemetry::metrics::set_instance_conflict(&info.id, true);
                    }
                    Err(e) => tracing::debug!(error = %e, "heartbeat failed"),
                }
            }
        }
        let _ = self.store.instance_remove(&info.id, &info.boot_id).await;
    }
}
