//! Who is calling the management API, and what they may do (concept section 5.4).

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use chrono::Utc;

use super::AdminState;
use super::error::ApiError;
use crate::crypto::keys::{ADMIN_TOKEN_PREFIX, SESSION_PREFIX};
use crate::crypto::{ct_eq, sha256};
use crate::model::{RoleName, RoleScope, Status};
use crate::snapshot::Snapshot;
use crate::store::AuditEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    User,
    Token,
    /// The analytics service, authenticated with the shared internal token.
    Internal,
}

impl PrincipalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalKind::User => "user",
            PrincipalKind::Token => "token",
            PrincipalKind::Internal => "internal",
        }
    }
}

/// An authenticated caller with their roles.
#[derive(Debug, Clone)]
pub struct Principal {
    pub id: String,
    pub kind: PrincipalKind,
    /// Email or token name, for the audit log and the UI.
    pub display: String,
    pub roles: Vec<(RoleName, RoleScope)>,
    /// For a session: the SHA-256 of the session token, to end it on logout.
    pub session_hash: Option<[u8; 32]>,
}

impl Principal {
    fn has(&self, role: RoleName, scope: &RoleScope) -> bool {
        self.roles.iter().any(|(r, s)| *r == role && s == scope)
    }

    pub fn is_platform_admin(&self) -> bool {
        self.kind == PrincipalKind::Internal || self.has(RoleName::PlatformAdmin, &RoleScope::Platform)
    }

    pub fn can_admin_org(&self, org: &str) -> bool {
        self.is_platform_admin() || self.has(RoleName::OrgAdmin, &RoleScope::Organisation(org.to_owned()))
    }

    pub fn can_admin_workspace(&self, ws: &str, org: &str) -> bool {
        self.can_admin_org(org) || self.has(RoleName::WorkspaceAdmin, &RoleScope::Workspace(ws.to_owned()))
    }

    /// Any role on the platform or on this organisation.
    pub fn can_read_org(&self, org: &str) -> bool {
        self.is_platform_admin()
            || self.roles.iter().any(|(_, s)| match s {
                RoleScope::Platform => true,
                RoleScope::Organisation(o) => o == org,
                RoleScope::Workspace(_) => false,
            })
    }

    /// Any role on the platform, on the organisation or on this workspace.
    pub fn can_read_workspace(&self, ws: &str, org: &str) -> bool {
        self.can_read_org(org) || self.roles.iter().any(|(_, s)| matches!(s, RoleScope::Workspace(w) if w == ws))
    }

    /// Audit-mode records hold prompts and answers: admins and auditors only, never viewers.
    pub fn can_read_audit_records(&self, ws: &str, org: &str) -> bool {
        self.can_admin_workspace(ws, org)
            || self.roles.iter().any(|(r, s)| {
                *r == RoleName::Auditor
                    && match s {
                        RoleScope::Platform => true,
                        RoleScope::Organisation(o) => o == org,
                        RoleScope::Workspace(w) => w == ws,
                    }
            })
    }

    /// May read the audit log of administrative changes.
    pub fn can_read_audit_log(&self, org: Option<&str>) -> bool {
        self.is_platform_admin()
            || self.has(RoleName::Auditor, &RoleScope::Platform)
            || org.is_some_and(|o| {
                self.can_admin_org(o) || self.has(RoleName::Auditor, &RoleScope::Organisation(o.to_owned()))
            })
    }

    /// Organisations this principal can see at all.
    pub fn sees_org(&self, org: &str, snap: &Snapshot) -> bool {
        self.can_read_org(org)
            || self.roles.iter().any(|(_, s)| match s {
                RoleScope::Workspace(w) => snap.workspaces.get(w).is_some_and(|ws| ws.organisation_id == org),
                _ => false,
            })
    }

    /// The start of an audit entry for something this principal does.
    pub fn audit(&self, organisation_id: Option<&str>, workspace_id: Option<&str>) -> AuditEntry {
        AuditEntry {
            actor: self.id.clone(),
            actor_kind: self.kind.as_str().to_owned(),
            organisation_id: organisation_id.map(str::to_owned),
            workspace_id: workspace_id.map(str::to_owned),
            ..Default::default()
        }
    }
}

fn roles_of(snap: &Snapshot, principal_id: &str) -> Vec<(RoleName, RoleScope)> {
    snap.role_bindings
        .iter()
        .filter(|b| b.doc.principal == principal_id)
        .map(|b| (b.doc.role, b.doc.scope.clone()))
        .collect()
}

/// Resolves a bearer token to a principal.
pub async fn authenticate(st: &AdminState, token: &str) -> Result<Principal, ApiError> {
    let denied = || ApiError::unauthorized("invalid or expired credentials");
    let snap = st.config.snapshot();

    if token.starts_with(&format!("{SESSION_PREFIX}_")) {
        let hash = sha256(token.as_bytes());
        let user_id = st.store.session_user(&hash).await?.ok_or_else(denied)?;
        let user = snap.users.get(&user_id).filter(|u| u.doc.status == Status::Active).ok_or_else(denied)?;
        return Ok(Principal {
            roles: roles_of(&snap, &user.id),
            id: user.id.clone(),
            kind: PrincipalKind::User,
            display: user.doc.email.clone(),
            session_hash: Some(hash),
        });
    }

    if token.starts_with(&format!("{ADMIN_TOKEN_PREFIX}_")) {
        let hmac = st.boot.pepper.hash(token);
        let tok = snap
            .admin_tokens
            .get(&hmac)
            .filter(|t| t.doc.status == Status::Active && t.doc.expires_at.is_none_or(|e| Utc::now() < e))
            .ok_or_else(denied)?;
        return Ok(Principal {
            roles: roles_of(&snap, &tok.id),
            id: tok.id.clone(),
            kind: PrincipalKind::Token,
            display: tok.doc.name.clone(),
            session_hash: None,
        });
    }

    if !st.boot.internal_token.is_empty() && ct_eq(token.as_bytes(), st.boot.internal_token.as_bytes()) {
        return Ok(Principal {
            id: "internal".into(),
            kind: PrincipalKind::Internal,
            display: "analytics service".into(),
            roles: Vec::new(),
            session_hash: None,
        });
    }

    Err(denied())
}

impl FromRequestParts<AdminState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &AdminState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
            .map(str::trim)
            .ok_or_else(|| ApiError::unauthorized("send a session or admin token as a Bearer token"))?;
        authenticate(st, token).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(roles: Vec<(RoleName, RoleScope)>) -> Principal {
        Principal { id: "usr_1".into(), kind: PrincipalKind::User, display: "u".into(), roles, session_hash: None }
    }

    #[test]
    fn platform_admin_can_do_everything() {
        let a = p(vec![(RoleName::PlatformAdmin, RoleScope::Platform)]);
        assert!(a.can_admin_org("o") && a.can_admin_workspace("w", "o") && a.can_read_audit_records("w", "o"));
        assert!(a.can_read_audit_log(None));
    }

    #[test]
    fn org_admin_is_limited_to_its_organisation() {
        let a = p(vec![(RoleName::OrgAdmin, RoleScope::Organisation("o1".into()))]);
        assert!(a.can_admin_org("o1") && a.can_admin_workspace("w", "o1"));
        assert!(!a.can_admin_org("o2") && !a.can_read_org("o2") && !a.can_admin_workspace("w", "o2"));
        assert!(!a.is_platform_admin());
        assert!(a.can_read_audit_log(Some("o1")) && !a.can_read_audit_log(Some("o2")) && !a.can_read_audit_log(None));
    }

    #[test]
    fn workspace_admin_is_limited_to_its_workspace() {
        let a = p(vec![(RoleName::WorkspaceAdmin, RoleScope::Workspace("w1".into()))]);
        assert!(a.can_admin_workspace("w1", "o1") && a.can_read_workspace("w1", "o1"));
        assert!(!a.can_admin_workspace("w2", "o1") && !a.can_read_workspace("w2", "o1"));
        assert!(!a.can_admin_org("o1") && !a.can_read_org("o1"));
        assert!(a.can_read_audit_records("w1", "o1"));
    }

    #[test]
    fn viewer_reads_but_not_audit_records() {
        let v = p(vec![(RoleName::Viewer, RoleScope::Organisation("o1".into()))]);
        assert!(v.can_read_org("o1") && v.can_read_workspace("w1", "o1"));
        assert!(!v.can_admin_workspace("w1", "o1") && !v.can_read_audit_records("w1", "o1"));
    }

    #[test]
    fn auditor_reads_audit_records_and_changes_nothing() {
        let a = p(vec![(RoleName::Auditor, RoleScope::Workspace("w1".into()))]);
        assert!(a.can_read_audit_records("w1", "o1") && !a.can_read_audit_records("w2", "o1"));
        assert!(!a.can_admin_workspace("w1", "o1"));
        let org_auditor = p(vec![(RoleName::Auditor, RoleScope::Organisation("o1".into()))]);
        assert!(org_auditor.can_read_audit_records("w9", "o1") && org_auditor.can_read_audit_log(Some("o1")));
    }

    #[test]
    fn no_roles_means_no_access() {
        let n = p(vec![]);
        assert!(!n.can_read_org("o") && !n.can_read_workspace("w", "o") && !n.is_platform_admin());
    }
}
