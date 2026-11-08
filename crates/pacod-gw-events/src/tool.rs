//! One record per call of a tool or an agent through the gateway (catalogue MCP-14): an MCP tool
//! call on `/mcp`, or a request to an agent on `/a2a`.
//!
//! It is not a usage event. A tool call uses no tokens and has no model, and counting it among
//! the model calls would change every report of tokens, cost and requests. It has its own
//! spool, its own ingest path and its own table (`tool_calls`); the report of one trace shows
//! the model calls and the tool calls together.
//!
//! The arguments of a call are never recorded. `arguments_hmac` is a keyed hash of them: it
//! tells calls with the same arguments apart from the others without saying what they were, and
//! the key keeps small arguments from being guessed back from it.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The protocol of a call.
pub mod protocol {
    /// An MCP tool call (`tools/call`).
    pub const MCP: &str = "mcp";
    /// A request to an agent over A2A.
    pub const A2A: &str = "a2a";
}

/// What the gateway decided about a call before it reached the server or the agent.
pub mod verdict {
    /// Passed on.
    pub const ALLOWED: &str = "allowed";
    /// The caller may not use the tool or the agent: it is not granted, it is unknown, or the
    /// calling agent is disabled. One verdict for all of these, so that a caller learns nothing
    /// from it about tools it may not see.
    pub const DENIED: &str = "denied";
    /// The tool or the agent card is not approved, or changed since its approval.
    pub const UNAPPROVED: &str = "unapproved";
    /// A limit refused the call.
    pub const LIMITED: &str = "limited";
    /// The request itself was not acceptable: not one JSON-RPC request, or its headers and its
    /// body disagree.
    pub const INVALID: &str = "invalid";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    // identity
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub instance_id: String,

    // scope
    pub organisation_id: String,
    pub workspace_id: String,
    pub key_id: String,

    // attribution, as on the usage event
    #[serde(default)]
    pub end_user: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub trace_id: String,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    /// The agent that made the call.
    #[serde(default)]
    pub agent_id: String,
    /// The platform user the agent acts for.
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub task: String,

    // what was called
    /// `mcp` or `a2a`.
    pub protocol: String,
    /// The revision of the protocol the caller spoke: `2026-07-28` or `2025-11-25` for MCP,
    /// `1.0` for A2A. Empty when the request did not say.
    #[serde(default)]
    pub protocol_version: String,
    /// The tool server's or the agent's name, as registered; `broker` for pacod-broker's
    /// discovery tools.
    pub target: String,
    /// The id of the tool server or the agent; empty for the broker.
    #[serde(default)]
    pub target_id: String,
    /// The tool's own name on its server, or the A2A method.
    pub operation: String,
    /// The tool's name as the caller saw and called it (`<server>__<tool>`); empty for A2A.
    #[serde(default)]
    pub exposed_name: String,
    /// The hash of the approved tool definition or agent card the call was served under.
    #[serde(default)]
    pub pin_hash: String,
    /// A keyed hash of the canonical arguments; empty when the call had none.
    #[serde(default)]
    pub arguments_hmac: String,

    // outcome
    /// One of [`verdict`].
    pub verdict: String,
    /// `ok`, `error`, `timeout`, `cancelled` or `rejected`, as on the usage event.
    pub status: String,
    /// The HTTP status the caller was answered with.
    pub http_status: u16,
    /// The JSON-RPC error code of the answer; zero when it was a result.
    #[serde(default)]
    pub rpc_error_code: i32,
    #[serde(default)]
    pub error_type: String,
    #[serde(default)]
    pub error_message: String,
    /// The tool answered with `isError: true`, or the agent's task failed: the call worked and
    /// its result is an error.
    #[serde(default)]
    pub result_error: bool,

    // size and time
    /// Bytes of the request as received.
    #[serde(default)]
    pub request_bytes: u64,
    /// Bytes of the answer as sent to the caller.
    #[serde(default)]
    pub response_bytes: u64,
    /// The answer was cut or refused at the size the tool server allows.
    #[serde(default)]
    pub truncated: bool,
    /// From the request to the server or the agent to the end of its answer, in milliseconds.
    #[serde(default)]
    pub upstream_ms: f32,
    /// From the first request byte to the last answer byte, in milliseconds.
    #[serde(default)]
    pub total_ms: f32,
}

impl ToolCallRecord {
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
            agent_id: String::new(),
            user_id: String::new(),
            task: String::new(),
            protocol: String::new(),
            protocol_version: String::new(),
            target: String::new(),
            target_id: String::new(),
            operation: String::new(),
            exposed_name: String::new(),
            pin_hash: String::new(),
            arguments_hmac: String::new(),
            verdict: String::new(),
            status: String::new(),
            http_status: 0,
            rpc_error_code: 0,
            error_type: String::new(),
            error_message: String::new(),
            result_error: false,
            request_bytes: 0,
            response_bytes: 0,
            truncated: false,
            upstream_ms: 0.0,
            total_ms: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn a_record_is_one_flat_object() {
        let mut r = ToolCallRecord::new(Uuid::nil(), "core-1");
        r.tags.insert("team".into(), "a".into());
        let v = serde_json::to_value(&r).unwrap();
        let fields = v.as_object().unwrap();
        assert!(fields.values().all(|f| !f.is_object() || f == &json!({ "team": "a" })), "{v}");
        assert_eq!(serde_json::from_value::<ToolCallRecord>(v).unwrap(), r);
    }

    #[test]
    fn a_record_reads_with_only_its_required_fields() {
        let v: Value = json!({
            "id": Uuid::nil(), "timestamp": "2026-11-08T10:00:00Z", "instance_id": "core-1",
            "organisation_id": "org", "workspace_id": "ws", "key_id": "key",
            "protocol": "mcp", "target": "broker", "operation": "discover_resources",
            "verdict": "allowed", "status": "ok", "http_status": 200
        });
        let r: ToolCallRecord = serde_json::from_value(v).unwrap();
        assert_eq!(r.rpc_error_code, 0);
        assert!(r.arguments_hmac.is_empty() && !r.truncated);
    }
}
