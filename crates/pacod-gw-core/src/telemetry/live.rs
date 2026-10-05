//! Rolling counters kept in the process, for the live view of the management API.
//!
//! Prometheus answers "what happened over time"; this answers "what is happening in this
//! instance right now" without a round trip to another system. It covers the last minute.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::Serialize;

const SECONDS: usize = 60;
/// Delay histogram bucket upper bounds, in microseconds.
const BOUNDS_US: [u64; 16] =
    [50, 100, 200, 350, 500, 750, 1_000, 1_500, 2_000, 3_500, 5_000, 10_000, 25_000, 50_000, 250_000, u64::MAX];

#[derive(Default)]
struct Second {
    /// The unix second this slot currently holds.
    stamp: AtomicU64,
    requests: AtomicU64,
    errors: AtomicU64,
    rejected: AtomicU64,
    delay: [AtomicU64; 16],
}

impl Second {
    fn reset(&self, stamp: u64) {
        self.requests.store(0, Ordering::Relaxed);
        self.errors.store(0, Ordering::Relaxed);
        self.rejected.store(0, Ordering::Relaxed);
        for b in &self.delay {
            b.store(0, Ordering::Relaxed);
        }
        self.stamp.store(stamp, Ordering::Relaxed);
    }
}

/// How many one-minute buckets the health view covers.
const HEALTH_MINUTES: u64 = 5;

/// Calls of one provider and model in one minute.
#[derive(Debug, Clone, Copy, Default)]
struct HealthBucket {
    minute: u64,
    ok: u32,
    failed: u32,
    /// Sum of the time to the provider's first byte, for the mean.
    first_byte_ms: f64,
}

/// The health of one provider and model, from live traffic (concept section 6.4).
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
pub struct Health {
    pub provider: String,
    pub model: String,
    /// Calls in the last five minutes that reached the provider.
    pub calls: u32,
    pub failed: u32,
    /// The share of those calls that succeeded. 1 when there were none.
    pub success_ratio: f64,
    /// Mean time to the provider's first byte, in milliseconds.
    pub first_byte_ms: Option<f64>,
}

pub struct LiveStats {
    seconds: Vec<Second>,
    in_flight: AtomicUsize,
    health: parking_lot::Mutex<std::collections::HashMap<(String, String), [HealthBucket; HEALTH_MINUTES as usize]>>,
}

impl Default for LiveStats {
    fn default() -> Self {
        Self {
            seconds: (0..SECONDS).map(|_| Second::default()).collect(),
            in_flight: AtomicUsize::new(0),
            health: Default::default(),
        }
    }
}

/// What the live endpoint returns.
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
pub struct LiveSnapshot {
    /// Calls currently in progress in this instance.
    pub in_flight: usize,
    /// Over the last minute.
    pub requests_per_second: f64,
    pub errors_per_second: f64,
    pub rejected_per_second: f64,
    pub error_ratio: f64,
    /// Gateway delay over the last minute, in milliseconds. `None` when there were no calls.
    pub gateway_delay_p50_ms: Option<f64>,
    pub gateway_delay_p95_ms: Option<f64>,
    pub gateway_delay_p99_ms: Option<f64>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl LiveStats {
    fn slot(&self, now: u64) -> &Second {
        let s = &self.seconds[(now % SECONDS as u64) as usize];
        if s.stamp.load(Ordering::Relaxed) != now {
            // A slot from a minute ago: start it afresh. Two threads may both reset; the few
            // counts lost in that race do not matter for a live view.
            s.reset(now);
        }
        s
    }

    pub fn started(&self) -> usize {
        self.in_flight.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn finished(&self) -> usize {
        self.in_flight.fetch_sub(1, Ordering::Relaxed).saturating_sub(1)
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Relaxed)
    }

    pub fn record(&self, ok: bool, gateway_delay_ms: f32) {
        self.record_at(now_secs(), ok, gateway_delay_ms);
    }

    fn record_at(&self, now: u64, ok: bool, gateway_delay_ms: f32) {
        let s = self.slot(now);
        s.requests.fetch_add(1, Ordering::Relaxed);
        if !ok {
            s.errors.fetch_add(1, Ordering::Relaxed);
        }
        let us = (f64::from(gateway_delay_ms.max(0.0)) * 1000.0) as u64;
        let idx = BOUNDS_US.iter().position(|&b| us <= b).unwrap_or(BOUNDS_US.len() - 1);
        s.delay[idx].fetch_add(1, Ordering::Relaxed);
    }

    /// Records how a call that reached a provider went.
    pub fn record_health(&self, provider: &str, model: &str, ok: bool, first_byte_ms: f32) {
        self.record_health_at(now_secs() / 60, provider, model, ok, first_byte_ms);
    }

    fn record_health_at(&self, minute: u64, provider: &str, model: &str, ok: bool, first_byte_ms: f32) {
        let mut map = self.health.lock();
        if map.len() > 5_000 {
            // Models that have not been called for a while make room.
            map.retain(|_, b| b.iter().any(|x| minute.saturating_sub(x.minute) < HEALTH_MINUTES));
        }
        let buckets = map.entry((provider.to_owned(), model.to_owned())).or_default();
        let b = &mut buckets[(minute % HEALTH_MINUTES) as usize];
        if b.minute != minute {
            *b = HealthBucket { minute, ..Default::default() };
        }
        if ok {
            b.ok += 1;
            b.first_byte_ms += f64::from(first_byte_ms);
        } else {
            b.failed += 1;
        }
    }

    /// Health per provider and model over the last five minutes, worst first.
    pub fn health(&self) -> Vec<Health> {
        self.health_at(now_secs() / 60)
    }

    fn health_at(&self, minute: u64) -> Vec<Health> {
        let map = self.health.lock();
        let mut out: Vec<Health> = map
            .iter()
            .filter_map(|((provider, model), buckets)| {
                let recent =
                    buckets.iter().filter(|b| b.minute != 0 && minute.saturating_sub(b.minute) < HEALTH_MINUTES);
                let (ok, failed, sum) =
                    recent.fold((0u32, 0u32, 0.0), |a, b| (a.0 + b.ok, a.1 + b.failed, a.2 + b.first_byte_ms));
                let calls = ok + failed;
                (calls > 0).then(|| Health {
                    provider: provider.clone(),
                    model: model.clone(),
                    calls,
                    failed,
                    success_ratio: f64::from(ok) / f64::from(calls),
                    first_byte_ms: (ok > 0).then(|| (sum / f64::from(ok) * 10.0).round() / 10.0),
                })
            })
            .collect();
        out.sort_by(|a, b| {
            a.success_ratio
                .total_cmp(&b.success_ratio)
                .then_with(|| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)))
        });
        out
    }

    pub fn record_rejected(&self) {
        self.slot(now_secs()).rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> LiveSnapshot {
        self.snapshot_at(now_secs())
    }

    fn snapshot_at(&self, now: u64) -> LiveSnapshot {
        let (mut requests, mut errors, mut rejected) = (0u64, 0u64, 0u64);
        let mut hist = [0u64; 16];
        for s in &self.seconds {
            let stamp = s.stamp.load(Ordering::Relaxed);
            if stamp == 0 || now.saturating_sub(stamp) >= SECONDS as u64 {
                continue;
            }
            requests += s.requests.load(Ordering::Relaxed);
            errors += s.errors.load(Ordering::Relaxed);
            rejected += s.rejected.load(Ordering::Relaxed);
            for (h, b) in hist.iter_mut().zip(&s.delay) {
                *h += b.load(Ordering::Relaxed);
            }
        }
        let total: u64 = hist.iter().sum();
        let quantile = |q: f64| -> Option<f64> {
            if total == 0 {
                return None;
            }
            let rank = (q * total as f64).ceil().max(1.0) as u64;
            let mut seen = 0;
            for (i, n) in hist.iter().enumerate() {
                seen += n;
                if seen >= rank {
                    // Report the bucket's upper bound; the last bucket has none, so use the
                    // one before it.
                    let bound = if BOUNDS_US[i] == u64::MAX { BOUNDS_US[i - 1] } else { BOUNDS_US[i] };
                    return Some(bound as f64 / 1000.0);
                }
            }
            None
        };
        LiveSnapshot {
            in_flight: self.in_flight(),
            requests_per_second: requests as f64 / SECONDS as f64,
            errors_per_second: errors as f64 / SECONDS as f64,
            rejected_per_second: rejected as f64 / SECONDS as f64,
            error_ratio: if requests == 0 { 0.0 } else { errors as f64 / requests as f64 },
            gateway_delay_p50_ms: quantile(0.50),
            gateway_delay_p95_ms: quantile(0.95),
            gateway_delay_p99_ms: quantile(0.99),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_percentiles_over_the_last_minute() {
        let l = LiveStats::default();
        let t = 1_000_000;
        for i in 0..100 {
            // 90 fast calls, 9 slower, 1 slow.
            let delay = if i < 90 {
                0.3
            } else if i < 99 {
                1.2
            } else {
                40.0
            };
            l.record_at(t + (i % 10), i % 20 != 0, delay);
        }
        let s = l.snapshot_at(t + 10);
        assert!((s.requests_per_second - 100.0 / 60.0).abs() < 1e-9);
        assert!((s.error_ratio - 0.05).abs() < 1e-9);
        assert_eq!(s.gateway_delay_p50_ms, Some(0.35));
        assert_eq!(s.gateway_delay_p95_ms, Some(1.5));
        assert_eq!(s.gateway_delay_p99_ms, Some(1.5));
    }

    #[test]
    fn old_seconds_fall_out_of_the_window() {
        let l = LiveStats::default();
        l.record_at(500, true, 1.0);
        assert!(l.snapshot_at(520).requests_per_second > 0.0);
        let later = l.snapshot_at(500 + 60);
        assert_eq!(later.requests_per_second, 0.0);
        assert_eq!(later.gateway_delay_p50_ms, None);
        // The slot is reused a minute later without carrying the old count.
        l.record_at(560, true, 1.0);
        assert!((l.snapshot_at(561).requests_per_second - 1.0 / 60.0).abs() < 1e-9);
    }

    #[test]
    fn in_flight_counts_up_and_down() {
        let l = LiveStats::default();
        assert_eq!(l.started(), 1);
        assert_eq!(l.started(), 2);
        assert_eq!(l.finished(), 1);
        assert_eq!(l.in_flight(), 1);
    }
}
