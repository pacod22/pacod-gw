//! What this instance has learned about each model it routes to: calls in flight, time to the
//! first byte, and a circuit breaker.
//!
//! The state lives outside the snapshot, so that a configuration change does not reset it,
//! and it is per instance: there is no shared store between gateway instances. The request
//! path reaches it through an `Arc` held by the snapshot, never through a lookup.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use crate::model::BreakerSettings;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

/// Consecutive failures that open the breaker, by default.
pub const FAILURES_TO_OPEN: u32 = 5;
/// How long the breaker stays open the first time, by default. Each further opening without a
/// success in between doubles it, up to `MAX_COOLDOWN_MS`.
pub const COOLDOWN_MS: u64 = 30_000;
pub const MAX_COOLDOWN_MS: u64 = 300_000;

// The installation's settings (`GlobalSettings.breaker`), set with each snapshot.
static FAILURES: AtomicU32 = AtomicU32::new(FAILURES_TO_OPEN);
static COOLDOWN: AtomicU64 = AtomicU64::new(COOLDOWN_MS);
static MAX_COOLDOWN: AtomicU64 = AtomicU64::new(MAX_COOLDOWN_MS);

/// Applies the installation's breaker settings.
pub fn configure(s: &BreakerSettings) {
    FAILURES.store(s.failures.max(1), Relaxed);
    COOLDOWN.store(u64::from(s.cooldown_seconds.max(1)) * 1000, Relaxed);
    MAX_COOLDOWN.store(u64::from(s.max_cooldown_seconds.max(s.cooldown_seconds).max(1)) * 1000, Relaxed);
}
/// Once the cool-down is over, one call at a time may try the model; another one only after
/// this long without an outcome.
const PROBE_MS: u64 = 10_000;
/// Weight of a new time-to-first-byte sample in the moving average, in 1/1024.
const EWMA_WEIGHT: u64 = 205;

/// Milliseconds since the epoch, the clock the breaker runs on.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The breaker's state, for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BreakerState {
    Closed,
    Open,
    /// The cool-down is over; a probe call decides.
    HalfOpen,
}

#[derive(Debug, Default)]
pub struct TargetState {
    in_flight: AtomicU32,
    /// Moving average of the time to the first byte, in microseconds. 0: no sample yet.
    ewma_ttfb_us: AtomicU64,
    failures: AtomicU32,
    /// When an open breaker lets a probe through. 0: closed.
    open_until_ms: AtomicU64,
    /// Openings since the last success, for the growing cool-down.
    openings: AtomicU32,
    /// When the current probe was let through.
    probe_at_ms: AtomicU64,
}

impl TargetState {
    pub fn in_flight(&self) -> u32 {
        self.in_flight.load(Relaxed)
    }

    pub fn ewma_ttfb_us(&self) -> u64 {
        self.ewma_ttfb_us.load(Relaxed)
    }

    pub fn breaker(&self, now: u64) -> BreakerState {
        match self.open_until_ms.load(Relaxed) {
            0 => BreakerState::Closed,
            t if now < t => BreakerState::Open,
            _ => BreakerState::HalfOpen,
        }
    }

    /// Whether a call may be sent now. A closed breaker always lets it through; an open one
    /// never; after the cool-down, one probe at a time.
    pub fn admits(&self, now: u64) -> bool {
        let until = self.open_until_ms.load(Relaxed);
        if until == 0 {
            return true;
        }
        if now < until {
            return false;
        }
        let probe = self.probe_at_ms.load(Relaxed);
        now >= probe.saturating_add(PROBE_MS) && self.probe_at_ms.compare_exchange(probe, now, Relaxed, Relaxed).is_ok()
    }

    /// Counts a call in flight until the guard drops.
    pub fn begin(self: &Arc<Self>) -> InFlight {
        self.in_flight.fetch_add(1, Relaxed);
        InFlight(self.clone())
    }

    /// The model answered. Closes the breaker.
    pub fn success(&self, first_byte: Duration) {
        self.failures.store(0, Relaxed);
        if self.open_until_ms.load(Relaxed) != 0 {
            self.open_until_ms.store(0, Relaxed);
            self.openings.store(0, Relaxed);
            self.probe_at_ms.store(0, Relaxed);
        }
        let sample = (first_byte.as_micros() as u64).max(1);
        let old = self.ewma_ttfb_us.load(Relaxed);
        // A lost update between two calls finishing together is harmless.
        let new = if old == 0 { sample } else { (old * (1024 - EWMA_WEIGHT) + sample * EWMA_WEIGHT) / 1024 };
        self.ewma_ttfb_us.store(new.max(1), Relaxed);
    }

    /// The model failed: an error answer, no connection or no answer in time. `retry_after` is
    /// what a `429` asked for; the breaker then opens for that long right away.
    pub fn failure(&self, now: u64, retry_after: Option<Duration>) {
        let until = self.open_until_ms.load(Relaxed);
        if let Some(wait) = retry_after {
            let wait = (wait.as_millis() as u64).min(MAX_COOLDOWN.load(Relaxed));
            let at = now + wait;
            if at > until {
                self.open_until_ms.store(at, Relaxed);
            }
            return;
        }
        let half_open = until != 0 && now >= until;
        let failures = self.failures.fetch_add(1, Relaxed) + 1;
        if half_open || (until == 0 && failures >= FAILURES.load(Relaxed)) {
            let n = self.openings.fetch_add(1, Relaxed).min(16);
            let cooldown = (COOLDOWN.load(Relaxed) << n).min(MAX_COOLDOWN.load(Relaxed));
            self.open_until_ms.store(now + cooldown, Relaxed);
            self.probe_at_ms.store(0, Relaxed);
        }
    }
}

/// A call in flight at a model.
#[derive(Debug)]
pub struct InFlight(Arc<TargetState>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Relaxed);
    }
}

/// The state of every model this instance routes to, by credential and model.
///
/// Keyed by the credential as well: a `429` describes one provider key, so an organisation
/// that exhausts its own key must not open the breaker for the others.
#[derive(Debug, Default)]
pub struct RouteRegistry {
    map: Mutex<HashMap<(String, String), Arc<TargetState>>>,
}

/// One model at one credential, for the monitoring page.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TargetView {
    /// The model's object id.
    pub model_id: String,
    /// The credential's id; `-` when the provider has none.
    pub credential_id: String,
    pub breaker: BreakerState,
    pub in_flight: u32,
    /// Moving average of the time to the first byte. Unset before the first answer.
    pub first_byte_ms: Option<f64>,
}

impl RouteRegistry {
    /// The state of a model at a credential, the same `Arc` each time.
    pub fn get(&self, credential_id: &str, model_id: &str) -> Arc<TargetState> {
        self.map.lock().entry((credential_id.to_owned(), model_id.to_owned())).or_default().clone()
    }

    /// Forgets models that no snapshot refers to any more.
    pub fn prune(&self) {
        self.map.lock().retain(|_, s| Arc::strong_count(s) > 1);
    }

    pub fn view(&self) -> Vec<TargetView> {
        let now = now_ms();
        let mut v: Vec<TargetView> = self
            .map
            .lock()
            .iter()
            .map(|((cred, model), s)| TargetView {
                model_id: model.clone(),
                credential_id: cred.clone(),
                breaker: s.breaker(now),
                in_flight: s.in_flight(),
                first_byte_ms: match s.ewma_ttfb_us() {
                    0 => None,
                    us => Some(us as f64 / 1000.0),
                },
            })
            .collect();
        v.sort_by(|a, b| (&a.model_id, &a.credential_id).cmp(&(&b.model_id, &b.credential_id)));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_breaker_opens_after_consecutive_failures() {
        let s = TargetState::default();
        for _ in 0..FAILURES_TO_OPEN - 1 {
            s.failure(1_000, None);
        }
        assert_eq!(s.breaker(1_000), BreakerState::Closed);
        s.success(Duration::from_millis(10));
        for _ in 0..FAILURES_TO_OPEN - 1 {
            s.failure(1_000, None);
        }
        assert_eq!(s.breaker(1_000), BreakerState::Closed, "a success in between resets the count");
        s.failure(1_000, None);
        assert_eq!(s.breaker(1_000), BreakerState::Open);
        assert!(!s.admits(1_000 + COOLDOWN_MS - 1));
    }

    #[test]
    fn half_open_lets_one_probe_through_and_reopens_for_longer() {
        let s = TargetState::default();
        for _ in 0..FAILURES_TO_OPEN {
            s.failure(0, None);
        }
        let t = COOLDOWN_MS;
        assert_eq!(s.breaker(t), BreakerState::HalfOpen);
        assert!(s.admits(t), "the probe");
        assert!(!s.admits(t + 1), "only one at a time");
        assert!(s.admits(t + PROBE_MS), "another one when the first never reported back");
        // The probe fails: open again, twice as long.
        s.failure(t + PROBE_MS, None);
        assert_eq!(s.breaker(t + PROBE_MS + 2 * COOLDOWN_MS - 1), BreakerState::Open);
        assert_eq!(s.breaker(t + PROBE_MS + 2 * COOLDOWN_MS), BreakerState::HalfOpen);
        // A success closes it and resets the cool-down.
        s.success(Duration::from_millis(5));
        assert_eq!(s.breaker(0), BreakerState::Closed);
        assert!(s.admits(0));
    }

    #[test]
    fn retry_after_opens_at_once_for_that_long() {
        let s = TargetState::default();
        s.failure(1_000, Some(Duration::from_secs(20)));
        assert_eq!(s.breaker(20_999), BreakerState::Open);
        assert_eq!(s.breaker(21_000), BreakerState::HalfOpen);
        s.failure(1_000, Some(Duration::from_secs(3_600)));
        assert_eq!(s.breaker(1_000 + MAX_COOLDOWN_MS), BreakerState::HalfOpen, "capped");
    }

    #[test]
    fn moving_average_and_in_flight() {
        let s = Arc::new(TargetState::default());
        s.success(Duration::from_millis(100));
        assert_eq!(s.ewma_ttfb_us(), 100_000);
        s.success(Duration::from_millis(200));
        assert!((119_000..=121_000).contains(&s.ewma_ttfb_us()));
        let a = s.begin();
        let b = s.begin();
        assert_eq!(s.in_flight(), 2);
        drop((a, b));
        assert_eq!(s.in_flight(), 0);
    }

    #[test]
    fn the_registry_hands_out_the_same_state_and_prunes() {
        let r = RouteRegistry::default();
        let a = r.get("crd_1", "mdl_1");
        assert!(Arc::ptr_eq(&a, &r.get("crd_1", "mdl_1")));
        assert!(!Arc::ptr_eq(&a, &r.get("crd_2", "mdl_1")));
        r.prune();
        assert_eq!(r.view().len(), 1, "only the one still held");
    }
}
