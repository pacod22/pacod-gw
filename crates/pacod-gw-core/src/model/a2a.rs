//! Remote agents (catalogue ING-14, MCP-15): A2A agents an organisation registers, which its
//! callers reach through the gateway's `/a2a/{name}`, as it reaches tool servers through
//! `/mcp`. The gateway calls the agent with the organisation's credential (a
//! `ConnectorCredential`), serves the agent's card as approved and rewritten to itself, and
//! keeps each workspace's tasks apart.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::tools::{PinState, ToolAuth};
use super::{Doc, Status};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteAgent {
    /// Slug, unique in the organisation, at most 20 characters and starting with a letter:
    /// callers reach the agent at `/a2a/{name}`, and bundles grant it as `a2a:{name}`. It
    /// cannot change.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Where the agent's card is: usually `https://…/.well-known/agent-card.json`.
    pub card_url: String,
    /// How the gateway presents the agent's credential. Without a current credential nothing
    /// is sent.
    #[serde(default)]
    pub auth: ToolAuth,
    /// The time a request to the agent may take, in milliseconds (1000 to 600000), a stream
    /// included.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    /// The largest answer passed on, or the largest event of a stream, in bytes (1 KiB to
    /// 16 MiB). A larger one is refused, never cut.
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u32,
    /// The organisation's workspaces whose callers may use the agent. Unset: every workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<Vec<String>>,
    #[serde(default)]
    pub status: Status,
}

fn default_timeout_ms() -> u32 {
    120_000
}

fn default_max_bytes() -> u32 {
    4 << 20
}

impl Doc for RemoteAgent {
    const KIND: &'static str = "remote_agent";
    const PREFIX: &'static str = "rag";
}

/// A remote agent's card as an administrator approved it. Callers are served this card,
/// rewritten to the gateway, not what the agent says now; a probe that finds the card changed
/// holds the agent until it is approved again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentCardPin {
    /// The card as approved, without its signatures, which the gateway's rewriting would break.
    pub card: Value,
    /// `sha256:` and the hash of the canonical card.
    pub hash: String,
    /// The card's JSON-RPC interface of version 1.0, where the gateway calls the agent.
    pub interface_url: String,
    /// The `tenant` that interface declares, which every request must carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(default)]
    pub state: PinState,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
}

impl Doc for AgentCardPin {
    const KIND: &'static str = "agent_card";
    const PREFIX: &'static str = "acp";
}
