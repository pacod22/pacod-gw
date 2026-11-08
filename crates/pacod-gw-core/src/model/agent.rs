//! Agents as the gateway knows them (catalogue MCP-15, without A2A): registered in a
//! workspace, spoken for only by their own keys, and narrower than those keys where they say
//! so. Until an agent is registered it is only a label a caller sends (`x-gw-agent-id`), which
//! anyone with a key of the workspace can claim.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Doc, Status};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Agent {
    /// Slug, unique in the workspace: the `agent_id` its calls carry in every report. It
    /// cannot change, so that an agent's reports stay one.
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    /// Model ids the agent may use, within its key's. Empty narrows nothing.
    #[serde(default)]
    pub allowed_models: Vec<String>,
    /// Template names the agent may use, within its key's. Empty narrows nothing.
    #[serde(default)]
    pub allowed_templates: Vec<String>,
    /// Tool bundles the agent may use, within its key's. Empty narrows nothing.
    #[serde(default)]
    pub tool_bundles: Vec<String>,
    /// `disabled` stops the agent: every call its keys make for it is refused, within a second
    /// on every instance.
    #[serde(default)]
    pub status: Status,
}

impl Doc for Agent {
    const KIND: &'static str = "agent";
    const PREFIX: &'static str = "agt";
}
