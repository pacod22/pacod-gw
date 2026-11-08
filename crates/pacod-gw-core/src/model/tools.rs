//! Tool servers (catalogue MCP-01, MCP-03, MCP-10): MCP servers an organisation registers, the
//! credential the gateway calls each with, and the tool definitions an administrator approved.
//!
//! Callers never reach a tool server directly. The gateway's `/mcp` lists the approved tools
//! of the servers a caller may use, under the name `<server>__<tool>`, and calls the server with
//! the organisation's credential: the caller's own key never leaves the gateway.
//!
//! The names here are `tool_*` and `connector_*`, not `mcp_*`: `mcp` and `mcp_servers` already
//! name Anthropic's provider-side MCP connector (`ScopeSettings::provider_tools`).

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CredentialScope, CredentialState, Doc, Status};
use crate::crypto::envelope::Sealed;

/// Longest name of a tool server: room is left for the tool's name within the 64 characters
/// providers allow a function name.
pub const MAX_SERVER_NAME: usize = 20;
/// Most tools a server may have pinned: every pinned definition is in every snapshot.
pub const MAX_TOOLS_PER_SERVER: usize = 500;

/// An MCP server an organisation registers, reached over Streamable HTTP.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolServer {
    /// Slug, unique in the organisation, at most 20 characters and starting with a letter. Its
    /// tools are called `<name>__<tool>`, so it cannot change once callers know them.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The server's MCP endpoint, `https://` (or `http://` inside a private network the
    /// operator allows for tool servers).
    pub url: String,
    /// How the gateway presents the server's credential. Without a current credential nothing
    /// is sent.
    #[serde(default)]
    pub auth: ToolAuth,
    /// The time a call to the server may take, in milliseconds (1000 to 300000).
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    /// What the gateway passes on of a tool's answer.
    #[serde(default)]
    pub result: ResultLimits,
    /// How the gateway keeps a session with a server of the 2025-11-25 revision, which has
    /// sessions. A server of the 2026-07-28 revision has none.
    #[serde(default)]
    pub session: SessionMode,
    /// The organisation's workspaces whose callers may use the server. Unset: every workspace.
    /// A workspace's bundles can grant only the servers it is let into here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<Vec<String>>,
    #[serde(default)]
    pub status: Status,
}

fn default_timeout_ms() -> u32 {
    30_000
}

impl Doc for ToolServer {
    const KIND: &'static str = "tool_server";
    const PREFIX: &'static str = "tsv";
}

/// The header that carries a tool server's credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolAuth {
    /// The header's name, for example `authorization` or `x-mcp-token`.
    #[serde(default = "default_auth_header")]
    pub header: String,
    /// Written before the secret, for example `Bearer `. Empty: the secret alone.
    #[serde(default = "default_auth_prefix")]
    pub prefix: String,
}

fn default_auth_header() -> String {
    "authorization".into()
}

fn default_auth_prefix() -> String {
    "Bearer ".into()
}

impl Default for ToolAuth {
    fn default() -> Self {
        Self { header: default_auth_header(), prefix: default_auth_prefix() }
    }
}

/// Limits on a tool's answer (catalogue MCP-13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResultLimits {
    /// The largest answer passed on, in bytes (1 KiB to 16 MiB). A larger one is refused with
    /// an error, never cut: a cut answer would be read as a whole one.
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u32,
    /// The kinds of content a tool's result may hold (`text`, `image`, `audio`, `resource`,
    /// `resource_link`). A result with another kind is refused.
    #[serde(default = "default_content_types")]
    pub content_types: Vec<String>,
}

fn default_max_bytes() -> u32 {
    1 << 20
}

fn default_content_types() -> Vec<String> {
    ["text", "image", "resource", "resource_link"].map(str::to_owned).into()
}

impl Default for ResultLimits {
    fn default() -> Self {
        Self { max_bytes: default_max_bytes(), content_types: default_content_types() }
    }
}

/// The kinds of content an MCP tool result may hold.
pub const CONTENT_TYPES: [&str; 5] = ["text", "image", "audio", "resource", "resource_link"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    /// One session per instance, workspace and credential, opened again when the server
    /// forgets it. Calls of one workspace share it; calls of different workspaces never do.
    #[default]
    Pooled,
    /// A session of its own for every call: for a server that keeps state in its sessions.
    PerCall,
}

/// The secret the gateway calls a tool server (or, later, a remote agent) with. Stored
/// encrypted and never returned by the API, like a provider's credential.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectorCredential {
    /// The tool server it is for.
    pub owner_id: String,
    /// The organisation's, or one workspace's own, which that workspace's calls use instead.
    pub scope: CredentialScope,
    #[serde(default)]
    pub label: String,
    pub sealed: Sealed,
    /// A label that identifies the secret without revealing it.
    pub fingerprint: String,
    #[serde(default)]
    pub state: CredentialState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retire_at: Option<DateTime<Utc>>,
}

impl Doc for ConnectorCredential {
    const KIND: &'static str = "connector_credential";
    const PREFIX: &'static str = "ccr";
}

/// A tool definition an administrator approved (catalogue MCP-03). Callers are shown this
/// definition, not what the server says now: a server that changes a tool's description or
/// schema cannot change what models read, and the change holds the tool until it is approved
/// again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolPin {
    /// The tool's own name on its server.
    pub tool: String,
    /// The name callers call it by: `<server>__<tool>`, or an alias where that is not a name
    /// providers accept.
    pub exposed_name: String,
    /// The definition as approved: `name`, `title`, `description`, `inputSchema`,
    /// `outputSchema` and `annotations`.
    pub definition: Value,
    /// `sha256:` and the hash of the canonical definition.
    pub hash: String,
    #[serde(default)]
    pub state: PinState,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
}

impl Doc for ToolPin {
    const KIND: &'static str = "tool_pin";
    const PREFIX: &'static str = "tpn";
}

/// Tools a workspace grants to keys (catalogue MCP-01, MCP-02). Nothing is granted by default:
/// a key reaches the tools of the bundles it names, of servers the workspace is let into, as
/// approved. Bundles also keep a caller's tool list short: `/mcp/{name}` serves one bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolBundle {
    /// Slug, unique in the workspace.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// What it grants: `mcp:<server>/*` (every approved tool of a server) or
    /// `mcp:<server>/<tool>` (one, by its own name on the server, which an alias does not
    /// change).
    pub allow: Vec<String>,
    /// What it never grants, in the same form. A deny entry wins over an allow entry.
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub status: Status,
}

impl Doc for ToolBundle {
    const KIND: &'static str = "tool_bundle";
    const PREFIX: &'static str = "tbn";
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PinState {
    /// Served as approved.
    #[default]
    Approved,
    /// The server's definition is no longer the approved one: not served until approved again.
    Changed,
    /// Switched off by an administrator.
    Disabled,
}
