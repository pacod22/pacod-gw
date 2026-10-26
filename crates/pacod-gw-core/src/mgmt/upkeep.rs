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
        match st.store.job_lease(LEASE, &holder, lease_seconds(every)).await {
            // The round keeps the lease while it lasts, however long a discovery run takes:
            // another instance takes it over only when this one stops. A run still open when a
            // round begins is therefore one that broke off (`discovery::close_left_open`).
            Ok(true) => {
                tokio::select! {
                    () = once(&st) => {}
                    () = keep_lease(&st, &holder, every) => {}
                }
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "upkeep: the lease could not be taken"),
        }
    }
}

/// The lease outlasts three rounds' intervals, so that one late renewal does not lose it.
fn lease_seconds(every: Duration) -> u32 {
    u32::try_from(3 * every.as_secs()).unwrap_or(u32::MAX)
}

/// Renews the lease every `every` while a round runs. It never returns, so the round decides
/// when the renewing ends. Should another instance have taken the lease all the same, the
/// round still finishes: stopping it halfway would leave its work half done.
async fn keep_lease(st: &AdminState, holder: &str, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        match st.store.job_lease(LEASE, holder, lease_seconds(every)).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!("upkeep: another instance took the lease during a round");
                return std::future::pending().await;
            }
            Err(e) => tracing::warn!(error = %e, "upkeep: the lease could not be renewed"),
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
    // Batch requests kept for audit records past their time: a batch that never settled, an
    // input file never deleted.
    if let Some(objects) = st.gw.objects.as_deref()
        && let Err(e) = objects.items_expire().await
    {
        tracing::warn!(error = %e, "upkeep: removing expired batch requests failed");
    }
    if let Err(e) = super::discovery::close_left_open(st).await {
        tracing::warn!(error = %e, "upkeep: closing a discovery run left open failed");
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
