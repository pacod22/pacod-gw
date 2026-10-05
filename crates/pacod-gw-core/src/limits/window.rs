//! Window boundaries. Hour, day, week and month windows are aligned to the calendar of a
//! time zone; the minute window to the clock minute.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

use crate::model::Window;

/// Converts a local wall-clock time to an instant. Where the clock skips an hour the first
/// valid instant after it is used; where it repeats an hour, the earlier one.
fn local(tz: Tz, naive: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => t.with_timezone(&Utc),
        chrono::LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
        chrono::LocalResult::None => {
            // The local time does not exist (the clock was put forward): step over the gap.
            let mut t = naive;
            for _ in 0..8 {
                t += Duration::minutes(30);
                if let Some(v) = tz.from_local_datetime(&t).earliest() {
                    return v.with_timezone(&Utc);
                }
            }
            Utc.from_utc_datetime(&naive)
        }
    }
}

fn midnight(d: NaiveDate) -> NaiveDateTime {
    d.and_hms_opt(0, 0, 0).expect("midnight exists")
}

/// The start and end of the window that contains `now`, in unix seconds. Lifetime windows
/// start at 0 and never end.
pub fn bounds(window: Window, time_zone: &str, now: DateTime<Utc>) -> (i64, i64) {
    let tz: Tz = time_zone.parse().unwrap_or(chrono_tz::UTC);
    let here = now.with_timezone(&tz);
    let date = here.date_naive();
    let (start, end) = match window {
        Window::Lifetime => return (0, i64::MAX),
        Window::Minute => {
            let s = now.timestamp() - now.timestamp().rem_euclid(60);
            return (s, s + 60);
        }
        Window::Hour => {
            let s = now.timestamp() - i64::from(here.minute()) * 60 - i64::from(here.second());
            return (s, s + 3600);
        }
        Window::Day => (midnight(date), midnight(date + Duration::days(1))),
        Window::Week => {
            let monday = date - Duration::days(i64::from(date.weekday().num_days_from_monday()));
            (midnight(monday), midnight(monday + Duration::days(7)))
        }
        Window::Month => {
            let first = date.with_day(1).expect("day 1 exists");
            let next = if first.month() == 12 {
                NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)
            }
            .expect("first of a month exists");
            (midnight(first), midnight(next))
        }
    };
    (local(tz, start).timestamp(), local(tz, end).timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn fmt(window: Window, tz: &str, now: &str) -> (String, String) {
        let (s, e) = bounds(window, tz, at(now));
        let f = |t: i64| Utc.timestamp_opt(t, 0).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        (f(s), f(e))
    }

    #[test]
    fn utc_windows() {
        let now = "2026-10-04T13:47:21Z"; // a Sunday
        assert_eq!(fmt(Window::Minute, "UTC", now), ("2026-10-04T13:47:00Z".into(), "2026-10-04T13:48:00Z".into()));
        assert_eq!(fmt(Window::Hour, "UTC", now), ("2026-10-04T13:00:00Z".into(), "2026-10-04T14:00:00Z".into()));
        assert_eq!(fmt(Window::Day, "UTC", now), ("2026-10-04T00:00:00Z".into(), "2026-10-05T00:00:00Z".into()));
        assert_eq!(fmt(Window::Week, "UTC", now), ("2026-09-28T00:00:00Z".into(), "2026-10-05T00:00:00Z".into()));
        assert_eq!(fmt(Window::Month, "UTC", now), ("2026-10-01T00:00:00Z".into(), "2026-11-01T00:00:00Z".into()));
        assert_eq!(bounds(Window::Lifetime, "UTC", at(now)), (0, i64::MAX));
    }

    #[test]
    fn days_follow_the_workspace_time_zone() {
        // 23:30 UTC on the 4th is already the 5th in Sofia (UTC+3 in summer).
        assert_eq!(
            fmt(Window::Day, "Europe/Sofia", "2026-10-04T23:30:00Z"),
            ("2026-10-04T21:00:00Z".into(), "2026-10-05T21:00:00Z".into())
        );
        // December spans the year boundary.
        assert_eq!(
            fmt(Window::Month, "UTC", "2026-12-15T00:00:00Z"),
            ("2026-12-01T00:00:00Z".into(), "2027-01-01T00:00:00Z".into())
        );
    }

    #[test]
    fn a_day_with_a_clock_change_is_23_or_25_hours_long() {
        // Europe/Berlin goes back an hour on 2026-10-25 and forward on 2026-03-29.
        let (s, e) = bounds(Window::Day, "Europe/Berlin", at("2026-10-25T12:00:00Z"));
        assert_eq!(e - s, 25 * 3600);
        let (s, e) = bounds(Window::Day, "Europe/Berlin", at("2026-03-29T12:00:00Z"));
        assert_eq!(e - s, 23 * 3600);
    }

    #[test]
    fn half_hour_zones_and_unknown_zones() {
        // India is UTC+5:30: its hours start at half past in UTC.
        assert_eq!(
            fmt(Window::Hour, "Asia/Kolkata", "2026-10-04T13:47:00Z"),
            ("2026-10-04T13:30:00Z".into(), "2026-10-04T14:30:00Z".into())
        );
        // An unknown zone falls back to UTC rather than failing a request.
        assert_eq!(
            bounds(Window::Day, "Mars/Phobos", at("2026-10-04T12:00:00Z")),
            bounds(Window::Day, "UTC", at("2026-10-04T12:00:00Z"))
        );
    }

    #[test]
    fn a_midnight_that_does_not_exist() {
        // In Santiago the clock went from 00:00 to 01:00 on 2026-09-06, so that day has no
        // midnight. The window still has a start and lasts 23 hours.
        let (s, e) = bounds(Window::Day, "America/Santiago", at("2026-09-06T15:00:00Z"));
        assert!(s < at("2026-09-06T15:00:00Z").timestamp() && e > s);
        assert!((e - s) <= 24 * 3600);
    }
}
