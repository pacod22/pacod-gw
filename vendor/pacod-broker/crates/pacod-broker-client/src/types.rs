//! What the broker's public API takes and answers, as the client sends and reads it.
//!
//! The resource model (`ProviderProfile`, `ResourceSpec`, prices, availability, reputation) and
//! the discovery `Query` are the broker's own types, so an announcement that compiles here has
//! the shape the broker expects; `Announcement::validate` runs the broker's own checks before
//! anything is sent.

use std::collections::HashMap;

use jiff::Timestamp;
use pacod_broker_model::availability::AvailabilitySummary;
use pacod_broker_model::pricing::{EffectivePrice, PriceSummary};
use pacod_broker_model::schedule::Span;
use pacod_broker_model::{LiveStatus, Problems, ProviderProfile, Reputation, ResourceRecord, ResourceSpec, Source};
use serde::{Deserialize, Serialize};

pub use pacod_broker_index::{AvailabilityQuery, Coverage, HardwareQuery, ModelQuery, Query, SoftwareQuery, Sort};

/// What a provider announces: its profile and its resources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Announcement {
    /// Seconds the lease lasts without a heartbeat; the broker's default when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_s: Option<u64>,
    /// Replace the resources of this lease instead of opening a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    pub provider: ProviderProfile,
    #[serde(default)]
    pub resources: Vec<ResourceSpec>,
}

impl Announcement {
    /// The broker's own checks, before sending: every problem, each with its path.
    pub fn validate(&self, max_description_bytes: usize) -> Result<(), Problems> {
        let mut p = Problems::new();
        self.provider.validate(&mut p, max_description_bytes);
        for (i, r) in self.resources.iter().enumerate() {
            let path = format!("resources[{i}]");
            r.validate(&mut p, &path, max_description_bytes);
            r.validate_runs_on(self.provider.inventory.as_ref(), &mut p, &path);
        }
        p.into_result()
    }

    /// Reads an announcement from YAML or JSON text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let t = text.trim_start();
        if t.starts_with('{') {
            serde_json::from_str(text).map_err(|e| e.to_string())
        } else {
            serde_saphyr::from_str(text).map_err(|e| e.to_string())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Announced {
    pub lease_id: String,
    pub provider_id: String,
    pub ttl_s: u64,
    pub heartbeat_every_s: u64,
    pub expires_at: Timestamp,
    /// The id of each resource, by key.
    pub resources: HashMap<String, String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// The live state a heartbeat carries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<LiveStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<f64>,
    /// Per resource, by key.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub resources: HashMap<String, HeartbeatOne>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HeartbeatOne {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<LiveStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoverResponse {
    pub generation: u64,
    pub at: Timestamp,
    #[serde(default)]
    pub during: Option<Span>,
    pub total: usize,
    pub hits: Vec<Hit>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// One discovery result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub id: String,
    pub resource: ResourceRecord,
    pub provider: ProviderSummary,
    #[serde(default)]
    pub price: Option<EffectivePrice>,
    #[serde(default)]
    pub price_summary: Option<PriceSummary>,
    pub available: bool,
    #[serde(default)]
    pub capacity: Option<u32>,
    #[serde(default)]
    pub availability_summary: Option<AvailabilitySummary>,
    pub live: Live,
    #[serde(default)]
    pub reputation: Option<Reputation>,
    #[serde(default)]
    pub provider_reputation: Option<Reputation>,
    pub why: Why,
    /// Quote it in usage reports and ratings.
    #[serde(default)]
    pub ticket: Option<String>,
}

impl Hit {
    /// The first interface of the protocol given, or the first one.
    pub fn interface(&self, protocol: Option<&str>) -> Option<&pacod_broker_model::Interface> {
        let all = &self.resource.spec.interfaces;
        match protocol {
            Some(p) => all.iter().find(|i| i.protocol == p),
            None => all.first(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Live {
    pub status: LiveStatus,
    #[serde(default)]
    pub in_use: Option<u32>,
    #[serde(default)]
    pub ttft_ms: Option<f64>,
    #[serde(default)]
    pub expires_at: Option<Timestamp>,
    #[serde(default)]
    pub last_seen: Option<Timestamp>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Why {
    #[serde(default)]
    pub matched: Vec<String>,
    #[serde(default)]
    pub sorted_by: Option<String>,
    #[serde(default)]
    pub sort_value: Option<f64>,
    #[serde(default)]
    pub relevance: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: String,
    pub verified: bool,
}

/// One resource, read on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceView {
    #[serde(flatten)]
    pub record: ResourceRecord,
    pub provider: ProviderSummary,
    pub live: Live,
    #[serde(default)]
    pub price_now: Option<EffectivePrice>,
    pub available_now: bool,
    #[serde(default)]
    pub capacity_now: Option<u32>,
    #[serde(default)]
    pub reputation: Option<Reputation>,
    #[serde(default)]
    pub provider_reputation: Option<Reputation>,
    #[serde(default)]
    pub ticket: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderView {
    pub id: String,
    #[serde(default)]
    pub organisation_id: Option<String>,
    pub verified: bool,
    pub source: Source,
    pub profile: ProviderProfile,
    pub registered_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default)]
    pub reputation: Option<Reputation>,
    pub resources: Vec<ResourceBrief>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceBrief {
    pub id: String,
    pub key: String,
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub summary: String,
}

/// How a call ended, as the client reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    /// The caller's own mistake: does not count against the resource.
    ClientError,
    ServerError,
    Timeout,
    RateLimited,
    /// Refused although the resource said it had capacity.
    Refused,
}

impl std::str::FromStr for Outcome {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        serde_json::from_value(serde_json::Value::String(s.into()))
            .map_err(|_| format!("{s:?}: ok, client_error, server_error, timeout, rate_limited or refused"))
    }
}

/// One call's report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageReport {
    pub ticket: String,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms_per_token: Option<f64>,
    /// What was charged for one unit of `rate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_charged: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
}

impl UsageReport {
    pub fn new(ticket: impl Into<String>, outcome: Outcome) -> Self {
        Self {
            ticket: ticket.into(),
            outcome,
            ttft_ms: None,
            ms_per_token: None,
            price_charged: None,
            rate: None,
            at: None,
        }
    }

    pub fn latency(mut self, ttft_ms: f64, ms_per_token: Option<f64>) -> Self {
        self.ttft_ms = Some(ttft_ms);
        self.ms_per_token = ms_per_token;
        self
    }

    pub fn charged(mut self, rate: impl Into<String>, price: f64) -> Self {
        self.rate = Some(rate.into());
        self.price_charged = Some(price);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Accepted {
    pub accepted: usize,
    /// Reports that did not count, by position, with the reason.
    #[serde(default)]
    pub refused: Vec<(usize, String)>,
}

/// What a provider reports about a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientReportKind {
    Abuse,
    PolicyViolation,
    NonPayment,
    NoShow,
    LateCancel,
    GoodStanding,
}

impl std::str::FromStr for ClientReportKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        serde_json::from_value(serde_json::Value::String(s.into()))
            .map_err(|_| format!("{s:?}: abuse, policy_violation, non_payment, no_show, late_cancel or good_standing"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientReported {
    pub id: String,
    pub counted: bool,
}

/// Who presented a ticket, for providers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketInfo {
    pub client: String,
    pub resource_id: String,
    #[serde(default)]
    pub issued_at: Option<Timestamp>,
    #[serde(default)]
    pub client_reputation: Option<Reputation>,
}

/// The caller's own reputation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Me {
    pub client: String,
    #[serde(default)]
    pub as_client: Option<Reputation>,
    pub provider_id: String,
    #[serde(default)]
    pub as_provider: Option<Reputation>,
}

/// A change from the watch stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WatchEvent {
    /// First event: the generation the stream starts at.
    Hello {
        generation: u64,
    },
    /// The stream fell behind, or the caller's generation is old: read the catalogue again.
    Resync,
    Upserted {
        resource_id: String,
        provider_id: String,
    },
    Removed {
        resource_id: String,
        provider_id: String,
        reason: String,
    },
    Provider {
        provider_id: String,
        what: String,
    },
    Generation {
        generation: u64,
        resources: usize,
    },
}

/// An `application/problem+json` answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: u16,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub errors: Vec<String>,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status, self.detail.as_deref().unwrap_or(&self.title))?;
        for e in &self.errors {
            write!(f, "\n  - {e}")?;
        }
        Ok(())
    }
}
