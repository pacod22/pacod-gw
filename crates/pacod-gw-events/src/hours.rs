//! Hours of the week for prices that depend on the clock (off-peak rates), shared by the
//! analytics service, which reads them from pricing pages, and the core, which compares and
//! stores them.
//!
//! Pages state hours in their own time zone and usually the *peak* ones ("01:00–04:00 and
//! 06:00–10:00 UTC, Monday to Friday"); a price stores the hours its variant applies in UTC
//! (`CandidateWindow`). Everything goes through one form, a week of minutes in UTC, so that the
//! same hours written differently (another order, `24:00` for midnight, a span across midnight,
//! one window per day or one for several days) come out as the same windows.

use serde::Deserialize;

use crate::jobs::{CandidateTier, CandidateWindow};

const DAY: usize = 24 * 60;
const WEEK: usize = 7 * DAY;

/// Hours as a page states them: ISO weekdays (1 is Monday), spans `HH:MM-HH:MM` (an end of
/// `24:00` or `00:00` is midnight; an end before the start runs into the next day), and the
/// page's offset from UTC (`+08:00`, `-05:00`, `Z`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct StatedHours {
    #[serde(default)]
    pub weekdays: Vec<u8>,
    #[serde(default)]
    pub hours: Vec<String>,
    #[serde(default)]
    pub utc_offset: String,
}

/// A week of minutes in UTC, Monday 00:00 first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Week(Vec<bool>);

impl Week {
    fn empty() -> Self {
        Week(vec![false; WEEK])
    }

    /// The minutes the stated hours cover, moved to UTC. `None` when a weekday, a span or the
    /// offset is malformed, or nothing is covered.
    pub fn of(stated: &StatedHours) -> Option<Week> {
        let offset = offset_minutes(&stated.utc_offset)?;
        let mut week = Week::empty();
        if stated.weekdays.is_empty() || stated.hours.is_empty() {
            return None;
        }
        for &day in &stated.weekdays {
            if !(1..=7).contains(&day) {
                return None;
            }
            for span in &stated.hours {
                let (start, end) = span_minutes(span)?;
                let first = usize::from(day - 1) * DAY;
                // An end at or before the start runs into the next day.
                let length = if end > start { end - start } else { end + DAY - start };
                for m in 0..length {
                    let local = (first + start + m) as i64;
                    let utc = (local - offset).rem_euclid(WEEK as i64) as usize;
                    week.0[utc] = true;
                }
            }
        }
        week.0.iter().any(|&m| m).then_some(week)
    }

    /// The minutes this week does not cover.
    pub fn complement(&self) -> Week {
        Week(self.0.iter().map(|m| !m).collect())
    }

    fn union(&mut self, other: &Week) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a |= *b;
        }
    }

    /// The windows of `variant` that cover exactly these minutes: one window per set of days
    /// with the same spans, days and spans in order; a whole day is `00:00-00:00`.
    pub fn windows(&self, variant: &str) -> Vec<CandidateWindow> {
        let mut out: Vec<CandidateWindow> = Vec::new();
        for day in 0..7 {
            let minutes = &self.0[day * DAY..(day + 1) * DAY];
            let mut spans = Vec::new();
            let mut start = None;
            for (m, &on) in minutes.iter().chain(std::iter::once(&false)).enumerate() {
                match (on, start) {
                    (true, None) => start = Some(m),
                    (false, Some(s)) => {
                        spans.push(format!("{}-{}", clock(s), clock(m % DAY)));
                        start = None;
                    }
                    _ => {}
                }
            }
            if spans.is_empty() {
                continue;
            }
            let weekday = day as u8 + 1;
            match out.iter_mut().find(|w| w.hours_utc == spans) {
                Some(w) => w.weekdays.push(weekday),
                None => {
                    out.push(CandidateWindow { variant: variant.to_owned(), weekdays: vec![weekday], hours_utc: spans })
                }
            }
        }
        out
    }
}

/// The off-peak windows from what a page states: the rest of the week outside its peak hours,
/// together with any off-peak hours it names (pages usually state the peak hours and "all
/// other times", and a model reading one may list only part of the rest). `None` when neither
/// gives any hours.
pub fn off_peak_windows(peak: Option<&StatedHours>, off_peak: Option<&StatedHours>) -> Option<Vec<CandidateWindow>> {
    let week = match (peak.and_then(Week::of), off_peak.and_then(Week::of)) {
        (Some(peak), Some(off)) => {
            let mut rest = peak.complement();
            rest.union(&off);
            rest
        }
        (Some(peak), None) => peak.complement(),
        (None, Some(off)) => off,
        (None, None) => return None,
    };
    let windows = week.windows("off_peak");
    (!windows.is_empty()).then_some(windows)
}

/// The same hours in one canonical form: per variant, in name order. Windows that cannot be
/// read are left out.
pub fn canonical_windows(windows: &[CandidateWindow]) -> Vec<CandidateWindow> {
    let mut variants: Vec<&str> = windows.iter().map(|w| w.variant.as_str()).collect();
    variants.sort_unstable();
    variants.dedup();
    let mut out = Vec::new();
    for variant in variants {
        let mut week = Week::empty();
        for w in windows.iter().filter(|w| w.variant == variant) {
            let stated =
                StatedHours { weekdays: w.weekdays.clone(), hours: w.hours_utc.clone(), utc_offset: "Z".into() };
            if let Some(covered) = Week::of(&stated) {
                week.union(&covered);
            }
        }
        out.extend(week.windows(variant));
    }
    out
}

/// Tiers in threshold order, without empty ones or a repeated threshold (the first stays).
pub fn canonical_tiers(tiers: &[CandidateTier]) -> Vec<CandidateTier> {
    let mut out: Vec<CandidateTier> = tiers
        .iter()
        .filter(|t| t.above_input_tokens > 0 && (!t.rates.is_empty() || t.variants.values().any(|r| !r.is_empty())))
        .cloned()
        .collect();
    out.sort_by_key(|t| t.above_input_tokens);
    out.dedup_by_key(|t| t.above_input_tokens);
    out
}

/// `HH:MM` as minutes of the day, with `24:00` as 1440. `None` when malformed.
fn clock_minutes(t: &str) -> Option<usize> {
    let (h, m) = t.trim().split_once(':')?;
    let (h, m) = (h.trim().parse::<usize>().ok()?, m.trim().parse::<usize>().ok()?);
    match (h, m) {
        (24, 0) => Some(DAY),
        (h, m) if h < 24 && m < 60 => Some(h * 60 + m),
        _ => None,
    }
}

/// A span as start and end minute; an end of midnight (`00:00` or `24:00`) is 1440, unless the
/// span starts at midnight too, which is the whole day.
fn span_minutes(span: &str) -> Option<(usize, usize)> {
    let (a, b) = span.split_once('-').or_else(|| span.split_once('–'))?;
    let (start, end) = (clock_minutes(a)?, clock_minutes(b)?);
    let start = if start == DAY { 0 } else { start };
    let end = if end == 0 { DAY } else { end };
    (start != end || start == 0).then_some((start, end))
}

/// `+08:00`, `-0530`, `UTC+8`, `Z` as minutes east of UTC. An empty offset is UTC, and one
/// without a sign (`08:00`) is east.
fn offset_minutes(text: &str) -> Option<i64> {
    let t = text.trim().trim_start_matches("UTC").trim_start_matches("GMT").trim();
    if t.is_empty() || t == "Z" {
        return Some(0);
    }
    let (sign, rest) = match t.as_bytes().first()? {
        b'+' => (1, &t[1..]),
        b'-' => (-1, &t[1..]),
        b'0'..=b'9' => (1, t),
        _ => return None,
    };
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None if rest.len() == 4 => (rest[..2].parse::<i64>().ok()?, rest[2..].parse::<i64>().ok()?),
        None => (rest.parse::<i64>().ok()?, 0),
    };
    (h <= 14 && m < 60).then_some(sign * (h * 60 + m))
}

/// Minutes of the day as `HH:MM`; 1440 and 0 are both `00:00`.
fn clock(minute: usize) -> String {
    let m = minute % DAY;
    format!("{:02}:{:02}", m / 60, m % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stated(weekdays: &[u8], hours: &[&str], offset: &str) -> StatedHours {
        StatedHours {
            weekdays: weekdays.to_vec(),
            hours: hours.iter().map(|h| (*h).to_owned()).collect(),
            utc_offset: offset.into(),
        }
    }

    fn window(weekdays: &[u8], hours: &[&str]) -> CandidateWindow {
        CandidateWindow {
            variant: "off_peak".into(),
            weekdays: weekdays.to_vec(),
            hours_utc: hours.iter().map(|h| (*h).to_owned()).collect(),
        }
    }

    #[test]
    fn deepseeks_peak_hours_give_the_feeds_windows() {
        // DeepSeek's page: peak 01:00-04:00 and 06:00-10:00 UTC, Monday to Friday.
        let peak = stated(&[1, 2, 3, 4, 5], &["01:00-04:00", "06:00-10:00"], "+00:00");
        let expected = vec![
            window(&[1, 2, 3, 4, 5], &["00:00-01:00", "04:00-06:00", "10:00-00:00"]),
            window(&[6, 7], &["00:00-00:00"]),
        ];
        assert_eq!(off_peak_windows(Some(&peak), None).unwrap(), expected);
        // The same peak in Beijing time: 09:00-12:00 and 14:00-18:00, +08:00.
        let beijing = stated(&[1, 2, 3, 4, 5], &["09:00-12:00", "14:00-18:00"], "+08:00");
        assert_eq!(off_peak_windows(Some(&beijing), None).unwrap(), expected);
        // Feeds and pages written otherwise come out the same.
        let messy = vec![
            window(&[7, 6], &["00:00-24:00"]),
            window(&[5, 4, 3, 2, 1], &["10:00-24:00", "04:00-06:00", "00:00-01:00"]),
        ];
        assert_eq!(canonical_windows(&messy), expected);
    }

    #[test]
    fn spans_across_midnight_and_week_ends_wrap() {
        // Sunday 22:00 to Monday 02:00 local, in UTC+2: Sunday 20:00 to Monday 00:00 UTC.
        let off = stated(&[7], &["22:00-02:00"], "+02:00");
        assert_eq!(off_peak_windows(None, Some(&off)).unwrap(), vec![window(&[7], &["20:00-00:00"])]);
        // Monday 00:30 at +08:00 is still Sunday in UTC.
        let early = stated(&[1], &["00:00-01:00"], "+08:00");
        assert_eq!(off_peak_windows(None, Some(&early)).unwrap(), vec![window(&[7], &["16:00-17:00"])]);
        assert_eq!(offset_minutes("UTC+8"), Some(480));
        assert_eq!(offset_minutes("-0530"), Some(-330));
        assert_eq!(offset_minutes("00:00"), Some(0));
        // Peak hours with a partial list of the rest, as a model read DeepSeek's page live.
        let peak = stated(&[1, 2, 3, 4, 5], &["01:00-04:00", "06:00-10:00"], "00:00");
        let partial = stated(&[1, 2, 3, 4, 5], &["04:00-06:00", "10:00-24:00"], "00:00");
        assert_eq!(off_peak_windows(Some(&peak), Some(&partial)), off_peak_windows(Some(&peak), None));
    }

    #[test]
    fn what_cannot_be_read_gives_nothing() {
        assert!(off_peak_windows(Some(&stated(&[8], &["01:00-02:00"], "Z")), None).is_none());
        assert!(off_peak_windows(Some(&stated(&[1], &["25:00-02:00"], "Z")), None).is_none());
        assert!(off_peak_windows(Some(&stated(&[1], &["01:00-02:00"], "Beijing")), None).is_none());
        assert!(off_peak_windows(None, None).is_none());
        // An unreadable window is left out of the canonical form.
        assert_eq!(canonical_windows(&[window(&[1], &["xx"])]), vec![]);
    }

    #[test]
    fn tiers_are_ordered_and_empty_ones_dropped() {
        let tier = |n: u64, input: Option<f64>| CandidateTier {
            above_input_tokens: n,
            rates: crate::jobs::CandidateRates { input, ..Default::default() },
            variants: Default::default(),
        };
        let t = canonical_tiers(&[
            tier(200_000, Some(4.0)),
            tier(32_000, Some(2.0)),
            tier(0, Some(1.0)),
            tier(64_000, None),
            tier(32_000, Some(9.0)),
        ]);
        assert_eq!(
            t.iter().map(|t| (t.above_input_tokens, t.rates.input)).collect::<Vec<_>>(),
            [(32_000, Some(2.0)), (200_000, Some(4.0))]
        );
    }
}
