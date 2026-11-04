//! Evidence (docs/pacod-gw.md in pacod-broker, R3): how the upstreams the gateway called served,
//! reported to the broker as usage.
//!
//! Phase 1 (R3a): for each upstream model linked to a broker resource, the bridge holds a ticket
//! of its own and reports a sample of the gateway's calls under it, at most `sample_per_hour`
//! per resource and hour (the broker counts 120). Only calls made after the ticket was issued are
//! reported: the broker refuses older ones.

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Utc};
use pacod_broker_client::{Outcome, UsageReport};
use serde_json::Value;

/// A gateway call as a usage report; `None` for calls the upstream never saw or the caller left
/// (refused by the gateway, cancelled).
pub fn report(event: &Value, ticket: &str) -> Option<UsageReport> {
    let attempts = event["attempts"].as_u64().unwrap_or(0);
    let status = event["status"].as_str().unwrap_or_default();
    let http = event["http_status"].as_u64().unwrap_or(0);
    let outcome = match status {
        _ if attempts == 0 => return None,
        "ok" => Outcome::Ok,
        "timeout" => Outcome::Timeout,
        "rejected" | "cancelled" => return None,
        _ if http == 429 => Outcome::RateLimited,
        _ if (400..500).contains(&http) => Outcome::ClientError,
        _ => Outcome::ServerError,
    };
    let mut r = UsageReport::new(ticket, outcome);
    if outcome == Outcome::Ok {
        // The first token when the gateway records it (phase 2), else the first byte.
        r.ttft_ms =
            event["first_token_ms"].as_f64().or_else(|| event["upstream_first_byte_ms"].as_f64()).filter(|v| *v > 0.0);
        let tokens = event["output_tokens"].as_f64().unwrap_or(0.0);
        if event["stream"].as_bool() == Some(true) && tokens > 0.0 {
            r.ms_per_token = event["upstream_stream_ms"].as_f64().map(|ms| ms / tokens).filter(|v| *v > 0.0);
        }
    }
    r.at = event["timestamp"].as_str().and_then(|t| t.parse::<jiff::Timestamp>().ok());
    Some(r)
}

/// A ticket the bridge holds for one resource, and how many reports it sent this hour.
#[derive(Debug, Clone)]
pub struct Held {
    pub ticket: String,
    pub issued: DateTime<Utc>,
    /// The newest call already reported, and the ids of calls at that instant.
    pub reported_to: DateTime<Utc>,
    pub seen: HashSet<String>,
    pub hour: i64,
    pub sent_this_hour: u32,
}

impl Held {
    pub fn new(ticket: String, issued: DateTime<Utc>) -> Self {
        Self {
            ticket,
            issued,
            reported_to: issued,
            seen: HashSet::new(),
            hour: issued.timestamp() / 3600,
            sent_this_hour: 0,
        }
    }

    /// Tickets last 24 hours on the broker by default; a fresh one well before keeps reports valid.
    pub fn stale(&self, now: DateTime<Utc>) -> bool {
        now - self.issued > chrono::Duration::hours(12)
    }

    /// The calls of a batch to report now: newer than what was reported, not seen, within the
    /// hour's budget. `events` come newest first, as the gateway lists them.
    pub fn take(&mut self, events: Vec<Value>, budget: u32, now: DateTime<Utc>) -> Vec<UsageReport> {
        if self.hour != now.timestamp() / 3600 {
            self.hour = now.timestamp() / 3600;
            self.sent_this_hour = 0;
        }
        let mut fresh: VecDeque<(DateTime<Utc>, String, Value)> = VecDeque::new();
        for e in events {
            let Some(at) = e["timestamp"].as_str().and_then(|t| t.parse::<DateTime<Utc>>().ok()) else { continue };
            let id = e["id"].as_str().unwrap_or_default().to_string();
            if at < self.reported_to || at < self.issued || (at == self.reported_to && self.seen.contains(&id)) {
                continue;
            }
            fresh.push_front((at, id, e));
        }
        // Oldest first, so the budget goes to calls in order and the mark only moves forward.
        let mut out = Vec::new();
        for (at, id, e) in fresh {
            if self.sent_this_hour >= budget {
                break;
            }
            if at > self.reported_to {
                self.reported_to = at;
                self.seen.clear();
            }
            self.seen.insert(id);
            if let Some(r) = report(&e, &self.ticket) {
                out.push(r);
                self.sent_this_hour += 1;
            }
        }
        out
    }
}

/// Tickets by broker resource id.
pub type Tickets = HashMap<String, Held>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(id: &str, at: &str, status: &str, http: u64) -> Value {
        json!({"id": id, "timestamp": at, "status": status, "http_status": http, "attempts": 1, "stream": true,
               "upstream_first_byte_ms": 220.0, "upstream_stream_ms": 900.0, "output_tokens": 90})
    }

    #[test]
    fn outcomes_follow_the_gateways_record() {
        let r = report(&event("a", "2026-11-04T10:00:00Z", "ok", 200), "tkt").unwrap();
        assert_eq!(r.outcome, Outcome::Ok);
        assert_eq!(r.ttft_ms, Some(220.0));
        assert_eq!(r.ms_per_token, Some(10.0));
        assert_eq!(
            report(&event("b", "2026-11-04T10:00:00Z", "error", 429), "t").unwrap().outcome,
            Outcome::RateLimited
        );
        assert_eq!(
            report(&event("c", "2026-11-04T10:00:00Z", "error", 502), "t").unwrap().outcome,
            Outcome::ServerError
        );
        assert_eq!(
            report(&event("d", "2026-11-04T10:00:00Z", "error", 400), "t").unwrap().outcome,
            Outcome::ClientError
        );
        assert_eq!(report(&event("e", "2026-11-04T10:00:00Z", "timeout", 0), "t").unwrap().outcome, Outcome::Timeout);
        assert!(report(&event("f", "2026-11-04T10:00:00Z", "rejected", 429), "t").is_none(), "the gateway refused it");
        let mut never = event("g", "2026-11-04T10:00:00Z", "error", 502);
        never["attempts"] = json!(0);
        assert!(report(&never, "t").is_none(), "the upstream never saw it");
    }

    #[test]
    fn only_new_calls_within_the_budget_after_the_ticket() {
        let issued: DateTime<Utc> = "2026-11-04T10:00:00Z".parse().unwrap();
        let mut h = Held::new("tkt".into(), issued);
        let now: DateTime<Utc> = "2026-11-04T10:30:00Z".parse().unwrap();
        // Newest first, as the gateway lists them; one before the ticket.
        let batch = vec![
            event("c", "2026-11-04T10:20:00Z", "ok", 200),
            event("b", "2026-11-04T10:10:00Z", "ok", 200),
            event("a", "2026-11-04T09:59:00Z", "ok", 200),
        ];
        assert_eq!(h.take(batch.clone(), 100, now).len(), 2);
        assert!(h.take(batch, 100, now).is_empty(), "nothing twice");
        let more = vec![event("e", "2026-11-04T10:25:00Z", "ok", 200), event("d", "2026-11-04T10:21:00Z", "ok", 200)];
        assert_eq!(h.take(more, 3, now).len(), 1, "the hour's budget of three is spent after one more");
    }
}
