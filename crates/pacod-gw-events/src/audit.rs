//! The full record of an audited call (concept section 14.6).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One of the four message views of an audited call, or one attempt of the two provider views.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AuditPart {
    /// Request parts: the HTTP method. Empty for responses.
    #[serde(default)]
    pub method: String,
    /// Request parts: path and query as received, or the full provider URL.
    #[serde(default)]
    pub url: String,
    /// Response parts: the HTTP status. Zero for requests.
    #[serde(default)]
    pub status: u16,
    /// Headers in order. Credentials are never present: they are removed before recording.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The body. UTF-8 text when `body_encoding` is `utf8`, base64 otherwise.
    #[serde(default)]
    pub body: String,
    /// `utf8`, `base64`, or `ref` when the body equals another part (see `same_as`).
    #[serde(default)]
    pub body_encoding: String,
    /// Size of the body in bytes before any truncation.
    #[serde(default)]
    pub body_bytes: u64,
    /// Name of the part whose body is byte-identical, when the body is stored only once.
    #[serde(default)]
    pub same_as: String,
    /// Streams: for each chunk, microseconds since the start of the call and its length in bytes.
    #[serde(default)]
    pub chunks: Vec<(u64, u32)>,
    /// True when the body was cut at the configured size limit.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub audit_id: String,
    /// The id of the usage event of the same call.
    pub event_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,

    pub organisation_id: String,
    pub workspace_id: String,
    pub key_id: String,
    #[serde(default)]
    pub end_user: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub trace_id: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub complexity: String,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub mode: String,

    pub client_request: AuditPart,
    /// One entry per attempt.
    pub provider_requests: Vec<AuditPart>,
    /// One entry per attempt.
    pub provider_responses: Vec<AuditPart>,
    pub client_response: AuditPart,

    /// How the call was resolved: template and version, where each parameter value came from,
    /// what the capability filter dropped, the limits checked and their state.
    pub resolution: serde_json::Value,
    /// The time stamps behind the gateway-delay stages, in microseconds since the start.
    pub timeline: serde_json::Value,
    /// Usage, cost and price version, finish reason, and the provider's error for failures.
    pub outcome: serde_json::Value,

    /// True when any part was cut at the size limit.
    #[serde(default)]
    pub truncated: bool,
}
