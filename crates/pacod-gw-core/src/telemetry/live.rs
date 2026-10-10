//! Rolling counters kept in the process, for the live view of the management API.
//!
//! Prometheus answers "what happened over time"; this answers "what is happening in this
//! instance right now" without a round trip to another system. It covers the last minute.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const SECONDS: usize = 60;
/// Delay histogram bucket upper bounds, in microseconds.
pub const BOUNDS_US: [u64; 16] =
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
    /// One slot per minute of the alert window, indexed by the minute modulo its length.
    window: parking_lot::Mutex<Vec<WindowMinute>>,
}

impl Default for LiveStats {
    fn default() -> Self {
        Self {
            seconds: (0..SECONDS).map(|_| Second::default()).collect(),
            in_flight: AtomicUsize::new(0),
            health: Default::default(),
            window: parking_lot::Mutex::new((0..WINDOW_MINUTES).map(|_| WindowMinute::default()).collect()),
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

/// The raw counts behind a [`LiveSnapshot`]: these add up across instances, percentiles do not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficCounts {
    /// Over the last minute.
    pub requests: u64,
    pub errors: u64,
    pub rejected: u64,
    /// Calls per gateway-delay bucket; see [`BOUNDS_US`].
    pub delay: [u64; 16],
}

impl TrafficCounts {
    pub fn add(&mut self, other: &TrafficCounts) {
        self.requests += other.requests;
        self.errors += other.errors;
        self.rejected += other.rejected;
        for (a, b) in self.delay.iter_mut().zip(other.delay) {
            *a += b;
        }
    }
}

/// The calls to one provider and model behind a [`Health`] entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HealthCounts {
    pub provider: String,
    pub model: String,
    pub ok: u32,
    pub failed: u32,
    /// Sum of the time to the first byte of the successful calls.
    pub first_byte_ms_sum: f64,
}

impl Health {
    /// Health from counts, which may come from several instances.
    pub fn from_counts(c: &HealthCounts) -> Option<Self> {
        let calls = c.ok + c.failed;
        (calls > 0).then(|| Health {
            provider: c.provider.clone(),
            model: c.model.clone(),
            calls,
            failed: c.failed,
            success_ratio: f64::from(c.ok) / f64::from(calls),
            first_byte_ms: (c.ok > 0).then(|| (c.first_byte_ms_sum / f64::from(c.ok) * 10.0).round() / 10.0),
        })
    }

    /// Worst first.
    pub fn sort(list: &mut [Health]) {
        list.sort_by(|a, b| {
            a.success_ratio
                .total_cmp(&b.success_ratio)
                .then_with(|| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)))
        });
    }
}

/// A quantile of a delay histogram over [`BOUNDS_US`], in milliseconds: the upper bound of
/// the bucket it falls in. `None` for an empty histogram.
pub fn delay_quantile(delay: &[u64; 16], q: f64) -> Option<f64> {
    let total: u64 = delay.iter().sum();
    if total == 0 {
        return None;
    }
    let rank = (q * total as f64).ceil().max(1.0) as u64;
    let mut seen = 0;
    for (i, n) in delay.iter().enumerate() {
        seen += n;
        if seen >= rank {
            // Report the bucket's upper bound; the last bucket has none, so use the one before it.
            let bound = if BOUNDS_US[i] == u64::MAX { BOUNDS_US[i - 1] } else { BOUNDS_US[i] };
            return Some(bound as f64 / 1000.0);
        }
    }
    None
}

fn delay_bucket(gateway_delay_ms: f32) -> usize {
    let us = (f64::from(gateway_delay_ms.max(0.0)) * 1000.0) as u64;
    BOUNDS_US.iter().position(|&b| us <= b).unwrap_or(BOUNDS_US.len() - 1)
}

/// How many one-minute buckets the alert window keeps (concept section 14.9).
pub const WINDOW_MINUTES: u64 = 15;
/// Rejections and the gateway delay are judged over the last ten of them.
pub const SHORT_WINDOW_MINUTES: u64 = 10;

/// Calls of one provider and model in the alert window. Cancelled calls are left out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelWindow {
    pub provider: String,
    pub model: String,
    pub calls: u64,
    pub failed: u64,
    pub timeouts: u64,
}

/// What the alerts of concept section 14.9 are judged on, as raw counts that add up across
/// instances: calls per provider and model over the last 15 minutes, rejections per reason
/// and the gateway delay per mode over the last 10.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowCounts {
    pub models: Vec<ModelWindow>,
    pub rejected: std::collections::BTreeMap<String, u64>,
    /// Calls per gateway-delay bucket ([`BOUNDS_US`]), by mode.
    pub delay: std::collections::BTreeMap<String, [u64; 16]>,
}

impl WindowCounts {
    pub fn add(&mut self, other: &WindowCounts) {
        for m in &other.models {
            match self.models.iter_mut().find(|x| x.provider == m.provider && x.model == m.model) {
                Some(x) => {
                    x.calls += m.calls;
                    x.failed += m.failed;
                    x.timeouts += m.timeouts;
                }
                None => self.models.push(m.clone()),
            }
        }
        for (reason, n) in &other.rejected {
            *self.rejected.entry(reason.clone()).or_default() += n;
        }
        for (mode, h) in &other.delay {
            let mine = self.delay.entry(mode.clone()).or_insert([0; 16]);
            for (a, b) in mine.iter_mut().zip(h) {
                *a += b;
            }
        }
    }
}

#[derive(Default)]
struct WindowMinute {
    minute: u64,
    models: std::collections::HashMap<(String, String), (u64, u64, u64)>,
    rejected: std::collections::HashMap<&'static str, u64>,
    delay: std::collections::HashMap<String, [u64; 16]>,
}

impl LiveSnapshot {
    /// The figures from counts, which may come from several instances.
    pub fn from_counts(c: &TrafficCounts, in_flight: usize) -> Self {
        let quantile = |q: f64| delay_quantile(&c.delay, q);
        LiveSnapshot {
            in_flight,
            requests_per_second: c.requests as f64 / SECONDS as f64,
            errors_per_second: c.errors as f64 / SECONDS as f64,
            rejected_per_second: c.rejected as f64 / SECONDS as f64,
            error_ratio: if c.requests == 0 { 0.0 } else { c.errors as f64 / c.requests as f64 },
            gateway_delay_p50_ms: quantile(0.50),
            gateway_delay_p95_ms: quantile(0.95),
            gateway_delay_p99_ms: quantile(0.99),
        }
    }
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
        s.delay[delay_bucket(gateway_delay_ms)].fetch_add(1, Ordering::Relaxed);
    }

    fn window_minute(&self, minute: u64) -> parking_lot::MappedMutexGuard<'_, WindowMinute> {
        parking_lot::MutexGuard::map(self.window.lock(), |w| {
            let slot = &mut w[(minute % WINDOW_MINUTES) as usize];
            if slot.minute != minute {
                *slot = WindowMinute { minute, ..Default::default() };
            }
            slot
        })
    }

    /// Records a settled call for the alert window: its mode and delay, and how the provider
    /// and model it went to did. A cancelled call says nothing about the provider.
    pub fn record_window(&self, mode: &str, provider: &str, model: &str, status: &str, gateway_delay_ms: f32) {
        self.record_window_at(now_secs() / 60, mode, provider, model, status, gateway_delay_ms);
    }

    fn record_window_at(&self, minute: u64, mode: &str, provider: &str, model: &str, status: &str, delay_ms: f32) {
        let mut slot = self.window_minute(minute);
        if !mode.is_empty() {
            slot.delay.entry(mode.to_owned()).or_insert([0; 16])[delay_bucket(delay_ms)] += 1;
        }
        if !provider.is_empty() && status != "cancelled" && slot.models.len() < 5_000 {
            let c = slot.models.entry((provider.to_owned(), model.to_owned())).or_default();
            c.0 += 1;
            c.1 += u64::from(status != "ok");
            c.2 += u64::from(status == "timeout");
        }
    }

    /// Records a request the gateway refused, by reason, for the alert window.
    pub fn record_rejection(&self, reason: &'static str) {
        *self.window_minute(now_secs() / 60).rejected.entry(reason).or_default() += 1;
    }

    /// The alert window's counts; see [`WindowCounts`].
    pub fn window(&self) -> WindowCounts {
        self.window_at(now_secs() / 60)
    }

    fn window_at(&self, minute: u64) -> WindowCounts {
        let w = self.window.lock();
        let mut out = WindowCounts::default();
        let mut models: std::collections::BTreeMap<(String, String), (u64, u64, u64)> = Default::default();
        for slot in w.iter().filter(|s| s.minute != 0 && minute.saturating_sub(s.minute) < WINDOW_MINUTES) {
            for (k, c) in &slot.models {
                let m = models.entry(k.clone()).or_default();
                (m.0, m.1, m.2) = (m.0 + c.0, m.1 + c.1, m.2 + c.2);
            }
            if minute.saturating_sub(slot.minute) < SHORT_WINDOW_MINUTES {
                for (reason, n) in &slot.rejected {
                    *out.rejected.entry((*reason).to_owned()).or_default() += n;
                }
                for (mode, h) in &slot.delay {
                    let mine = out.delay.entry(mode.clone()).or_insert([0; 16]);
                    for (a, b) in mine.iter_mut().zip(h) {
                        *a += b;
                    }
                }
            }
        }
        out.models = models
            .into_iter()
            .map(|((provider, model), (calls, failed, timeouts))| ModelWindow {
                provider,
                model,
                calls,
                failed,
                timeouts,
            })
            .collect();
        out
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
        let mut out: Vec<Health> = self.health_counts_at(minute).iter().filter_map(Health::from_counts).collect();
        Health::sort(&mut out);
        out
    }

    /// The counts behind [`health`](Self::health).
    pub fn health_counts(&self) -> Vec<HealthCounts> {
        self.health_counts_at(now_secs() / 60)
    }

    fn health_counts_at(&self, minute: u64) -> Vec<HealthCounts> {
        let map = self.health.lock();
        map.iter()
            .map(|((provider, model), buckets)| {
                let recent =
                    buckets.iter().filter(|b| b.minute != 0 && minute.saturating_sub(b.minute) < HEALTH_MINUTES);
                let (ok, failed, sum) =
                    recent.fold((0u32, 0u32, 0.0), |a, b| (a.0 + b.ok, a.1 + b.failed, a.2 + b.first_byte_ms));
                HealthCounts { provider: provider.clone(), model: model.clone(), ok, failed, first_byte_ms_sum: sum }
            })
            .filter(|c| c.ok + c.failed > 0)
            .collect()
    }

    pub fn record_rejected(&self) {
        self.slot(now_secs()).rejected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> LiveSnapshot {
        self.snapshot_at(now_secs())
    }

    fn snapshot_at(&self, now: u64) -> LiveSnapshot {
        LiveSnapshot::from_counts(&self.traffic_counts_at(now), self.in_flight())
    }

    /// The counts behind [`snapshot`](Self::snapshot).
    pub fn traffic_counts(&self) -> TrafficCounts {
        self.traffic_counts_at(now_secs())
    }

    fn traffic_counts_at(&self, now: u64) -> TrafficCounts {
        let mut c = TrafficCounts::default();
        for s in &self.seconds {
            let stamp = s.stamp.load(Ordering::Relaxed);
            if stamp == 0 || now.saturating_sub(stamp) >= SECONDS as u64 {
                continue;
            }
            c.requests += s.requests.load(Ordering::Relaxed);
            c.errors += s.errors.load(Ordering::Relaxed);
            c.rejected += s.rejected.load(Ordering::Relaxed);
            for (h, b) in c.delay.iter_mut().zip(&s.delay) {
                *h += b.load(Ordering::Relaxed);
            }
        }
        c
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
    fn counts_from_several_instances_add_up() {
        let (a, b) = (LiveStats::default(), LiveStats::default());
        let t = 2_000_000;
        for i in 0..90 {
            a.record_at(t, true, 0.3);
            if i < 10 {
                b.record_at(t, false, 40.0);
            }
        }
        let mut sum = a.traffic_counts_at(t + 1);
        sum.add(&b.traffic_counts_at(t + 1));
        let s = LiveSnapshot::from_counts(&sum, 3);
        assert_eq!((sum.requests, sum.errors, s.in_flight), (100, 10, 3));
        assert!((s.error_ratio - 0.1).abs() < 1e-9);
        // 90 of 100 calls were fast: the merged median is fast, the 95th percentile is not.
        assert_eq!(s.gateway_delay_p50_ms, Some(0.35));
        assert_eq!(s.gateway_delay_p95_ms, Some(50.0));

        a.record_health_at(10, "p", "m", true, 100.0);
        b.record_health_at(10, "p", "m", true, 300.0);
        b.record_health_at(10, "p", "m", false, 0.0);
        let mut c = a.health_counts_at(10).pop().unwrap();
        let d = b.health_counts_at(10).pop().unwrap();
        (c.ok, c.failed, c.first_byte_ms_sum) =
            (c.ok + d.ok, c.failed + d.failed, c.first_byte_ms_sum + d.first_byte_ms_sum);
        let h = Health::from_counts(&c).unwrap();
        assert_eq!((h.calls, h.failed, h.first_byte_ms), (3, 1, Some(200.0)), "the mean is over successful calls");
        let json = serde_json::to_string(&sum).unwrap();
        assert_eq!(serde_json::from_str::<TrafficCounts>(&json).unwrap(), sum);
    }

    #[test]
    fn in_flight_counts_up_and_down() {
        let l = LiveStats::default();
        assert_eq!(l.started(), 1);
        assert_eq!(l.started(), 2);
        assert_eq!(l.finished(), 1);
        assert_eq!(l.in_flight(), 1);
    }

    #[test]
    fn the_alert_window_keeps_fifteen_minutes_of_calls_and_ten_of_the_rest() {
        let l = LiveStats::default();
        let start = 1_000;
        // Minute 0: a timeout and a rejection. Minutes 6 to 14: one good call each.
        l.record_window_at(start, "managed", "openai", "gpt", "timeout", 30.0);
        *l.window_minute(start).rejected.entry("quota_exceeded").or_default() += 3;
        for m in 6..15 {
            l.record_window_at(start + m, "proxy", "openai", "gpt", "ok", 0.4);
        }
        l.record_window_at(start + 14, "proxy", "openai", "gpt", "cancelled", 0.4);
        let w = l.window_at(start + 14);
        assert_eq!(
            w.models,
            [ModelWindow { provider: "openai".into(), model: "gpt".into(), calls: 10, failed: 1, timeouts: 1 }]
        );
        // The first minute is outside the ten-minute part.
        assert!(w.rejected.is_empty() && !w.delay.contains_key("managed"), "{w:?}");
        assert_eq!(w.delay["proxy"].iter().sum::<u64>(), 10, "the cancelled call has a delay too");
        assert_eq!(delay_quantile(&w.delay["proxy"], 0.99), Some(0.5));
        // A minute later the first one is gone altogether.
        assert_eq!(l.window_at(start + 15).models[0].calls, 9);
        let mut both = w.clone();
        both.add(&w);
        assert_eq!((both.models[0].calls, both.delay["proxy"].iter().sum::<u64>()), (20, 20));
    }
}
