//! The time building blocks shared by pricing and availability: time zones, weekdays, times of
//! day, recurring daily windows and absolute spans.
//!
//! A recurring window (`weekdays`, `from_time`–`to_time` in a zone) may cross midnight
//! (22:00–06:00); the hours after midnight then belong to the day the window started on. Windows
//! are evaluated in their zone, so they follow daylight-saving changes as people expect: an
//! off-peak window from 22:00 stays at 22:00 local time all year.
//!
//! Queries over a span (`during {from, to}`) cut the span at every instant where something
//! changes (a period starts, a window opens) and evaluate each piece once; recurring windows are
//! expanded only for the days the span covers.

use std::fmt;
use std::str::FromStr;

use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::limits;
use crate::validate::Problems;

/// An IANA time zone, by name (`Europe/Sofia`, `UTC`). Resolved once when the document is read.
#[derive(Clone)]
pub struct Zone {
    name: String,
    tz: TimeZone,
}

impl Zone {
    pub fn utc() -> Self {
        Self { name: "UTC".into(), tz: TimeZone::UTC }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tz(&self) -> &TimeZone {
        &self.tz
    }
}

impl Default for Zone {
    fn default() -> Self {
        Self::utc()
    }
}

impl fmt::Debug for Zone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

impl PartialEq for Zone {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl FromStr for Zone {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let tz = TimeZone::get(s).map_err(|_| format!("unknown time zone {s:?}"))?;
        Ok(Self { name: s.to_string(), tz })
    }
}

impl Serialize for Zone {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name)
    }
}

impl<'de> Deserialize<'de> for Zone {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Zone {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TimeZone".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type": "string", "description": "An IANA time zone name, such as Europe/Sofia or UTC.", "examples": ["UTC", "Europe/Sofia"]})
    }
}

/// A day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    /// The ISO number: 1 is Monday, 7 is Sunday.
    pub fn iso(self) -> u8 {
        self as u8 + 1
    }

    pub fn of(d: Date) -> Self {
        use jiff::civil::Weekday as W;
        match d.weekday() {
            W::Monday => Self::Mon,
            W::Tuesday => Self::Tue,
            W::Wednesday => Self::Wed,
            W::Thursday => Self::Thu,
            W::Friday => Self::Fri,
            W::Saturday => Self::Sat,
            W::Sunday => Self::Sun,
        }
    }
}

/// A time of day, `HH:MM` or `HH:MM:SS`. `24:00` means the end of the day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeOfDay {
    /// Seconds since midnight, 0 to 86 400.
    secs: u32,
}

impl TimeOfDay {
    pub const MIDNIGHT: Self = Self { secs: 0 };
    pub const END: Self = Self { secs: 86_400 };

    pub fn hm(h: u32, m: u32) -> Self {
        Self { secs: h * 3600 + m * 60 }
    }

    fn of(t: Time) -> Self {
        Self { secs: t.hour() as u32 * 3600 + t.minute() as u32 * 60 + t.second() as u32 }
    }

    /// Seconds since midnight; 86 400 is `24:00`.
    pub fn seconds(self) -> u32 {
        self.secs
    }

    /// The instant this time has on `date` in `tz`. A time that a daylight-saving change skips is
    /// moved forward, as clocks do; `24:00` is the next midnight.
    pub fn on(self, date: Date, tz: &TimeZone) -> Option<Timestamp> {
        let (date, secs) = if self.secs >= 86_400 { (date.tomorrow().ok()?, 0) } else { (date, self.secs) };
        let t = Time::new((secs / 3600) as i8, ((secs / 60) % 60) as i8, (secs % 60) as i8, 0).ok()?;
        date.to_datetime(t).to_zoned(tz.clone()).ok().map(|z| z.timestamp())
    }
}

impl FromStr for TimeOfDay {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || format!("{s:?} is not a time of day (HH:MM)");
        let parts: Vec<&str> = s.split(':').collect();
        if !(2..=3).contains(&parts.len())
            || parts.iter().any(|p| p.len() != 2 || !p.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(bad());
        }
        let n: Vec<u32> = parts.iter().map(|p| p.parse().unwrap_or(99)).collect();
        let (h, m, sec) = (n[0], n[1], n.get(2).copied().unwrap_or(0));
        if (h == 24 && m == 0 && sec == 0) || (h < 24 && m < 60 && sec < 60) {
            Ok(Self { secs: h * 3600 + m * 60 + sec })
        } else {
            Err(bad())
        }
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (h, m, s) = (self.secs / 3600, (self.secs / 60) % 60, self.secs % 60);
        if s == 0 { write!(f, "{h:02}:{m:02}") } else { write!(f, "{h:02}:{m:02}:{s:02}") }
    }
}

impl Serialize for TimeOfDay {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TimeOfDay {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for TimeOfDay {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TimeOfDay".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type": "string", "pattern": "^([01][0-9]|2[0-4]):[0-5][0-9](:[0-5][0-9])?$", "examples": ["22:00", "06:30"]})
    }
}

/// A window that recurs every day, or on the weekdays listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Recurring {
    /// The days the window starts on; empty means every day.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weekdays: Vec<Weekday>,
    pub from_time: TimeOfDay,
    pub to_time: TimeOfDay,
}

impl Recurring {
    pub fn validate(&self, p: &mut Problems, path: &str) {
        p.check(
            self.from_time != self.to_time,
            path,
            "from_time and to_time are equal; use 00:00–24:00 for a whole day",
        );
        p.check(self.from_time < TimeOfDay::END, format!("{path}.from_time"), "before 24:00");
    }

    fn crosses_midnight(&self) -> bool {
        self.to_time <= self.from_time
    }

    fn on_day(&self, d: Date) -> bool {
        self.weekdays.is_empty() || self.weekdays.contains(&Weekday::of(d))
    }

    /// Whether the window is open at `t`.
    pub fn contains(&self, t: Timestamp, tz: &TimeZone) -> bool {
        let local = t.to_zoned(tz.clone());
        self.contains_local(local.date(), TimeOfDay::of(local.time()))
    }

    /// Whether the window is open at the instant `clock` holds, in `zone`.
    pub fn contains_at(&self, clock: &Clock, zone: &Zone) -> bool {
        let (date, time) = clock.local(zone);
        self.contains_local(date, time)
    }

    fn contains_local(&self, date: Date, time: TimeOfDay) -> bool {
        if self.crosses_midnight() {
            (self.on_day(date) && time >= self.from_time)
                || (time < self.to_time && date.yesterday().is_ok_and(|y| self.on_day(y)))
        } else {
            self.on_day(date) && time >= self.from_time && time < self.to_time
        }
    }

    /// Every instant in `[from, to]` at which the window opens or closes.
    pub fn boundaries(&self, from: Timestamp, to: Timestamp, tz: &TimeZone, out: &mut Vec<Timestamp>) {
        let first = from.to_zoned(tz.clone()).date().yesterday().unwrap_or(Date::MIN);
        let last = to.to_zoned(tz.clone()).date();
        let mut d = first;
        while d <= last {
            if self.on_day(d) {
                let close_day = if self.crosses_midnight() { d.tomorrow().unwrap_or(d) } else { d };
                for t in [self.from_time.on(d, tz), self.to_time.on(close_day, tz)].into_iter().flatten() {
                    if t > from && t < to {
                        out.push(t);
                    }
                }
            }
            match d.tomorrow() {
                Ok(n) => d = n,
                Err(_) => break,
            }
        }
    }
}

/// One instant with its local date and time in each zone asked for, worked out once.
///
/// Converting an instant to a zone's local time is the costly part of evaluating windows and
/// rules. A query evaluates one instant against thousands of resources that use a handful of
/// zones, so it converts once per zone, not once per resource.
pub struct Clock {
    t: Timestamp,
    utc: (Date, TimeOfDay),
    zones: std::cell::RefCell<Vec<(String, Date, TimeOfDay)>>,
}

impl Clock {
    pub fn new(t: Timestamp) -> Self {
        let u = t.to_zoned(TimeZone::UTC);
        Self { t, utc: (u.date(), TimeOfDay::of(u.time())), zones: std::cell::RefCell::new(Vec::new()) }
    }

    pub fn t(&self) -> Timestamp {
        self.t
    }

    pub fn local(&self, zone: &Zone) -> (Date, TimeOfDay) {
        if zone.name == "UTC" {
            return self.utc;
        }
        let mut zones = self.zones.borrow_mut();
        if let Some((_, d, t)) = zones.iter().find(|(n, _, _)| *n == zone.name) {
            return (*d, *t);
        }
        let z = self.t.to_zoned(zone.tz.clone());
        let v = (z.date(), TimeOfDay::of(z.time()));
        zones.push((zone.name.clone(), v.0, v.1));
        v
    }
}

/// An absolute span of time, `from` inclusive, `to` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    pub from: Timestamp,
    pub to: Timestamp,
}

impl Span {
    pub fn new(from: Timestamp, to: Timestamp) -> Result<Self, String> {
        if to <= from {
            return Err("`to` must be after `from`".into());
        }
        if to.duration_since(from).as_secs() > limits::SPAN_DAYS * 86_400 {
            return Err(format!("a span of at most {} days", limits::SPAN_DAYS));
        }
        Ok(Self { from, to })
    }

    pub fn contains(&self, t: Timestamp) -> bool {
        t >= self.from && t < self.to
    }

    pub fn seconds(&self) -> f64 {
        self.to.duration_since(self.from).as_secs_f64()
    }

    /// The pieces of the span between consecutive change points, each with its midpoint, at
    /// which the piece is evaluated.
    pub fn pieces(&self, mut cuts: Vec<Timestamp>) -> Vec<(Timestamp, Timestamp, Timestamp)> {
        cuts.retain(|t| *t > self.from && *t < self.to);
        cuts.sort_unstable();
        cuts.dedup();
        let mut out = Vec::with_capacity(cuts.len() + 1);
        let mut start = self.from;
        for c in cuts.into_iter().chain([self.to]) {
            let half = c.duration_since(start) / 2;
            let mid = start.checked_add(half).unwrap_or(start);
            out.push((start, c, mid));
            start = c;
        }
        out
    }
}

/// Adds `days` days to an instant, for tests and defaults.
pub fn days_after(t: Timestamp, days: i64) -> Timestamp {
    t.checked_add(days.hours() * 24).unwrap_or(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn sofia() -> TimeZone {
        TimeZone::get("Europe/Sofia").unwrap()
    }

    #[test]
    fn times_of_day_parse() {
        assert_eq!("22:00".parse::<TimeOfDay>().unwrap(), TimeOfDay::hm(22, 0));
        assert_eq!("24:00".parse::<TimeOfDay>().unwrap(), TimeOfDay::END);
        assert!("24:01".parse::<TimeOfDay>().is_err());
        assert!("7:00".parse::<TimeOfDay>().is_err());
        assert_eq!(TimeOfDay::hm(6, 30).to_string(), "06:30");
    }

    #[test]
    fn a_window_across_midnight_belongs_to_its_start_day() {
        // Friday 22:00 to Saturday 06:00, Sofia time; Sofia is UTC+2 in winter.
        let w =
            Recurring { weekdays: vec![Weekday::Fri], from_time: TimeOfDay::hm(22, 0), to_time: TimeOfDay::hm(6, 0) };
        let tz = sofia();
        assert!(w.contains(ts("2026-11-06T21:00:00Z"), &tz)); // Fri 23:00 local
        assert!(w.contains(ts("2026-11-07T03:00:00Z"), &tz)); // Sat 05:00 local
        assert!(!w.contains(ts("2026-11-07T04:00:00Z"), &tz)); // Sat 06:00 local, closed
        assert!(!w.contains(ts("2026-11-07T21:00:00Z"), &tz)); // Sat 23:00 local, not a Friday start
        assert!(!w.contains(ts("2026-11-05T21:00:00Z"), &tz)); // Thu 23:00 local
    }

    #[test]
    fn windows_follow_daylight_saving_time() {
        // 22:00 local is 19:00Z in summer (UTC+3) and 20:00Z in winter (UTC+2).
        let w = Recurring { weekdays: vec![], from_time: TimeOfDay::hm(22, 0), to_time: TimeOfDay::END };
        let tz = sofia();
        assert!(w.contains(ts("2026-07-01T19:30:00Z"), &tz));
        assert!(!w.contains(ts("2026-12-01T19:30:00Z"), &tz));
        assert!(w.contains(ts("2026-12-01T20:30:00Z"), &tz));
    }

    #[test]
    fn boundaries_cover_the_span() {
        let w = Recurring { weekdays: vec![], from_time: TimeOfDay::hm(22, 0), to_time: TimeOfDay::hm(6, 0) };
        let mut out = vec![];
        w.boundaries(ts("2026-11-02T00:00:00Z"), ts("2026-11-04T00:00:00Z"), &TimeZone::UTC, &mut out);
        out.sort();
        let want: Vec<Timestamp> =
            ["2026-11-02T06:00:00Z", "2026-11-02T22:00:00Z", "2026-11-03T06:00:00Z", "2026-11-03T22:00:00Z"]
                .iter()
                .map(|s| ts(s))
                .collect();
        assert_eq!(out, want);
    }

    #[test]
    fn spans_are_bounded() {
        assert!(Span::new(ts("2026-11-02T00:00:00Z"), ts("2026-11-01T00:00:00Z")).is_err());
        assert!(Span::new(ts("2026-01-01T00:00:00Z"), ts("2026-06-01T00:00:00Z")).is_err());
        let s = Span::new(ts("2026-11-02T00:00:00Z"), ts("2026-11-02T10:00:00Z")).unwrap();
        let p = s.pieces(vec![ts("2026-11-02T04:00:00Z"), ts("2026-11-02T04:00:00Z"), ts("2026-11-03T00:00:00Z")]);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].2, ts("2026-11-02T02:00:00Z"));
    }
}
