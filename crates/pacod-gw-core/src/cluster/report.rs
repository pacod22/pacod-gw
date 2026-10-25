//! The live figures each instance publishes for the central console.
//!
//! Every few seconds an instance writes what it is doing into its row of the registry. The
//! figures are raw counts, so that the management API can add them up across instances:
//! request and error counts and the delay histogram rather than rates and percentiles, and
//! successful calls and their summed time to the first byte rather than a mean.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dataplane::Gw;
use crate::routing::TargetView;
use crate::spool::Spool;
use crate::telemetry::live::{Health, HealthCounts, TrafficCounts, WindowCounts};

/// The format of the report. A reader skips reports of another version.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveReport {
    pub v: u32,
    pub taken_at: Option<DateTime<Utc>>,
    /// Calls in progress.
    pub in_flight: u64,
    /// The last minute.
    pub traffic: TrafficCounts,
    /// The last five minutes, per provider and model.
    pub health: Vec<HealthCounts>,
    /// Breakers, calls in flight and time to first byte per model and credential.
    pub routing: Vec<TargetView>,
    pub spool: SpoolFigures,
    pub audit_spool: SpoolFigures,
    pub config: ConfigFigures,
    /// The gateways this instance counts; it shares out key concurrency caps over them.
    pub data_plane_instances: u64,
    pub secrets: SecretChecks,
    /// The alert window. Missing in reports of older releases, which read as empty.
    pub window: WindowCounts,
    /// The response cache's store as this instance sees it. Missing in reports of older
    /// releases: unknown, not empty.
    pub answers: Option<AnswerFigures>,
}

/// What an instance knows of the store it keeps answers in (`answers::StoreFigures`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct AnswerFigures {
    /// `memory` or `valkey`.
    pub store: String,
    /// Answers held in this instance's memory; unset for Valkey, which every instance shares.
    pub entries: Option<u64>,
    pub bytes: Option<u64>,
    pub capacity_bytes: Option<u64>,
    /// Calls skip the store for a while after failures in a row.
    pub breaker_open: bool,
    pub failures_in_a_row: u32,
}

impl From<crate::answers::StoreFigures> for AnswerFigures {
    fn from(f: crate::answers::StoreFigures) -> Self {
        Self {
            store: f.store.to_owned(),
            entries: f.entries,
            bytes: f.bytes,
            capacity_bytes: f.capacity_bytes,
            breaker_open: f.breaker_open,
            failures_in_a_row: f.failures_in_a_row,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpoolFigures {
    pub enabled: bool,
    /// On disk; the size limit applies to this.
    pub bytes: u64,
    /// Not yet delivered.
    pub pending_bytes: u64,
    pub max_bytes: u64,
    pub dropped: u64,
}

impl SpoolFigures {
    fn of(spool: &Spool, max_bytes: u64) -> Self {
        Self {
            enabled: spool.enabled(),
            bytes: spool.bytes(),
            pending_bytes: spool.pending_bytes(),
            max_bytes,
            dropped: spool.dropped(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigFigures {
    /// The configuration version applied.
    pub version: i64,
    /// How many versions behind the database it was at the last poll.
    pub lag: i64,
    /// Configuration objects the instance could not use.
    pub problems: u64,
}

/// Key check values of the secrets every instance must share: an instance with another
/// pepper refuses every gateway key, one with another key-encryption key cannot read the
/// provider credentials.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecretChecks {
    pub kek_id: String,
    pub kek: String,
    pub pepper: String,
}

impl SecretChecks {
    pub fn of(gw: &Gw) -> Self {
        Self {
            kek_id: gw.boot.keks.first().map(|k| k.id.clone()).unwrap_or_default(),
            kek: gw.boot.keks.first().map(|k| k.check_value()).unwrap_or_default(),
            pepper: gw.boot.pepper.check_value(),
        }
    }
}

impl LiveReport {
    /// What this instance is doing now.
    pub fn collect(gw: &Gw) -> Self {
        let config = &gw.config;
        Self {
            v: VERSION,
            taken_at: Some(Utc::now()),
            in_flight: gw.live.in_flight() as u64,
            traffic: gw.live.traffic_counts(),
            health: gw.live.health_counts(),
            routing: config.routes().view(),
            spool: SpoolFigures::of(&gw.usage_spool, gw.boot.spool_max_bytes),
            audit_spool: SpoolFigures::of(&gw.audit_spool, gw.boot.audit_spool_max_bytes),
            config: ConfigFigures {
                version: config.version(),
                lag: config.lag(),
                problems: config.snapshot().problems.len() as u64,
            },
            data_plane_instances: gw.data_plane_instances.load(std::sync::atomic::Ordering::Relaxed) as u64,
            secrets: SecretChecks::of(gw),
            window: gw.live.window(),
            answers: Some(gw.answers.figures().into()),
        }
    }

    /// Reads a stored report; `None` for one of another format version.
    pub fn parse(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value::<Self>(value.clone()).ok().filter(|r| r.v == VERSION)
    }
}

/// Health per provider and model over several instances, worst first.
pub fn merged_health<'a>(reports: impl IntoIterator<Item = &'a LiveReport>) -> Vec<Health> {
    let mut by_model: std::collections::BTreeMap<(String, String), HealthCounts> = Default::default();
    for r in reports {
        for h in &r.health {
            let entry = by_model.entry((h.provider.clone(), h.model.clone())).or_insert_with(|| HealthCounts {
                provider: h.provider.clone(),
                model: h.model.clone(),
                ..Default::default()
            });
            entry.ok += h.ok;
            entry.failed += h.failed;
            entry.first_byte_ms_sum += h.first_byte_ms_sum;
        }
    }
    let mut out: Vec<Health> = by_model.values().filter_map(Health::from_counts).collect();
    Health::sort(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(provider: &str, ok: u32, failed: u32, sum: f64) -> HealthCounts {
        HealthCounts { provider: provider.into(), model: "m".into(), ok, failed, first_byte_ms_sum: sum }
    }

    #[test]
    fn health_adds_up_across_instances() {
        let a = LiveReport { health: vec![health("p", 9, 1, 900.0), health("q", 5, 0, 50.0)], ..Default::default() };
        let b = LiveReport { health: vec![health("p", 1, 9, 300.0)], ..Default::default() };
        let merged = merged_health([&a, &b]);
        assert_eq!(merged.len(), 2);
        let p = &merged[0];
        assert_eq!((p.provider.as_str(), p.calls, p.failed), ("p", 20, 10), "worst first");
        assert_eq!(p.first_byte_ms, Some(120.0), "weighted by successful calls");
    }

    #[test]
    fn reports_round_trip_and_tolerate_change() {
        let r = LiveReport { v: VERSION, in_flight: 3, data_plane_instances: 2, ..Default::default() };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(LiveReport::parse(&json), Some(r));
        // A field a newer version added is ignored; a missing one takes its default.
        let newer = serde_json::json!({ "v": 1, "in_flight": 4, "something_new": true });
        assert_eq!(LiveReport::parse(&newer).map(|r| r.in_flight), Some(4));
        assert_eq!(LiveReport::parse(&serde_json::json!({ "v": 2 })), None);
    }

    #[test]
    fn the_cache_figures_of_an_older_release_are_unknown() {
        let older = serde_json::json!({ "v": 1, "in_flight": 1 });
        assert_eq!(LiveReport::parse(&older).map(|r| r.answers), Some(None));
        let figures = AnswerFigures { store: "memory".into(), entries: Some(2), ..Default::default() };
        let r = LiveReport { v: VERSION, answers: Some(figures), ..Default::default() };
        assert_eq!(LiveReport::parse(&serde_json::to_value(&r).unwrap()), Some(r));
    }
}
