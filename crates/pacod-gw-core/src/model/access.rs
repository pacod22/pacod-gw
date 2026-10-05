//! Console users, admin tokens and roles (concept section 5.4).

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Doc, Status};

/// A person who can log in to the console.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct User {
    /// Login name, unique.
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    /// Argon2id hash. Never returned by the API.
    pub password_hash: String,
    #[serde(default)]
    pub status: Status,
}

impl Doc for User {
    const KIND: &'static str = "user";
    const PREFIX: &'static str = "usr";
}

/// A token for automation on the management API. Roles are bound to it like to a user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AdminToken {
    pub name: String,
    /// HMAC-SHA256 of the token, hex.
    pub secret_hmac: String,
    #[serde(default)]
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub status: Status,
}

impl Doc for AdminToken {
    const KIND: &'static str = "admin_token";
    const PREFIX: &'static str = "tok";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleName {
    /// Everything.
    PlatformAdmin,
    /// One organisation: its workspaces, members, grants, credentials and caps.
    OrgAdmin,
    /// One workspace: templates, keys, limits, settings, usage and audit records.
    WorkspaceAdmin,
    /// Read configuration, usage, audit records and the audit log. Change nothing.
    Auditor,
    /// Read configuration and usage. No secrets, no audit records.
    Viewer,
}

/// Where a role applies.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum RoleScope {
    Platform,
    Organisation(String),
    Workspace(String),
}

/// Gives a user or an admin token a role in a scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RoleBinding {
    /// The id of a user or an admin token.
    pub principal: String,
    pub role: RoleName,
    pub scope: RoleScope,
}

impl Doc for RoleBinding {
    const KIND: &'static str = "role_binding";
    const PREFIX: &'static str = "rb";
}

impl RoleBinding {
    /// A role must be bound in the scope it is made for.
    pub fn is_consistent(&self) -> bool {
        matches!(
            (self.role, &self.scope),
            (RoleName::PlatformAdmin, RoleScope::Platform)
                | (RoleName::OrgAdmin, RoleScope::Organisation(_))
                | (RoleName::WorkspaceAdmin, RoleScope::Workspace(_))
                | (
                    RoleName::Auditor | RoleName::Viewer,
                    RoleScope::Organisation(_) | RoleScope::Workspace(_) | RoleScope::Platform
                )
        )
    }
}
