//! Who is calling the management API, and what they may do (concept section 5.4).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::LazyLock;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use chrono::Utc;
use pacod_id_authz::{Binding, Catalog, Manifest, ScopeRef, Subject};
use pacod_id_client::{AuthError, Caller};

use super::AdminState;
use super::error::ApiError;
use crate::crypto::keys::{ADMIN_TOKEN_PREFIX, SESSION_PREFIX};
use crate::crypto::sha256;
use crate::identity::Identity;
use crate::model::{RoleName, RoleScope, Status};
use crate::snapshot::Snapshot;
use crate::store::AuditEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    User,
    Token,
}

impl PrincipalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalKind::User => "user",
            PrincipalKind::Token => "token",
        }
    }
}

/// The gateway's manifest for pacod-id's rules engine (`authz/gw.yaml`): which role may do what,
/// at which scope. Every check below asks it.
static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
    Catalog::new().with(Manifest::from_yaml(include_str!("../../../../authz/gw.yaml")).expect("authz/gw.yaml is valid"))
});

/// The engine's name of a role: `gw.<role>`.
pub fn role_name(role: RoleName) -> String {
    let name = serde_json::to_value(role).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    format!("gw.{name}")
}

fn org_path(org: &str) -> Vec<ScopeRef> {
    vec![ScopeRef::platform(), ScopeRef::organisation(org)]
}

fn workspace_path(ws: &str, org: &str) -> Vec<ScopeRef> {
    vec![ScopeRef::platform(), ScopeRef::organisation(org), ScopeRef::of("gw.workspace", ws)]
}

/// An authenticated caller with their roles.
#[derive(Clone)]
pub struct Principal {
    pub id: String,
    pub kind: PrincipalKind,
    /// Email or token name, for the audit log and the UI.
    pub display: String,
    pub roles: Vec<(RoleName, RoleScope)>,
    /// For a session of the core's own (`PGW_AUTH=local`): the SHA-256 of the session token, to
    /// end it on logout.
    pub session_hash: Option<[u8; 32]>,
    /// For a person signed in through pacod-id: the SSO session, to end it on logout.
    pub sid: Option<String>,
    /// In OIDC mode, the bearer token itself: the facades exchange it for a pacod-id admin
    /// token acting for the caller (concept 13.2). Never logged or shown.
    pub(crate) bearer: Option<String>,
    /// The roles as the rules engine's bindings, each with its scope's path. A workspace role
    /// whose workspace is gone has no path and grants nothing.
    subject: Subject,
}

/// Leaves out the bearer token and the session hash, which must not reach a log.
impl std::fmt::Debug for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Principal")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("display", &self.display)
            .field("roles", &self.roles)
            .field("sid", &self.sid)
            .field("bearer", &self.bearer.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl Principal {
    /// `org_of` finds a workspace's organisation, to place workspace roles in their path.
    pub fn new(
        id: String,
        kind: PrincipalKind,
        display: String,
        roles: Vec<(RoleName, RoleScope)>,
        session_hash: Option<[u8; 32]>,
        org_of: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let bindings = roles
            .iter()
            .filter_map(|(role, scope)| {
                let path = match scope {
                    RoleScope::Platform => vec![ScopeRef::platform()],
                    RoleScope::Organisation(o) => org_path(o),
                    RoleScope::Workspace(w) => workspace_path(w, &org_of(w)?),
                };
                Some(Binding::new(role_name(*role), path))
            })
            .collect();
        let subject = match kind {
            PrincipalKind::User => Subject::user(bindings),
            PrincipalKind::Token => Subject::service(bindings),
        };
        Principal { id, kind, display, roles, session_hash, sid: None, bearer: None, subject }
    }

    /// The pacod-id session the caller's token belongs to.
    pub fn with_sid(mut self, sid: String) -> Self {
        self.sid = Some(sid);
        self
    }

    fn with_bearer(mut self, token: &str) -> Self {
        self.bearer = Some(token.to_owned());
        self
    }

    /// The caller's token, for a facade's call to pacod-id (OIDC mode only).
    pub(crate) fn bearer(&self) -> Result<&str, ApiError> {
        self.bearer.as_deref().ok_or_else(|| ApiError::internal("a facade without the caller's token"))
    }

    fn has(&self, role: RoleName, scope: &RoleScope) -> bool {
        let scope = match scope {
            RoleScope::Platform => ScopeRef::platform(),
            RoleScope::Organisation(o) => ScopeRef::organisation(o),
            RoleScope::Workspace(w) => ScopeRef::of("gw.workspace", w),
        };
        CATALOG.has_role(&self.subject, &role_name(role), &scope)
    }

    fn allowed(&self, permission: &str, at: &[ScopeRef]) -> bool {
        CATALOG.allowed(&self.subject, permission, at)
    }

    /// Any role for the whole platform, including viewer and auditor.
    pub fn has_any_platform_role(&self) -> bool {
        CATALOG.holds_any_at(&self.subject, &ScopeRef::platform())
    }

    pub fn is_platform_admin(&self) -> bool {
        self.has(RoleName::PlatformAdmin, &RoleScope::Platform)
    }

    pub fn can_admin_org(&self, org: &str) -> bool {
        self.allowed("gw.org.manage", &org_path(org))
    }

    pub fn can_admin_workspace(&self, ws: &str, org: &str) -> bool {
        self.allowed("gw.workspace.manage", &workspace_path(ws, org))
    }

    /// Any role on the platform or on this organisation.
    pub fn can_read_org(&self, org: &str) -> bool {
        self.allowed("gw.org.read", &org_path(org))
    }

    /// Any role on the platform, on the organisation or on this workspace.
    pub fn can_read_workspace(&self, ws: &str, org: &str) -> bool {
        self.allowed("gw.workspace.read", &workspace_path(ws, org))
    }

    /// Audit-mode records hold prompts and answers: admins and auditors only, never viewers.
    pub fn can_read_audit_records(&self, ws: &str, org: &str) -> bool {
        self.allowed("gw.audit_records.read", &workspace_path(ws, org))
    }

    /// Whether the principal holds `role` for the whole platform (`org` is `None`) or for
    /// an organisation.
    pub fn has_role_at(&self, role: RoleName, org: Option<&str>) -> bool {
        self.has(role, &RoleScope::Platform)
            || org.is_some_and(|o| self.has(role, &RoleScope::Organisation(o.to_owned())))
    }

    /// May read the audit log of administrative changes.
    pub fn can_read_audit_log(&self, org: Option<&str>) -> bool {
        match org {
            None => self.allowed("gw.audit_log.read", &[ScopeRef::platform()]),
            Some(o) => self.allowed("gw.audit_log.read", &org_path(o)),
        }
    }

    /// Organisations this principal can see at all: a role on the platform, on the
    /// organisation, or on one of its workspaces.
    pub fn sees_org(&self, org: &str) -> bool {
        CATALOG.touches(&self.subject, &org_path(org))
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

/// A workspace's organisation, from the snapshot.
fn org_of(snap: &Snapshot) -> impl Fn(&str) -> Option<String> + '_ {
    |ws| snap.workspaces.get(ws).map(|w| w.organisation_id.clone())
}

/// The gateway's roles among a caller's bindings at pacod-id: `gw.org_admin` at
/// `organisation:o1` is `(OrgAdmin, Organisation("o1"))`. Other products' and pacod-id's own
/// roles mean nothing to the gateway.
fn gw_roles(bindings: &[Binding]) -> Vec<(RoleName, RoleScope)> {
    bindings
        .iter()
        .filter_map(|b| {
            let role: RoleName = serde_json::from_value(serde_json::Value::from(b.role.strip_prefix("gw.")?)).ok()?;
            let at = b.path.last()?;
            let scope = match (at.kind.as_str(), at.id.as_deref()) {
                ("platform", _) => RoleScope::Platform,
                ("organisation", Some(o)) => RoleScope::Organisation(o.to_owned()),
                ("gw.workspace", Some(w)) => RoleScope::Workspace(w.to_owned()),
                _ => return None,
            };
            Some((role, scope))
        })
        .collect()
}

/// pacod-id cannot vouch for the caller now: its copy is old, or it does not answer.
pub(crate) fn identity_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "identity_unavailable",
        "sign-in is not available right now (pacod-id); try again shortly",
    )
}

/// `PGW_AUTH=oidc`: a token pacod-id signed (a person's, or a service account's exchanged one)
/// or a service token it knows (`pidt_`, imported `pgwa_`); the roles from its feed. The core's
/// own sessions (`pgws_`) are not taken any more.
async fn authenticate_oidc(st: &AdminState, id: &Identity, token: &str) -> Result<Principal, ApiError> {
    if token.starts_with(&format!("{SESSION_PREFIX}_")) {
        return Err(ApiError::unauthorized("sign in through pacod-id: the gateway's own sessions are no longer taken")
            .with_details(serde_json::json!({ "code": "moved_to_identity", "issuer": id.issuer() })));
    }
    if !id.fresh() {
        return Err(identity_unavailable());
    }
    let caller = id.auth.authenticate(token).await.map_err(|e| match e {
        AuthError::Unavailable(why) => {
            tracing::warn!(error = %why, "pacod-id could not check a management token");
            identity_unavailable()
        }
        _ => ApiError::unauthorized("invalid or expired credentials"),
    })?;
    let snap = st.config.snapshot();
    let roles = gw_roles(caller.bindings());
    Ok(match caller {
        Caller::User { sub, sid, email, name, .. } => {
            let display = email.or(name).unwrap_or_else(|| sub.clone());
            Principal::new(sub, PrincipalKind::User, display, roles, None, org_of(&snap)).with_sid(sid)
        }
        Caller::Service { sub, name, .. } => {
            Principal::new(sub, PrincipalKind::Token, name, roles, None, org_of(&snap))
        }
    }
    .with_bearer(token))
}

/// Resolves a bearer token to a principal.
pub async fn authenticate(st: &AdminState, token: &str) -> Result<Principal, ApiError> {
    if let Some(id) = &st.identity {
        return authenticate_oidc(st, id, token).await;
    }
    let denied = || ApiError::unauthorized("invalid or expired credentials");
    let snap = st.config.snapshot();

    if token.starts_with(&format!("{SESSION_PREFIX}_")) {
        let hash = sha256(token.as_bytes());
        let user_id = st.store.session_user(&hash).await?.ok_or_else(denied)?;
        let user = snap.users.get(&user_id).filter(|u| u.doc.status == Status::Active).ok_or_else(denied)?;
        return Ok(Principal::new(
            user.id.clone(),
            PrincipalKind::User,
            user.doc.email.clone(),
            roles_of(&snap, &user.id),
            Some(hash),
            org_of(&snap),
        ));
    }

    if token.starts_with(&format!("{ADMIN_TOKEN_PREFIX}_")) {
        let hmac = st.boot.pepper.hash(token);
        let tok = snap
            .admin_tokens
            .get(&hmac)
            .filter(|t| t.doc.status == Status::Active && t.doc.expires_at.is_none_or(|e| Utc::now() < e))
            .ok_or_else(denied)?;
        return Ok(Principal::new(
            tok.id.clone(),
            PrincipalKind::Token,
            tok.doc.name.clone(),
            roles_of(&snap, &tok.id),
            None,
            org_of(&snap),
        ));
    }

    // The internal token is no principal: the analytics service presents it only to the job
    // endpoints, which check it themselves (`resources/pricejob.rs`). Every other component
    // holds it too, and the analytics service parses pages from outside, so it must not open
    // the management API.
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
        // Admin tokens are what an attacker would guess at (the gateway's pgwa_, pacod-id's
        // pidt_, each guess an introspection there); the console sends sessions and signed
        // tokens, all from its own address, so only failed admin tokens count against an address.
        if !token.starts_with(&format!("{ADMIN_TOKEN_PREFIX}_")) && !token.starts_with("pidt_") {
            return authenticate(st, token).await;
        }
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |ConnectInfo(addr)| addr.ip());
        let ip = st.boot.trusted_proxies.client_ip(peer, &parts.headers);
        if st.token_failures.blocked(ip) {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many requests with invalid admin tokens from this address; try again in a minute",
            ));
        }
        let result = authenticate(st, token).await;
        if result.as_ref().is_err_and(|e| e.status == StatusCode::UNAUTHORIZED) {
            st.token_failures.failed(ip);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Workspaces w1, w2 and w belong to organisation o1, w9 to o1 as well.
    fn p(roles: Vec<(RoleName, RoleScope)>) -> Principal {
        Principal::new("usr_1".into(), PrincipalKind::User, "u".into(), roles, None, |_| Some("o1".to_owned()))
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

    #[test]
    fn the_role_names_are_the_manifests() {
        for role in
            [RoleName::PlatformAdmin, RoleName::OrgAdmin, RoleName::WorkspaceAdmin, RoleName::Auditor, RoleName::Viewer]
        {
            assert!(CATALOG.manifest("gw").unwrap().role(&role_name(role)).is_some(), "{role:?}");
        }
    }

    #[test]
    fn the_gateway_vectors_hold() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../authz");
        let text = std::fs::read_to_string(dir.join("vectors/gw.yaml")).unwrap();
        let load = |name: &str| {
            let text = std::fs::read_to_string(dir.join(format!("{name}.yaml"))).map_err(|e| e.to_string())?;
            Manifest::from_yaml(&text).map_err(|e| e.to_string())
        };
        let checks = pacod_id_authz::vectors::run(&text, load).unwrap_or_else(|f| panic!("{}", f.join("\n")));
        assert!(checks > 60, "{checks}");
    }

    #[test]
    fn a_workspace_role_whose_workspace_is_gone_grants_nothing() {
        let a = Principal::new(
            "usr_1".into(),
            PrincipalKind::User,
            "u".into(),
            vec![(RoleName::WorkspaceAdmin, RoleScope::Workspace("gone".into()))],
            None,
            |_| None,
        );
        assert!(!a.can_admin_workspace("gone", "o1") && !a.sees_org("o1"));
    }
}
