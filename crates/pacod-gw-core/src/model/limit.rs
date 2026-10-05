//! Quotas, budgets and rate limits (concept section 12).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Doc, Status};

/// Whose consumption a limit counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LimitSubject {
    /// Everything in the organisation the limit belongs to.
    Organisation,
    /// Everything in the workspace the limit belongs to.
    Workspace,
    /// Each API key of the workspace separately.
    Key,
    /// Each end user of the workspace separately.
    EndUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// Input, cached and output tokens together.
    Tokens,
    InputTokens,
    OutputTokens,
    Requests,
    /// Cost in the reporting currency.
    Spend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Window {
    /// The last 60 seconds, sliding.
    Minute,
    Hour,
    Day,
    /// Starts on Monday.
    Week,
    Month,
    /// Never resets.
    Lifetime,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// The gateway rejects requests once the limit is used up.
    #[default]
    Request,
    /// The gateway counts and reports; the application decides at its own boundaries.
    Advisory,
}

/// Which calls count towards a limit. Empty lists mean no restriction.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LimitFilter {
    /// Model ids.
    pub models: Vec<String>,
    /// Template names.
    pub templates: Vec<String>,
    /// Tag values a call must carry, for example `purpose: synthesizer`.
    pub tags: BTreeMap<String, String>,
}

/// A limit definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LimitSpec {
    /// Slug, unique among the limits of its organisation or workspace.
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub subject: LimitSubject,
    pub metric: Metric,
    pub window: Window,
    /// The allowance per window. Zero allows nothing.
    pub amount: f64,
    #[serde(default)]
    pub enforcement: Enforcement,
    #[serde(default)]
    pub filter: LimitFilter,
    /// Percentages of the allowance at which an alert is raised, for example 50, 80, 100.
    #[serde(default)]
    pub alert_thresholds: Vec<u8>,
    #[serde(default)]
    pub status: Status,
}

/// A limit owned by an organisation. Its subject is always the organisation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct OrgLimit(pub LimitSpec);

impl Doc for OrgLimit {
    const KIND: &'static str = "org_limit";
    const PREFIX: &'static str = "olm";
}

/// A limit owned by a workspace. Its subject is the workspace, each key, or each end user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct WorkspaceLimit(pub LimitSpec);

impl Doc for WorkspaceLimit {
    const KIND: &'static str = "limit";
    const PREFIX: &'static str = "lim";
}

/// A different allowance for one end user, or an exemption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EndUserOverride {
    /// The limit's object id. It must be an end-user limit of the same workspace.
    pub limit_id: String,
    pub end_user: String,
    /// The allowance for this end user. Ignored when `exempt` is set.
    #[serde(default)]
    pub amount: Option<f64>,
    /// The limit does not apply to this end user.
    #[serde(default)]
    pub exempt: bool,
}

impl Doc for EndUserOverride {
    const KIND: &'static str = "end_user_override";
    const PREFIX: &'static str = "euo";
}
