//! Several core instances, on one machine or several (runbook section 9).
//!
//! The instances share PostgreSQL and nothing else. Each one records a heartbeat and its live
//! figures there; the management API reads them, so the central console sees every instance
//! without calling into any of them, and a gateway on another machine needs only outgoing
//! connections.

pub mod identity;

use std::time::Duration;

use crate::snapshot::InstanceInfo;
use crate::store::Store;

/// An instance counts as running while its last heartbeat is younger than this.
pub const ALIVE_SECONDS: i64 = 15;
/// Instances that stopped heart-beating are listed, and then forgotten, after this long.
pub const KEEP_STOPPED_HOURS: i64 = 24;

/// The name of the machine or container: `HOSTNAME`, else `/etc/hostname`, else `gw`.
pub fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "gw".into())
}

/// Takes this instance's row in the registry before the instance starts serving. Another
/// process that still runs with the same id makes the start fail: the two would mix their
/// figures and their spools would be indistinguishable. A process that has stopped
/// heart-beating is waited out, at most `ALIVE_SECONDS`.
pub async fn claim(store: &Store, info: &InstanceInfo, applied_version: i64) -> anyhow::Result<()> {
    let mut seen: Option<chrono::DateTime<chrono::Utc>> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(ALIVE_SECONDS as u64 + 5);
    loop {
        if store.instance_heartbeat(&info.heartbeat(applied_version)).await? {
            return Ok(());
        }
        let holder = store.instance_holder(&info.id).await?;
        if let (Some((host, beat)), Some(before)) = (&holder, seen)
            && *beat > before
        {
            anyhow::bail!(
                "the instance id \"{}\" is in use by a running process on host {host}; give every instance its own PGW_INSTANCE_ID and spool directory",
                info.id
            );
        }
        if seen.is_none() {
            tracing::warn!(instance = %info.id, "another process recently used this instance id; waiting to see whether it still runs");
        }
        seen = holder.map(|(_, beat)| beat);
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "the instance id \"{}\" could not be claimed; another process keeps using it",
            info.id
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
