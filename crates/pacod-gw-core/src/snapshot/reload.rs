//! Holds the current snapshot and keeps it up to date.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, broadcast, watch};

use super::{Snapshot, build_with};
use crate::crypto::envelope::Kek;
use crate::routing::RouteRegistry;
use crate::store::{Change, Heartbeat, Row, Store, StoreError};

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
    /// None in a gateway without a database (`pacod-gw-lite`): its snapshot never changes.
    store: Option<Store>,
    keks: Vec<Kek>,
    /// The rows the current snapshot was built from. The lock also serialises reloads, so that
    /// an older build cannot overwrite a newer one.
    rows: Mutex<BTreeMap<String, Row>>,
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
        let snapshot = build_with(version, rows.clone(), &keks, &routes);
        for p in &snapshot.problems {
            tracing::error!(problem = %p, "configuration object could not be used");
        }
        let (changes, _) = broadcast::channel(1024);
        Ok(Arc::new(Self {
            current: ArcSwap::from_pointee(snapshot),
            store: Some(store),
            keks,
            rows: Mutex::new(rows.into_iter().map(|r| (r.id.clone(), r)).collect()),
            changes,
            seen_head: AtomicI64::new(version),
            routes,
        }))
    }

    /// A holder without a database: the snapshot is built once from `rows` and never changes.
    /// What could not be used is in the snapshot's `problems`; the caller decides whether that
    /// stops the start.
    pub fn from_rows(rows: Vec<Row>, keks: Vec<Kek>) -> Arc<Self> {
        let routes = Arc::new(RouteRegistry::default());
        let snapshot = build_with(1, rows.clone(), &keks, &routes);
        let (changes, _) = broadcast::channel(16);
        Arc::new(Self {
            current: ArcSwap::from_pointee(snapshot),
            store: None,
            keks,
            rows: Mutex::new(rows.into_iter().map(|r| (r.id.clone(), r)).collect()),
            changes,
            seen_head: AtomicI64::new(1),
            routes,
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

    /// The database, unless this gateway runs without one.
    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
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
    /// one, an older one after a restore. Only the rows the change log names are read; the
    /// whole configuration is read when the log cannot say what changed (a restore, or a gap).
    /// Returns the applied version.
    pub async fn reload(&self) -> Result<i64, StoreError> {
        self.reload_with(false).await
    }

    /// Reads the whole configuration again, whatever the change log says. For rows edited in
    /// the database by hand, which the log does not name.
    pub async fn reload_all(&self) -> Result<i64, StoreError> {
        self.reload_with(true).await
    }

    async fn reload_with(&self, everything: bool) -> Result<i64, StoreError> {
        let Some(store) = &self.store else { return Ok(self.version()) };
        let mut rows = self.rows.lock().await;
        let before = self.version();
        let delta = if everything { None } else { store.load_since(before).await? };
        let (version, changes) = match delta {
            Some(delta) => {
                self.seen_head.store(delta.version, Ordering::Relaxed);
                if delta.version == before {
                    return Ok(before);
                }
                for c in &delta.changes {
                    rows.remove(&c.object_id);
                }
                rows.extend(delta.rows.into_iter().map(|r| (r.id.clone(), r)));
                (delta.version, Some(delta.changes))
            }
            None => {
                let (version, all) = store.load_all().await?;
                self.seen_head.store(version, Ordering::Relaxed);
                if version == before && !everything {
                    return Ok(before);
                }
                *rows = all.into_iter().map(|r| (r.id.clone(), r)).collect();
                (version, None)
            }
        };
        let snapshot = build_with(version, rows.values().cloned().collect(), &self.keks, &self.routes);
        for p in &snapshot.problems {
            tracing::error!(problem = %p, "configuration object could not be used");
        }
        self.current.store(Arc::new(snapshot));
        tracing::debug!(from = before, to = version, incremental = changes.is_some(), "configuration applied");

        // Tell listeners what changed; after a full read they are told to refresh everything.
        match changes {
            Some(changes) => {
                for c in changes {
                    let _ = self.changes.send(c);
                }
            }
            None if version != before => {
                let _ = self.changes.send(Change::reload(version));
            }
            None => {}
        }
        Ok(version)
    }

    /// Polls the configuration version and records the instance heartbeat until `shutdown`
    /// fires. Runs on the control runtime.
    pub async fn run(self: Arc<Self>, info: InstanceInfo, mut shutdown: watch::Receiver<bool>) {
        let Some(store) = self.store.clone() else { return };
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
            match store.head_version().await {
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
                match store.instance_heartbeat(&info.heartbeat(applied)).await {
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
        let _ = store.instance_remove(&info.id, &info.boot_id).await;
    }
}
