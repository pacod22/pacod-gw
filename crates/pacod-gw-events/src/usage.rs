//! One record per logical call, in both modes (concept section 14.1).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Outcome of a call as recorded on the event.
pub mod status {
    pub const OK: &str = "ok";
    pub const ERROR: &str = "error";
    pub const TIMEOUT: &str = "timeout";
    pub const CANCELLED: &str = "cancelled";
    pub const REJECTED: &str = "rejected";
}

/// Where the token counts on an event came from.
pub mod usage_source {
    pub const PROVIDER: &str = "provider";
    pub const ESTIMATED: &str = "estimated";
    pub const MISSING: &str = "missing";
}

/// The stages behind the gateway delay (concept section 14.2). All values are milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Timing {
    /// `pre_ms + post_ms`: the time by which the gateway delayed the answer.
    pub gateway_delay_ms: f32,
    /// Reading the request from the client.
    pub receive_ms: f32,
    /// From the complete request to the start of the provider call.
    pub pre_ms: f32,
    /// Waiting for a pooled connection to the provider, or opening a new one.
    pub connect_ms: f32,
    /// Failed attempts and the waits between them.
    pub retry_ms: f32,
    /// From the connection being ready to the provider's response head.
    pub upstream_first_byte_ms: f32,
    /// From the provider's first byte to its last.
    pub upstream_stream_ms: f32,
    /// The gateway's work on the answer.
    pub post_ms: f32,
    /// Streams only: the longest time any chunk spent inside the gateway.
    pub relay_max_ms: f32,
    /// Accounting after the response has been sent.
    pub settle_ms: f32,
    /// From the first request byte to the last response byte.
    pub total_ms: f32,
}

/// Token and unit counts in the canonical form (concept section 8.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens that were not served from a cache.
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    /// All cache writes; the two fields below split it by lifetime where the provider reports that.
    pub cache_write_tokens: u64,
    pub cache_write_5m_tokens: u64,
    pub cache_write_1h_tokens: u64,
    pub output_tokens: u64,
    /// Part of `output_tokens`.
    pub reasoning_tokens: u64,
    /// Speech: characters of input text.
    pub characters: u64,
    /// Provider-executed tool calls, for example web searches.
    pub server_tool_calls: u32,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        *self == Usage::default()
    }

    /// Total prompt size as OpenAI reports it: uncached input plus cache reads and writes.
    pub fn prompt_total(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageEvent {
    // identity
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub instance_id: String,

    // scope
    pub organisation_id: String,
    pub workspace_id: String,
    pub key_id: String,

    // attribution
    #[serde(default)]
    pub end_user: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub trace_id: String,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,

    // routing
    /// `proxy` or `managed`.
    pub mode: String,
    /// `openai`, `anthropic`, `native`, or `proxy`.
    pub interface: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model_requested: String,
    #[serde(default)]
    pub model_served: String,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub template_version: u32,
    #[serde(default)]
    pub path: String,

    // outcome
    pub status: String,
    pub http_status: u16,
    #[serde(default)]
    pub error_type: String,
    #[serde(default)]
    pub error_message: String,
    #[serde(default)]
    pub finish_reason: String,
    pub attempts: u8,
    pub stream: bool,
    #[serde(default)]
    pub upstream_request_id: String,

    #[serde(flatten)]
    pub timing: Timing,

    #[serde(flatten)]
    pub usage: Usage,
    pub usage_source: String,

    // cost
    pub cost: Option<f64>,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub price_version: u32,
    #[serde(default)]
    pub price_variant: String,
    #[serde(default)]
    pub price_missing: bool,

    // other
    #[serde(default)]
    pub dropped_params: Vec<String>,
    /// Set when audit mode recorded the call.
    #[serde(default)]
    pub audit_id: String,
}

impl UsageEvent {
    /// An event with identity and scope filled and everything else at its zero value.
    pub fn new(id: Uuid, instance_id: impl Into<String>) -> Self {
        Self {
            id,
            timestamp: Utc::now(),
            instance_id: instance_id.into(),
            organisation_id: String::new(),
            workspace_id: String::new(),
            key_id: String::new(),
            end_user: String::new(),
            session_id: String::new(),
            trace_id: String::new(),
            tags: BTreeMap::new(),
            mode: String::new(),
            interface: String::new(),
            provider: String::new(),
            model_requested: String::new(),
            model_served: String::new(),
            template: String::new(),
            template_version: 0,
            path: String::new(),
            status: status::ERROR.to_owned(),
            http_status: 0,
            error_type: String::new(),
            error_message: String::new(),
            finish_reason: String::new(),
            attempts: 0,
            stream: false,
            upstream_request_id: String::new(),
            timing: Timing::default(),
            usage: Usage::default(),
            usage_source: usage_source::MISSING.to_owned(),
            cost: None,
            currency: String::new(),
            price_version: 0,
            price_variant: String::new(),
            price_missing: false,
            dropped_params: Vec::new(),
            audit_id: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trips_as_flat_json() {
        let mut e = UsageEvent::new(Uuid::now_v7(), "i-1");
        e.timing.pre_ms = 0.9;
        e.usage.input_tokens = 812;
        e.tags.insert("purpose".into(), "planner".into());
        let json = serde_json::to_value(&e).unwrap();
        // Timing and usage are flattened: ClickHouse receives one flat row.
        assert_eq!(json["pre_ms"], serde_json::json!(0.9f32));
        assert_eq!(json["input_tokens"], 812);
        assert!(json.get("timing").is_none());
        let back: UsageEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, e);
    }
}
