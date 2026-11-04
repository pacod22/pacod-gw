//! The live state of a resource: what heartbeats change, held in atomics so that a heartbeat
//! never rebuilds the index and a query never takes a lock to read it.

use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU32, Ordering};

use jiff::Timestamp;
use pacod_broker_model::LiveStatus;
use schemars::JsonSchema;
use serde::Serialize;

const UNKNOWN: u32 = u32::MAX;

/// One resource's live cell.
#[derive(Debug)]
pub struct Live {
    /// Milliseconds since the epoch at which the lease ends; `i64::MAX` for static entries.
    expires_at_ms: AtomicI64,
    last_seen_ms: AtomicI64,
    status: AtomicU8,
    in_use: AtomicU32,
    /// Observed by the provider and sent with its heartbeat, in tenths of a millisecond.
    ttft_tenths: AtomicU32,
}

/// A copy of the live cell at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
pub struct LiveView {
    pub status: LiveStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_use: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<Timestamp>,
}

impl Live {
    /// A cell for an entry that never expires.
    pub fn permanent(status: LiveStatus) -> Self {
        Self::new(i64::MAX, status)
    }

    pub fn leased(expires_at: Timestamp, status: LiveStatus) -> Self {
        Self::new(expires_at.as_millisecond(), status)
    }

    fn new(expires_at_ms: i64, status: LiveStatus) -> Self {
        Self {
            expires_at_ms: AtomicI64::new(expires_at_ms),
            last_seen_ms: AtomicI64::new(Timestamp::now().as_millisecond()),
            status: AtomicU8::new(status.as_u8()),
            in_use: AtomicU32::new(UNKNOWN),
            ttft_tenths: AtomicU32::new(UNKNOWN),
        }
    }

    pub fn is_expired(&self, now: Timestamp) -> bool {
        self.expires_at_ms.load(Ordering::Relaxed) <= now.as_millisecond()
    }

    pub fn extend(&self, expires_at: Timestamp, now: Timestamp) {
        self.expires_at_ms.store(expires_at.as_millisecond(), Ordering::Relaxed);
        self.last_seen_ms.store(now.as_millisecond(), Ordering::Relaxed);
    }

    /// Ends the lease now: the entry disappears from queries before the next rebuild drops it.
    pub fn expire(&self) {
        self.expires_at_ms.store(i64::MIN, Ordering::Relaxed);
    }

    pub fn set_status(&self, s: LiveStatus) {
        self.status.store(s.as_u8(), Ordering::Relaxed);
    }

    pub fn set_in_use(&self, n: Option<u32>) {
        self.in_use.store(n.map_or(UNKNOWN, |v| v.min(UNKNOWN - 1)), Ordering::Relaxed);
    }

    pub fn set_ttft_ms(&self, ms: Option<f64>) {
        let v = match ms {
            Some(ms) if ms.is_finite() && ms >= 0.0 => ((ms * 10.0).round() as u64).min(UNKNOWN as u64 - 1) as u32,
            _ => UNKNOWN,
        };
        self.ttft_tenths.store(v, Ordering::Relaxed);
    }

    pub fn status(&self) -> LiveStatus {
        LiveStatus::from_u8(self.status.load(Ordering::Relaxed))
    }

    pub fn in_use(&self) -> Option<u32> {
        let v = self.in_use.load(Ordering::Relaxed);
        (v != UNKNOWN).then_some(v)
    }

    pub fn ttft_ms(&self) -> Option<f64> {
        let v = self.ttft_tenths.load(Ordering::Relaxed);
        (v != UNKNOWN).then(|| v as f64 / 10.0)
    }

    pub fn view(&self) -> LiveView {
        let exp = self.expires_at_ms.load(Ordering::Relaxed);
        let seen = self.last_seen_ms.load(Ordering::Relaxed);
        LiveView {
            status: self.status(),
            in_use: self.in_use(),
            ttft_ms: self.ttft_ms(),
            expires_at: (exp != i64::MAX).then(|| Timestamp::from_millisecond(exp).ok()).flatten(),
            last_seen: Timestamp::from_millisecond(seen).ok(),
        }
    }

    /// The live state in the form the availability model takes.
    pub fn model(&self, now: Timestamp) -> pacod_broker_model::availability::Live {
        pacod_broker_model::availability::Live { now, status: self.status(), in_use: self.in_use() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_round_trip() {
        let now = Timestamp::now();
        let l = Live::leased(now.checked_add(jiff::SignedDuration::from_secs(30)).unwrap(), LiveStatus::Active);
        assert!(!l.is_expired(now));
        l.set_in_use(Some(3));
        l.set_ttft_ms(Some(123.4));
        l.set_status(LiveStatus::Degraded);
        let v = l.view();
        assert_eq!((v.in_use, v.ttft_ms, v.status), (Some(3), Some(123.4), LiveStatus::Degraded));
        l.set_in_use(None);
        assert_eq!(l.in_use(), None);
        l.expire();
        assert!(l.is_expired(now));
        assert!(!Live::permanent(LiveStatus::Active).is_expired(now));
        assert_eq!(Live::permanent(LiveStatus::Active).view().expires_at, None);
    }
}
