//! Rate classes (concept section 5.5): named sets of throttles that platform admins define
//! once and API keys name, instead of carrying their own numbers.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Doc, Status};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RateClass {
    /// Slug, unique. Keys name the class by it.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Calls per minute (sliding) of each key in the class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<u32>,
    /// Tokens per minute (sliding) of each key in the class: input, cached and output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_minute: Option<u64>,
    /// Calls in progress at once of each key in the class, across all gateway instances.
    /// A key's own `max_concurrency` applies too; the lower one holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    #[serde(default)]
    pub status: Status,
}

impl Doc for RateClass {
    const KIND: &'static str = "rate_class";
    const PREFIX: &'static str = "rcl";
}
