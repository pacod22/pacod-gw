//! Housekeeping of the management instances: retiring models at their sunset, disabling
//! credentials after their rotation grace period, settling message batches that have ended,
//! and the other periodic work. One instance at a time does it, under a lease.

use std::time::Duration;

use tokio::sync::watch;

use super::AdminState;

/// The lease all management instances compete for.
const LEASE: &str = "upkeep";
/// Runs until `shutdown` fires.
pub async fn run(st: AdminState, mut shutdown: watch::Receiver<bool>) {
    let holder = st.boot.instance_id.clone();
    let every = Duration::from_secs(st.boot.upkeep_seconds.max(1));
    loop {
        tokio::select! {
            () = tokio::time::sleep(every) => {}
            _ = shutdown.changed() => return,
        }
        match st.store.job_lease(LEASE, &holder, 3 * every.as_secs() as u32).await {
            Ok(true) => once(&st).await,
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "upkeep: the lease could not be taken"),
        }
    }
}

/// One round of housekeeping.
pub async fn once(st: &AdminState) {
    if let Err(e) = super::resources::repo::retire_at_sunset(st).await {
        tracing::warn!(error = %e, "upkeep: retiring models at their sunset failed");
    }
    if let Err(e) = super::resources::provider::disable_retired_credentials(st).await {
        tracing::warn!(error = %e, "upkeep: disabling retired credentials failed");
    }
    // Batches created through the gateway: asked about once a minute (in tests sooner), and
    // accounted when they have ended.
    let min_seconds = (st.boot.upkeep_seconds * 2).min(60) as i64;
    if let Err(e) = crate::dataplane::objects::settle_batches(&st.gw, 20, min_seconds).await {
        tracing::warn!(error = %e, "upkeep: settling message batches failed");
    }
    match super::discovery::due(st).await {
        Ok(true) => {
            let trigger = if st.store.job_state(super::discovery::JOB).await.is_ok_and(|s| s.run_requested) {
                "manual"
            } else {
                "schedule"
            };
            if let Err(e) = super::discovery::run(st, trigger).await {
                tracing::warn!(error = %e, "upkeep: model discovery failed");
            }
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "upkeep: model discovery could not be scheduled"),
    }
}
