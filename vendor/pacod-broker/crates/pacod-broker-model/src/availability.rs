//! Availability over time (docs/concept.md §2.4).
//!
//! A resource says when it can be used and how much of it:
//! - `valid_from` / `valid_to` bound the listing itself, so capacity that comes next week can be
//!   announced today;
//! - schedule **rules** are opening hours (weekdays, times of day, in a zone), each available or
//!   unavailable, with a capacity;
//! - **maintenance** windows are planned outages;
//! - **slots** are absolute spans of bookable capacity (GPU hours, say), available, reserved or
//!   tentative;
//! - the **live** status and the capacity in use come with the provider's heartbeats.
//!
//! The capacity at an instant: zero outside the listing or inside maintenance; a slot covering
//! the instant decides when there is one; otherwise the rules (when there are rules, a time no
//! rule opens is closed); otherwise the declared maximum. The live status and use apply near the
//! present only (`LIVE_HORIZON`): a provider draining now says nothing about next Tuesday.

use jiff::{SignedDuration, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::is_slug;
use crate::limits;
use crate::schedule::{Clock, Recurring, Span, Zone};
use crate::validate::{self, Problems};

/// How far ahead of now the live status and use count.
pub const LIVE_HORIZON: SignedDuration = SignedDuration::from_mins(15);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Availability {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<Timestamp>,
    /// The status when announced. Heartbeats change it without a new announcement.
    #[serde(default)]
    pub status: LiveStatus,
    /// Regions the resource is served from (slugs: `eu-central`, `bg`, `us-east-1`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<Capacity>,
    /// Declared latency. What callers observe goes into reputation, not here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency: Option<Latency>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiveStatus {
    #[default]
    Active,
    /// Usable, but slower or less reliable than usual.
    Degraded,
    /// Finishing what runs, taking nothing new.
    Draining,
    Maintenance,
}

impl LiveStatus {
    pub fn usable(self) -> bool {
        matches!(self, Self::Active | Self::Degraded)
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Degraded,
            2 => Self::Draining,
            3 => Self::Maintenance,
            _ => Self::Active,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capacity {
    /// Concurrent requests, sessions or units the resource can take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    /// What one unit is: `requests`, `sessions`, `gpus`. Requests by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Latency {
    /// Time to the first token or byte, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<f64>,
    /// Milliseconds per output token after the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms_per_token: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    #[serde(default)]
    pub timezone: Zone,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maintenance: Vec<Maintenance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<Slot>,
}

/// Opening hours: on these weekdays, between these times, the resource is available (with an
/// optional capacity) or not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(flatten)]
    pub when: Recurring,
    #[serde(default)]
    pub state: RuleState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleState {
    #[default]
    Available,
    Unavailable,
}

/// A planned outage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Maintenance {
    pub from: Timestamp,
    pub to: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When the broker first saw this window. Set by the broker; reputation counts maintenance
    /// announced well ahead as planned, not as an outage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announced_at: Option<Timestamp>,
}

/// Bookable capacity for an absolute span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub from: Timestamp,
    pub to: Timestamp,
    pub capacity: u32,
    #[serde(default)]
    pub state: SlotState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SlotState {
    #[default]
    Available,
    Reserved,
    /// Probably available; the provider has not confirmed it.
    Tentative,
}

/// What heartbeats say now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Live {
    pub now: Timestamp,
    pub status: LiveStatus,
    pub in_use: Option<u32>,
}

/// Availability over a span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AvailabilitySummary {
    pub from: Timestamp,
    pub to: Timestamp,
    /// The share of the span with at least the capacity asked (any capacity when none was asked).
    pub available_share: f64,
    /// The lowest capacity over the span; absent when the resource declares none.
    pub min_capacity: Option<u32>,
    /// The spans in which the resource is available, merged, with their lowest capacity.
    pub windows: Vec<AvailableWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AvailableWindow {
    pub from: Timestamp,
    pub to: Timestamp,
    pub capacity: Option<u32>,
}

/// The capacity at an instant: `Closed`, or `Open` with a number, or `Open` without one when the
/// resource declares no capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    Closed,
    Open(Option<u32>),
}

impl At {
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open(c) if c != Some(0))
    }

    pub fn meets(self, min: Option<u32>) -> bool {
        match (self, min) {
            (Self::Closed, _) | (Self::Open(Some(0)), _) => false,
            (Self::Open(_), None) => true,
            (Self::Open(None), Some(_)) => true,
            (Self::Open(Some(c)), Some(m)) => c >= m,
        }
    }

    fn capacity(self) -> Option<u32> {
        match self {
            Self::Closed => Some(0),
            Self::Open(c) => c,
        }
    }
}

impl Availability {
    pub fn validate(&self, p: &mut Problems, path: &str) {
        if let (Some(a), Some(b)) = (self.valid_from, self.valid_to) {
            p.check(b > a, format!("{path}.valid_to"), "after valid_from");
        }
        p.check(self.regions.len() <= limits::TAGS, format!("{path}.regions"), "too many");
        for (i, r) in self.regions.iter().enumerate() {
            p.check(is_slug(r, limits::SLUG_CHARS), format!("{path}.regions[{i}]"), "a slug");
        }
        if let Some(c) = &self.capacity
            && let Some(u) = &c.unit
        {
            p.check(is_slug(u, limits::SLUG_CHARS), format!("{path}.capacity.unit"), "a slug");
        }
        if let Some(l) = &self.latency {
            for (f, v) in [("ttft_ms", l.ttft_ms), ("ms_per_token", l.ms_per_token)] {
                if let Some(v) = v {
                    p.check(v.is_finite() && v >= 0.0, format!("{path}.latency.{f}"), "a number of zero or more");
                }
            }
        }
        if let Some(s) = &self.schedule {
            let at = format!("{path}.schedule");
            for (name, n) in [("rules", s.rules.len()), ("maintenance", s.maintenance.len()), ("slots", s.slots.len())]
            {
                p.check(n <= limits::SCHEDULE_PARTS, format!("{at}.{name}"), "too many");
            }
            for (i, r) in s.rules.iter().enumerate() {
                r.when.validate(p, &format!("{at}.rules[{i}]"));
            }
            for (i, m) in s.maintenance.iter().enumerate() {
                p.check(m.to > m.from, format!("{at}.maintenance[{i}]"), "`to` after `from`");
                if let Some(r) = &m.reason {
                    validate::text_len(p, &format!("{at}.maintenance[{i}].reason"), r, limits::NOTE_CHARS);
                }
            }
            for (i, sl) in s.slots.iter().enumerate() {
                p.check(sl.to > sl.from, format!("{at}.slots[{i}]"), "`to` after `from`");
            }
        }
    }

    /// The capacity at `t`.
    pub fn at(&self, t: Timestamp, live: Option<Live>) -> At {
        self.at_clock(&Clock::new(t), live)
    }

    /// The capacity at the clock's instant.
    pub fn at_clock(&self, clock: &Clock, live: Option<Live>) -> At {
        let t = clock.t();
        if self.valid_from.is_some_and(|f| t < f) || self.valid_to.is_some_and(|e| t >= e) {
            return At::Closed;
        }
        let max = self.capacity.as_ref().and_then(|c| c.max_concurrency);
        let mut cap = max;
        if let Some(s) = &self.schedule {
            if s.maintenance.iter().any(|m| t >= m.from && t < m.to) {
                return At::Closed;
            }
            if let Some(slot) = s.slots.iter().find(|sl| t >= sl.from && t < sl.to) {
                if slot.state == SlotState::Reserved {
                    return At::Closed;
                }
                cap = Some(slot.capacity);
            } else if !s.rules.is_empty() {
                let matching = || s.rules.iter().filter(|r| r.when.contains_at(clock, &s.timezone));
                if matching().any(|r| r.state == RuleState::Unavailable) {
                    return At::Closed;
                }
                let mut open = matching().filter(|r| r.state == RuleState::Available).peekable();
                if open.peek().is_none() {
                    return At::Closed;
                }
                cap = open.filter_map(|r| r.capacity).max().or(max);
            }
        }
        if let Some(l) = live
            && t >= l.now.checked_sub(LIVE_HORIZON).unwrap_or(l.now)
            && t <= l.now.checked_add(LIVE_HORIZON).unwrap_or(l.now)
        {
            if !l.status.usable() {
                return At::Closed;
            }
            if let (Some(c), Some(u)) = (cap, l.in_use) {
                cap = Some(c.saturating_sub(u));
            }
        }
        At::Open(cap)
    }

    /// Availability over `span`, counting time with at least `min_capacity`.
    pub fn during(&self, span: Span, live: Option<Live>, min_capacity: Option<u32>) -> AvailabilitySummary {
        let mut cuts: Vec<Timestamp> = self.valid_from.into_iter().chain(self.valid_to).collect();
        if let Some(l) = live {
            cuts.extend(l.now.checked_sub(LIVE_HORIZON).ok());
            cuts.extend(l.now.checked_add(LIVE_HORIZON).ok());
        }
        if let Some(s) = &self.schedule {
            for m in &s.maintenance {
                cuts.extend([m.from, m.to]);
            }
            for sl in &s.slots {
                cuts.extend([sl.from, sl.to]);
            }
            for r in &s.rules {
                r.when.boundaries(span.from, span.to, s.timezone.tz(), &mut cuts);
            }
        }
        let total = span.seconds();
        let (mut ok_secs, mut min_cap): (f64, Option<u32>) = (0.0, None);
        let mut windows: Vec<AvailableWindow> = Vec::new();
        let mut declares = false;
        for (start, end, mid) in span.pieces(cuts) {
            let at = self.at(mid, live);
            if let Some(c) = at.capacity() {
                declares |= at != At::Closed;
                min_cap = Some(min_cap.map_or(c, |m| m.min(c)));
            } else if at == At::Closed {
                min_cap = Some(0);
            }
            if at.meets(min_capacity) {
                ok_secs += end.duration_since(start).as_secs_f64();
                let cap = at.capacity();
                let room = windows.len() < 200;
                match windows.last_mut() {
                    Some(w) if w.to == start => {
                        w.to = end;
                        w.capacity = match (w.capacity, cap) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            (a, b) => a.or(b),
                        };
                    }
                    _ if room => windows.push(AvailableWindow { from: start, to: end, capacity: cap }),
                    _ => {}
                }
            }
        }
        AvailabilitySummary {
            from: span.from,
            to: span.to,
            available_share: if total > 0.0 { ok_secs / total } else { 0.0 },
            min_capacity: if declares || min_cap == Some(0) { min_cap } else { None },
            windows,
        }
    }

    /// Maintenance windows announced less than `notice` before they began, for reputation: an
    /// outage called maintenance at the last minute is still an outage.
    pub fn short_notice(&self, notice: SignedDuration) -> impl Iterator<Item = &Maintenance> {
        self.schedule
            .iter()
            .flat_map(|s| s.maintenance.iter())
            .filter(move |m| m.announced_at.is_none_or(|a| m.from.duration_since(a) < notice))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn gpu_pool() -> Availability {
        serde_json::from_value(json!({
            "valid_from": "2026-11-01T00:00:00Z",
            "capacity": {"max_concurrency": 8, "unit": "gpus"},
            "schedule": {
                "timezone": "UTC",
                "rules": [
                    {"weekdays": ["mon","tue","wed","thu","fri"], "from_time": "08:00", "to_time": "20:00", "capacity": 4},
                    {"weekdays": ["sat","sun"], "from_time": "00:00", "to_time": "24:00"}
                ],
                "maintenance": [{"from": "2026-11-04T10:00:00Z", "to": "2026-11-04T12:00:00Z", "reason": "driver upgrade"}],
                "slots": [{"from": "2026-11-03T00:00:00Z", "to": "2026-11-03T04:00:00Z", "capacity": 6, "state": "available"},
                          {"from": "2026-11-05T09:00:00Z", "to": "2026-11-05T10:00:00Z", "capacity": 8, "state": "reserved"}]
            }
        }))
        .unwrap()
    }

    #[test]
    fn the_capacity_at_an_instant() {
        let a = gpu_pool();
        let mut p = Problems::new();
        a.validate(&mut p, "availability");
        assert!(p.is_empty(), "{p}");
        assert_eq!(a.at(ts("2026-10-30T10:00:00Z"), None), At::Closed); // before valid_from
        assert_eq!(a.at(ts("2026-11-02T10:00:00Z"), None), At::Open(Some(4))); // Monday hours
        assert_eq!(a.at(ts("2026-11-02T22:00:00Z"), None), At::Closed); // Monday night
        assert_eq!(a.at(ts("2026-11-03T02:00:00Z"), None), At::Open(Some(6))); // a slot at night
        assert_eq!(a.at(ts("2026-11-04T11:00:00Z"), None), At::Closed); // maintenance
        assert_eq!(a.at(ts("2026-11-05T09:30:00Z"), None), At::Closed); // reserved slot
        assert_eq!(a.at(ts("2026-11-07T22:00:00Z"), None), At::Open(Some(8))); // weekend, max
    }

    #[test]
    fn live_status_counts_near_now_only() {
        let a = gpu_pool();
        let now = ts("2026-11-02T10:00:00Z");
        let live = Live { now, status: LiveStatus::Draining, in_use: None };
        assert_eq!(a.at(now, Some(live)), At::Closed);
        assert_eq!(a.at(ts("2026-11-03T10:00:00Z"), Some(live)), At::Open(Some(4)));
        let busy = Live { now, status: LiveStatus::Active, in_use: Some(3) };
        assert_eq!(a.at(now, Some(busy)), At::Open(Some(1)));
    }

    #[test]
    fn availability_over_a_span() {
        let a = gpu_pool();
        // Monday 2 November, 00:00–24:00: open 08:00–20:00 with 4.
        let span = Span::new(ts("2026-11-02T00:00:00Z"), ts("2026-11-03T00:00:00Z")).unwrap();
        let s = a.during(span, None, None);
        assert!((s.available_share - 0.5).abs() < 1e-9, "{}", s.available_share);
        assert_eq!(
            s.windows,
            vec![AvailableWindow {
                from: ts("2026-11-02T08:00:00Z"),
                to: ts("2026-11-02T20:00:00Z"),
                capacity: Some(4)
            }]
        );
        assert_eq!(s.min_capacity, Some(0));
        // Asking for 5 GPUs: Monday has none.
        assert_eq!(a.during(span, None, Some(5)).available_share, 0.0);
    }

    #[test]
    fn maintenance_on_short_notice_is_found() {
        let mut a = gpu_pool();
        let m = &mut a.schedule.as_mut().unwrap().maintenance[0];
        m.announced_at = Some(ts("2026-11-04T09:00:00Z"));
        assert_eq!(a.short_notice(SignedDuration::from_hours(24)).count(), 1);
        a.schedule.as_mut().unwrap().maintenance[0].announced_at = Some(ts("2026-11-01T09:00:00Z"));
        assert_eq!(a.short_notice(SignedDuration::from_hours(24)).count(), 0);
    }
}
