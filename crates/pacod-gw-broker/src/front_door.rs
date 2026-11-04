//! The gateway as the front door of its offerings (docs/pacod-gw.md in pacod-broker, R1b and
//! R3b): calls to an offering that quoted the caller's broker ticket.
//!
//! - **The provider's side (R1b).** The offering organisation checks the ticket with the
//!   broker (it must be for that offering), and once a day reports how the client behaved:
//!   `good_standing`, or `abuse` when the gateway refused it for its limits again and again.
//! - **The witness's side (R3b).** The bridge, holding the broker's `witness` role, reports the
//!   call itself on the client's own ticket: the evidence is the client's, confirmed by the
//!   gateway that carried it.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use pacod_broker_client::ClientReportKind;
use serde_json::Value;

/// Calls refused for the workspace's limits in one day after which a client is reported for
/// abuse. A client that keeps hitting limits it was told about is not merely busy.
pub const ABUSE_REFUSALS_PER_DAY: u32 = 20;

/// What the bridge knows of one client of one offering today.
#[derive(Debug, Default, Clone)]
pub struct Standing {
    pub day: i64,
    pub ok_calls: u32,
    pub refused: u32,
    /// The last ticket the client quoted, to report with.
    pub ticket: String,
    pub reported_good: bool,
    pub reported_abuse: bool,
}

/// Whether a call was refused for the workspace's limits.
pub fn refused_for_limits(event: &Value) -> bool {
    event["status"] == "rejected" && event["http_status"].as_u64() == Some(429)
}

impl Standing {
    /// Counts a call; a new day starts over.
    pub fn count(&mut self, event: &Value, ticket: &str, now: DateTime<Utc>) {
        let day = now.timestamp() / 86_400;
        if self.day != day {
            *self = Standing { day, ..Standing::default() };
        }
        self.ticket = ticket.to_string();
        if refused_for_limits(event) {
            self.refused += 1;
        } else if event["status"] == "ok" {
            self.ok_calls += 1;
        }
    }

    /// The report due now, if any: abuse once past the threshold, else good standing once a day
    /// after a successful call.
    pub fn due(&mut self) -> Option<ClientReportKind> {
        if self.refused >= ABUSE_REFUSALS_PER_DAY && !self.reported_abuse {
            self.reported_abuse = true;
            return Some(ClientReportKind::Abuse);
        }
        if self.ok_calls > 0 && !self.reported_good && !self.reported_abuse {
            self.reported_good = true;
            return Some(ClientReportKind::GoodStanding);
        }
        None
    }
}

/// Where the bridge is in the gateway's list of ticketed calls.
#[derive(Debug, Clone)]
pub struct Cursor {
    pub to: DateTime<Utc>,
    pub seen: HashSet<String>,
}

impl Cursor {
    pub fn new(from: DateTime<Utc>) -> Self {
        Self { to: from, seen: HashSet::new() }
    }

    /// The calls of a batch (newest first) not taken before, oldest first.
    pub fn take(&mut self, events: Vec<Value>) -> Vec<Value> {
        let mut fresh: Vec<(DateTime<Utc>, Value)> = events
            .into_iter()
            .filter_map(|e| Some((e["timestamp"].as_str()?.parse::<DateTime<Utc>>().ok()?, e)))
            .filter(|(at, e)| {
                *at > self.to || (*at == self.to && !self.seen.contains(e["id"].as_str().unwrap_or_default()))
            })
            .collect();
        fresh.sort_by_key(|(at, _)| *at);
        for (at, e) in &fresh {
            if *at > self.to {
                self.to = *at;
                self.seen.clear();
            }
            self.seen.insert(e["id"].as_str().unwrap_or_default().to_string());
        }
        fresh.into_iter().map(|(_, e)| e).collect()
    }
}

/// Standings by (organisation, client).
pub type Standings = HashMap<(String, String), Standing>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_client_in_good_standing_is_reported_once_a_day_and_an_abuser_once() {
        let now: DateTime<Utc> = "2026-11-04T10:00:00Z".parse().unwrap();
        let mut s = Standing::default();
        s.count(&json!({"status": "ok"}), "tkt_a", now);
        assert_eq!(s.due(), Some(ClientReportKind::GoodStanding));
        assert_eq!(s.due(), None, "once a day");
        for _ in 0..ABUSE_REFUSALS_PER_DAY {
            s.count(&json!({"status": "rejected", "http_status": 429}), "tkt_a", now);
        }
        assert_eq!(s.due(), Some(ClientReportKind::Abuse));
        assert_eq!(s.due(), None);
        let tomorrow = now + chrono::Duration::days(1);
        s.count(&json!({"status": "ok"}), "tkt_b", tomorrow);
        assert_eq!(s.due(), Some(ClientReportKind::GoodStanding), "a new day");
        assert_eq!(s.ticket, "tkt_b");
    }

    #[test]
    fn the_cursor_takes_each_call_once_in_order() {
        let mut c = Cursor::new("2026-11-04T10:00:00Z".parse().unwrap());
        let e = |id: &str, at: &str| json!({"id": id, "timestamp": at});
        let batch =
            vec![e("b", "2026-11-04T10:02:00Z"), e("a", "2026-11-04T10:01:00Z"), e("old", "2026-11-04T09:00:00Z")];
        let got: Vec<String> = c.take(batch.clone()).iter().map(|v| v["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(got, vec!["a", "b"]);
        assert!(c.take(batch).is_empty());
    }
}
