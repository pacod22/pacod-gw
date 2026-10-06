//! Members of organisations and workspaces, and invitations to them.
//!
//! A member is a user or an admin token with a role binding in the organisation or in one of
//! its workspaces; the lists join the bindings with names from the snapshot. Members are the
//! organisation admins' business (concept section 5.4): they change roles, remove members and
//! invite people by email.
//!
//! An invitation carries a one-time token, of which only the hash is stored (`invitations`).
//! When the installation sends email the link goes to the invited address; otherwise, or when
//! sending fails, the token is returned once and the inviter hands the link on. Someone who
//! has no account yet chooses a password when accepting. Someone who has one accepts while
//! signed in, from the link or from the list of their invitations.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::AdminState;
use super::account::{invalid_token, send_in_background};
use super::auth::{Principal, PrincipalKind, authenticate};
use super::crud::{Params, Resource, ScopeKind, resolve_scope};
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use super::resources::access::MIN_PASSWORD_LEN;
use super::session::RoleView;
use crate::crypto::{password, random_bytes, sha256};
use crate::mail;
use crate::model::{Doc, Obj, RoleBinding, RoleName, RoleScope, Status, User, new_id};
use crate::snapshot::Snapshot;
use crate::store::{AuditEntry, Invitation, NewInvitation, TokenPurpose};

// --- members ---------------------------------------------------------------------------------

/// One role of a member.
#[derive(Debug, Serialize, JsonSchema)]
pub struct MemberRole {
    /// The role binding that gives the role.
    pub binding_id: String,
    pub role: RoleName,
    pub scope: RoleScope,
    /// The display name of the organisation or workspace.
    pub scope_name: Option<String>,
    /// On a workspace's list: the role is the organisation's, and applies to the workspace too.
    pub inherited: bool,
}

/// A user or an admin token with roles in an organisation or a workspace.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Member {
    pub principal_id: String,
    /// `user` or `token`.
    pub kind: String,
    /// A user's display name, a token's name.
    pub name: String,
    /// A user's email address.
    pub email: Option<String>,
    pub status: Status,
    pub roles: Vec<MemberRole>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MemberList {
    pub items: Vec<Member>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MemberRoleInput {
    /// Replaces every role the member has in this scope. `null` takes them away, here only:
    /// in the organisation, the member keeps their roles in its workspaces.
    pub role: Option<RoleName>,
}

/// Who a principal id is: kind, name, email and status. `None` for an unknown id.
fn principal_of(snap: &Snapshot, id: &str) -> Option<(&'static str, String, Option<String>, Status)> {
    if let Some(u) = snap.users.get(id) {
        return Some(("user", u.doc.display_name.clone(), Some(u.doc.email.clone()), u.doc.status));
    }
    snap.admin_tokens.values().find(|t| t.id == id).map(|t| ("token", t.doc.name.clone(), None, t.doc.status))
}

/// How the console and emails name a principal.
fn principal_name(snap: &Snapshot, id: &str) -> String {
    principal_of(snap, id).map(|(_, name, _, _)| name).unwrap_or_else(|| id.to_owned())
}

/// The members whose bindings `select` keeps; it answers whether a binding is inherited.
fn members_of(snap: &Snapshot, select: impl Fn(&RoleScope) -> Option<bool>) -> Vec<Member> {
    let mut by_id: HashMap<&str, Member> = HashMap::new();
    for b in &snap.role_bindings {
        let Some(inherited) = select(&b.doc.scope) else { continue };
        let Some((kind, name, email, status)) = principal_of(snap, &b.doc.principal) else { continue };
        let member = by_id.entry(&b.doc.principal).or_insert_with(|| Member {
            principal_id: b.doc.principal.clone(),
            kind: kind.into(),
            name,
            email,
            status,
            roles: Vec::new(),
        });
        member.roles.push(MemberRole {
            binding_id: b.id.clone(),
            role: b.doc.role,
            scope: b.doc.scope.clone(),
            scope_name: RoleView::new(snap, b.doc.role, &b.doc.scope).scope_name,
            inherited,
        });
    }
    let mut items: Vec<Member> = by_id.into_values().collect();
    for m in &mut items {
        // The organisation's roles first, then the workspaces' by name.
        m.roles.sort_by_key(|r| (!matches!(r.scope, RoleScope::Organisation(_)), r.scope_name.clone()));
    }
    items.sort_by_key(|m| (m.name.to_lowercase(), m.principal_id.clone()));
    items
}

fn in_org(snap: &Snapshot, org: &str, scope: &RoleScope) -> bool {
    match scope {
        RoleScope::Platform => false,
        RoleScope::Organisation(o) => o == org,
        RoleScope::Workspace(w) => snap.workspaces.get(w).is_some_and(|ws| ws.organisation_id == org),
    }
}

async fn list_org_members(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<MemberList>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Organisation, &params, &snap)?;
    if !p.can_read_org(scope.org()) {
        return Err(ApiError::forbidden());
    }
    let items = members_of(&snap, |s| in_org(&snap, scope.org(), s).then_some(false));
    Ok(Json(MemberList { items }))
}

async fn list_workspace_members(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<MemberList>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !p.can_read_workspace(scope.ws(), scope.org()) {
        return Err(ApiError::forbidden());
    }
    let items = members_of(&snap, |s| match s {
        RoleScope::Workspace(w) if w == scope.ws() => Some(false),
        RoleScope::Organisation(o) if o == scope.org() => Some(true),
        _ => None,
    });
    Ok(Json(MemberList { items }))
}

/// Replaces the bindings `remove` selects and adds `add`, refusing a change that leaves the
/// organisation without an organisation admin unless a platform admin makes it.
async fn change_bindings(
    st: &AdminState,
    p: &Principal,
    org: &str,
    ws: Option<&str>,
    remove: impl Fn(&RoleBinding) -> bool,
    add: Option<RoleBinding>,
) -> ApiResult<usize> {
    let mut tx = st.store.begin().await?;
    let mut removed = 0;
    let mut org_admins = 0;
    let org_scope = RoleScope::Organisation(org.to_owned());
    for row in tx.list(RoleBinding::KIND, None).await? {
        let b: Obj<RoleBinding> = row.into_obj()?;
        if remove(&b.doc) {
            tx.delete(&b.id, p.audit(Some(org), ws)).await?;
            removed += 1;
        } else if b.doc.role == RoleName::OrgAdmin && b.doc.scope == org_scope {
            org_admins += 1;
        }
    }
    if let Some(doc) = add {
        if doc.role == RoleName::OrgAdmin && doc.scope == org_scope {
            org_admins += 1;
        }
        tx.create(
            RoleBinding::KIND,
            &new_id(RoleBinding::PREFIX),
            None,
            <RoleBinding as Resource>::unique_name(&doc).as_deref(),
            &serde_json::to_value(&doc).map_err(ApiError::internal)?,
            p.audit(Some(org), ws),
        )
        .await?;
    }
    if org_admins == 0 && !p.is_platform_admin() {
        return Err(ApiError::conflict(
            "the organisation would have no organisation admin left; make someone else its admin first",
        )
        .with_details(serde_json::json!({ "code": "last_org_admin" })));
    }
    st.commit(tx).await?;
    Ok(removed)
}

/// Gives a member one role in a scope, replacing the roles they had there; `None` takes
/// their roles in the scope away.
async fn set_role(
    st: &AdminState,
    p: &Principal,
    org: &str,
    ws: Option<&str>,
    principal: &str,
    role: Option<RoleName>,
) -> ApiResult<StatusCode> {
    if !p.can_admin_org(org) {
        return Err(ApiError::forbidden());
    }
    if principal_of(&st.config.snapshot(), principal).is_none() {
        return Err(ApiError::not_found("member"));
    }
    let scope = match ws {
        Some(w) => RoleScope::Workspace(w.to_owned()),
        None => RoleScope::Organisation(org.to_owned()),
    };
    let doc = role.map(|role| RoleBinding { principal: principal.to_owned(), role, scope: scope.clone() });
    if doc.as_ref().is_some_and(|d| !d.is_consistent()) {
        return Err(ApiError::invalid(
            "this role cannot be given here: workspace admins belong to a workspace, organisation admins to the organisation",
        ));
    }
    change_bindings(st, p, org, ws, |b| b.principal == principal && b.scope == scope, doc).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_org_member(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<MemberRoleInput>,
) -> ApiResult<StatusCode> {
    let scope = resolve_scope(ScopeKind::Organisation, &params, &st.config.snapshot())?;
    let principal = params.get("principal").ok_or_else(|| ApiError::not_found("member"))?;
    set_role(&st, &p, scope.org(), None, principal, input.role).await
}

async fn set_workspace_member(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<MemberRoleInput>,
) -> ApiResult<StatusCode> {
    let scope = resolve_scope(ScopeKind::Workspace, &params, &st.config.snapshot())?;
    let principal = params.get("principal").ok_or_else(|| ApiError::not_found("member"))?;
    set_role(&st, &p, scope.org(), Some(scope.ws()), principal, input.role).await
}

/// Takes a member's roles away: in the organisation and all its workspaces, or in one workspace.
async fn remove_member(
    st: &AdminState,
    p: &Principal,
    org: &str,
    ws: Option<&str>,
    principal: &str,
) -> ApiResult<StatusCode> {
    if !p.can_admin_org(org) {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let removed = change_bindings(
        st,
        p,
        org,
        ws,
        |b| {
            b.principal == principal
                && match ws {
                    Some(w) => b.scope == RoleScope::Workspace(w.to_owned()),
                    None => in_org(&snap, org, &b.scope),
                }
        },
        None,
    )
    .await?;
    if removed == 0 {
        return Err(ApiError::not_found("member"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_org_member(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<StatusCode> {
    let scope = resolve_scope(ScopeKind::Organisation, &params, &st.config.snapshot())?;
    let principal = params.get("principal").ok_or_else(|| ApiError::not_found("member"))?;
    remove_member(&st, &p, scope.org(), None, principal).await
}

async fn remove_workspace_member(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<StatusCode> {
    let scope = resolve_scope(ScopeKind::Workspace, &params, &st.config.snapshot())?;
    let principal = params.get("principal").ok_or_else(|| ApiError::not_found("member"))?;
    remove_member(&st, &p, scope.org(), Some(scope.ws()), principal).await
}

// --- invitations -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InvitationInput {
    pub email: String,
    pub role: RoleName,
    /// A workspace of the organisation, to invite to that workspace only. Unset: the whole
    /// organisation.
    #[serde(default)]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InvitationView {
    pub id: String,
    pub email: String,
    pub role: RoleName,
    pub organisation_id: String,
    pub organisation_name: String,
    pub workspace_id: Option<String>,
    pub workspace_name: Option<String>,
    /// Who sent it: a user's display name or a token's name.
    pub invited_by: String,
    /// The link went to the address by email.
    pub emailed: bool,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub expired: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InvitationList {
    pub items: Vec<InvitationView>,
}

/// An invitation just sent or sent again.
#[derive(Debug, Serialize, JsonSchema)]
pub struct InvitationSent {
    #[serde(flatten)]
    pub invitation: InvitationView,
    /// The token of the link, when it was not emailed: hand on the console's
    /// `/invite?token=…` yourself. It cannot be shown again.
    pub token: Option<String>,
    /// Why the email could not be sent, when the installation tried.
    pub email_error: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InvitationTokenInput {
    /// The token from the invitation link.
    pub token: String,
}

/// What the page an invitation link opens shows before it is accepted.
#[derive(Debug, Serialize, JsonSchema)]
pub struct InvitationPreview {
    pub email: String,
    pub role: RoleName,
    pub organisation_name: String,
    pub workspace_name: Option<String>,
    pub invited_by: String,
    /// An account with this address exists: accepting needs its session.
    pub account_exists: bool,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InvitationAcceptInput {
    /// The token from the invitation link.
    pub token: String,
    /// For a new account; the address when left out.
    #[serde(default)]
    pub display_name: Option<String>,
    /// For a new account: its password.
    #[serde(default)]
    #[schemars(extend("writeOnly" = true))]
    pub password: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InvitationAccepted {
    pub email: String,
    /// A new account was created; it signs in with the password chosen.
    pub account_created: bool,
    /// The new account confirms its address first, with the link just emailed.
    pub needs_confirmation: bool,
    /// Where the role was given.
    pub organisation_id: String,
    pub workspace_id: Option<String>,
}

/// How mails and the console name a role.
fn role_label(role: RoleName) -> &'static str {
    match role {
        RoleName::PlatformAdmin => "platform admin",
        RoleName::OrgAdmin => "organisation admin",
        RoleName::WorkspaceAdmin => "workspace admin",
        RoleName::Auditor => "auditor",
        RoleName::Viewer => "viewer",
    }
}

fn role_of(inv: &Invitation) -> ApiResult<RoleName> {
    serde_json::from_value(serde_json::Value::String(inv.role.clone())).map_err(ApiError::internal)
}

fn scope_of(inv: &Invitation) -> RoleScope {
    match &inv.workspace_id {
        Some(w) => RoleScope::Workspace(w.clone()),
        None => RoleScope::Organisation(inv.organisation_id.clone()),
    }
}

fn org_name(snap: &Snapshot, org: &str) -> String {
    snap.organisations.get(org).map(|o| o.doc.display_name.clone()).unwrap_or_else(|| org.to_owned())
}

fn ws_name(snap: &Snapshot, ws: Option<&str>) -> Option<String> {
    ws.map(|w| snap.workspaces.get(w).map(|x| x.doc.display_name.clone()).unwrap_or_else(|| w.to_owned()))
}

fn view(snap: &Snapshot, inv: &Invitation) -> ApiResult<InvitationView> {
    Ok(InvitationView {
        id: inv.id.clone(),
        email: inv.email.clone(),
        role: role_of(inv)?,
        organisation_id: inv.organisation_id.clone(),
        organisation_name: org_name(snap, &inv.organisation_id),
        workspace_id: inv.workspace_id.clone(),
        workspace_name: ws_name(snap, inv.workspace_id.as_deref()),
        invited_by: principal_name(snap, &inv.invited_by),
        emailed: inv.emailed,
        created_at: inv.created_at,
        expires_at: inv.expires_at,
        expired: inv.expired(),
    })
}

/// An audit entry for something done to an invitation. Invitations are not configuration,
/// so these entries carry no configuration version.
fn audit_of(actor: &str, actor_kind: &str, action: &str, inv: &Invitation) -> AuditEntry {
    AuditEntry {
        actor: actor.to_owned(),
        actor_kind: actor_kind.to_owned(),
        action: action.to_owned(),
        kind: "invitation".into(),
        object_id: inv.id.clone(),
        organisation_id: Some(inv.organisation_id.clone()),
        workspace_id: inv.workspace_id.clone(),
        detail: serde_json::json!({ "email": inv.email, "role": inv.role }),
        ..Default::default()
    }
}

fn audit_by(p: &Principal, action: &str, inv: &Invitation) -> AuditEntry {
    audit_of(&p.id, p.kind.as_str(), action, inv)
}

fn new_token() -> (String, [u8; 32]) {
    let token = URL_SAFE_NO_PAD.encode(random_bytes::<32>());
    let hash = sha256(token.as_bytes());
    (token, hash)
}

/// Emails the link when the installation can. Returns the token when the inviter has to hand
/// it on, and why sending failed.
async fn deliver(
    st: &AdminState,
    snap: &Snapshot,
    inv: &mut Invitation,
    token: String,
) -> (Option<String>, Option<String>) {
    let Some(mailer) = st.mailer.as_ref() else { return (Some(token), None) };
    let place = match &inv.workspace_id {
        Some(_) => format!(
            "the workspace {} of {}",
            ws_name(snap, inv.workspace_id.as_deref()).unwrap_or_default(),
            org_name(snap, &inv.organisation_id)
        ),
        None => format!("the organisation {}", org_name(snap, &inv.organisation_id)),
    };
    let role = role_of(inv).map(role_label).unwrap_or("member");
    let message = mail::invitation(
        &principal_name(snap, &inv.invited_by),
        &place,
        role,
        &mailer.link("/invite", &token),
        snap.settings.invitation_link_days,
    );
    match mailer.send(&inv.email, message).await {
        Ok(()) => {
            if st.store.invitation_mark_emailed(&inv.id).await.is_ok() {
                inv.emailed = true;
            }
            (None, None)
        }
        Err(e) => (Some(token), Some(format!("{e:#}"))),
    }
}

fn expiry(snap: &Snapshot) -> DateTime<Utc> {
    Utc::now() + chrono::Duration::days(i64::from(snap.settings.invitation_link_days.clamp(1, 30)))
}

async fn invite(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<InvitationInput>,
) -> ApiResult<(StatusCode, Json<InvitationSent>)> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Organisation, &params, &snap)?;
    let org = scope.org();
    if !p.can_admin_org(org) {
        return Err(ApiError::forbidden());
    }
    let email = input.email.trim().to_lowercase();
    if !email.contains('@') || email.len() > 254 || email.chars().any(char::is_whitespace) {
        return Err(ApiError::invalid("email must be an email address"));
    }
    let ws = input.workspace_id.as_deref().filter(|w| !w.is_empty());
    if let Some(w) = ws
        && !snap.workspaces.get(w).is_some_and(|x| x.organisation_id == org)
    {
        return Err(ApiError::invalid("workspace_id must be a workspace of this organisation"));
    }
    let role_scope = match ws {
        Some(w) => RoleScope::Workspace(w.to_owned()),
        None => RoleScope::Organisation(org.to_owned()),
    };
    if !(RoleBinding { principal: String::new(), role: input.role, scope: role_scope }).is_consistent() {
        return Err(ApiError::invalid(
            "this role cannot be given here: workspace admins are invited to a workspace, organisation admins to the organisation",
        ));
    }
    let role = serde_json::to_value(input.role).map_err(ApiError::internal)?;
    let (token, hash) = new_token();
    let mut inv = st
        .store
        .invitation_upsert(&NewInvitation {
            id: &new_id("inv"),
            token_hash: &hash,
            email: &email,
            role: role.as_str().unwrap_or_default(),
            organisation_id: org,
            workspace_id: ws,
            invited_by: &p.id,
            expires_at: expiry(&snap),
        })
        .await?;
    let _ = st.store.audit(&audit_by(&p, "invite", &inv)).await;
    let (token, email_error) = deliver(&st, &snap, &mut inv, token).await;
    Ok((StatusCode::CREATED, Json(InvitationSent { invitation: view(&snap, &inv)?, token, email_error })))
}

#[derive(Debug, Deserialize)]
struct InvitationQuery {
    workspace_id: Option<String>,
}

async fn list_invitations(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Query(q): Query<InvitationQuery>,
) -> ApiResult<Json<InvitationList>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Organisation, &params, &snap)?;
    if !p.can_admin_org(scope.org()) {
        return Err(ApiError::forbidden());
    }
    let ws = q.workspace_id.as_deref().filter(|w| !w.is_empty());
    let rows = st.store.invitations_list(scope.org(), ws).await?;
    Ok(Json(InvitationList { items: rows.iter().map(|i| view(&snap, i)).collect::<ApiResult<_>>()? }))
}

/// The invitation `{id}` of the organisation in the path, for one of its admins.
async fn org_invitation(st: &AdminState, p: &Principal, params: &HashMap<String, String>) -> ApiResult<Invitation> {
    let scope = resolve_scope(ScopeKind::Organisation, params, &st.config.snapshot())?;
    if !p.can_admin_org(scope.org()) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("invitation"))?;
    st.store
        .invitation_by_id(id)
        .await?
        .filter(|i| i.organisation_id == scope.org())
        .ok_or_else(|| ApiError::not_found("invitation"))
}

async fn resend_invitation(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<InvitationSent>> {
    let inv = org_invitation(&st, &p, &params).await?;
    let snap = st.config.snapshot();
    let (token, hash) = new_token();
    let mut inv = st
        .store
        .invitation_renew(&inv.id, &hash, &p.id, expiry(&snap))
        .await?
        .ok_or_else(|| ApiError::not_found("invitation"))?;
    let _ = st.store.audit(&audit_by(&p, "resend_invite", &inv)).await;
    let (token, email_error) = deliver(&st, &snap, &mut inv, token).await;
    Ok(Json(InvitationSent { invitation: view(&snap, &inv)?, token, email_error }))
}

async fn revoke_invitation(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<StatusCode> {
    let inv = org_invitation(&st, &p, &params).await?;
    st.store.invitation_delete(&inv.id).await?;
    let _ = st.store.audit(&audit_by(&p, "revoke_invite", &inv)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Gives the invited role to `user`, in the same transaction as anything else in `tx`. It
/// replaces the roles the user had in that scope, as a change of role on the members page
/// does, and like it may not leave the organisation without an organisation admin.
async fn grant(tx: &mut crate::store::Tx, inv: &Invitation, user: &str, audit: AuditEntry) -> ApiResult<()> {
    let doc = RoleBinding { principal: user.to_owned(), role: role_of(inv)?, scope: scope_of(inv) };
    let org_scope = RoleScope::Organisation(inv.organisation_id.clone());
    let is_org_admin = |b: &RoleBinding| b.role == RoleName::OrgAdmin && b.scope == org_scope;
    let mut org_admins = usize::from(is_org_admin(&doc));
    let (mut present, mut removed_admin) = (false, false);
    for row in tx.list(RoleBinding::KIND, None).await? {
        let b: Obj<RoleBinding> = row.into_obj()?;
        if b.doc == doc {
            present = true;
        } else if b.doc.principal == user && b.doc.scope == doc.scope {
            removed_admin |= is_org_admin(&b.doc);
            tx.delete(&b.id, audit.clone()).await?;
        } else if is_org_admin(&b.doc) {
            org_admins += 1;
        }
    }
    if removed_admin && org_admins == 0 {
        return Err(ApiError::conflict(
            "accepting would leave the organisation without an organisation admin; ask another admin first",
        )
        .with_details(serde_json::json!({ "code": "last_org_admin" })));
    }
    if present {
        return Ok(());
    }
    tx.create(
        RoleBinding::KIND,
        &new_id(RoleBinding::PREFIX),
        None,
        <RoleBinding as Resource>::unique_name(&doc).as_deref(),
        &serde_json::to_value(&doc).map_err(ApiError::internal)?,
        audit,
    )
    .await?;
    Ok(())
}

/// Whether the organisation and workspace an invitation is for still exist.
fn scope_exists(snap: &Snapshot, inv: &Invitation) -> bool {
    snap.organisations.contains_key(&inv.organisation_id)
        && inv.workspace_id.as_ref().is_none_or(|w| snap.workspaces.contains_key(w))
}

/// The invitation a link carries, while it can be accepted and its scope still exists.
async fn by_token(st: &AdminState, token: &str) -> ApiResult<Invitation> {
    let inv = st.store.invitation_by_token(&sha256(token.trim().as_bytes())).await?.ok_or_else(invalid_token)?;
    if !scope_exists(&st.config.snapshot(), &inv) {
        return Err(invalid_token());
    }
    Ok(inv)
}

async fn preview_invitation(
    State(st): State<AdminState>,
    Json(input): Json<InvitationTokenInput>,
) -> ApiResult<Json<InvitationPreview>> {
    let inv = by_token(&st, &input.token).await?;
    let snap = st.config.snapshot();
    Ok(Json(InvitationPreview {
        email: inv.email.clone(),
        role: role_of(&inv)?,
        organisation_name: org_name(&snap, &inv.organisation_id),
        workspace_name: ws_name(&snap, inv.workspace_id.as_deref()),
        invited_by: principal_name(&snap, &inv.invited_by),
        account_exists: snap.user_emails.contains_key(&inv.email),
        expires_at: inv.expires_at,
    }))
}

/// The caller, if the request carries valid credentials.
async fn caller(st: &AdminState, headers: &HeaderMap) -> Option<Principal> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))?;
    authenticate(st, token.trim()).await.ok()
}

async fn accept_invitation(
    State(st): State<AdminState>,
    headers: HeaderMap,
    Json(input): Json<InvitationAcceptInput>,
) -> ApiResult<Json<InvitationAccepted>> {
    let inv = by_token(&st, &input.token).await?;
    let snap = st.config.snapshot();

    // Someone with an account accepts while signed in to it.
    if let Some(user_id) = snap.user_emails.get(&inv.email).cloned() {
        let signed_in = caller(&st, &headers).await.is_some_and(|p| p.kind == PrincipalKind::User && p.id == user_id);
        if !signed_in {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "sign_in_required",
                format!("sign in as {} to accept this invitation", inv.email),
            ));
        }
        let mut tx = st.store.begin().await?;
        grant(&mut tx, &inv, &user_id, audit_of(&user_id, "user", "accept_invite", &inv)).await?;
        st.commit(tx).await?;
        st.store.invitation_delete(&inv.id).await?;
        let _ = st.store.audit(&audit_of(&user_id, "user", "accept_invite", &inv)).await;
        return Ok(Json(InvitationAccepted {
            email: inv.email,
            account_created: false,
            needs_confirmation: false,
            organisation_id: inv.organisation_id,
            workspace_id: inv.workspace_id,
        }));
    }

    // Someone new chooses a password. Checked before anything is stored, so that a short
    // password can be corrected.
    let pw = input.password.unwrap_or_default();
    if pw.chars().count() < MIN_PASSWORD_LEN {
        return Err(ApiError::invalid(format!("password must be at least {MIN_PASSWORD_LEN} characters")));
    }
    let hash = tokio::task::spawn_blocking(move || password::hash(&pw))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    let display_name = input.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&inv.email);
    // An emailed link reached the inbox, which confirms the address. A link the inviter
    // handed on does not, where the installation can send the confirmation itself.
    let email_verified = inv.emailed || st.mailer.is_none();
    let user = User {
        email: inv.email.clone(),
        display_name: display_name.to_owned(),
        password_hash: hash,
        status: Status::Active,
        email_verified,
    };
    let user_id = new_id(User::PREFIX);
    let audit = audit_of(&user_id, "user", "accept_invite", &inv);
    let mut tx = st.store.begin().await?;
    let row = tx
        .create(
            User::KIND,
            &user_id,
            None,
            Some(&user.email),
            &serde_json::to_value(&user).map_err(ApiError::internal)?,
            audit.clone(),
        )
        .await?;
    grant(&mut tx, &inv, &user_id, audit.clone()).await?;
    st.commit(tx).await?;
    st.store.invitation_delete(&inv.id).await?;
    let _ = st.store.audit(&audit).await;
    if !email_verified {
        send_in_background(&st, row.into_obj()?, TokenPurpose::VerifyEmail);
    }
    Ok(Json(InvitationAccepted {
        email: inv.email,
        account_created: true,
        needs_confirmation: !email_verified && snap.settings.require_verified_email,
        organisation_id: inv.organisation_id,
        workspace_id: inv.workspace_id,
    }))
}

/// The signed-in user's own invitation `{id}`.
async fn my_invitation(st: &AdminState, p: &Principal, id: &str) -> ApiResult<(Invitation, String)> {
    let snap = st.config.snapshot();
    let user = snap.users.get(&p.id).filter(|_| p.kind == PrincipalKind::User).ok_or_else(ApiError::forbidden)?;
    let inv = st
        .store
        .invitation_by_id(id)
        .await?
        .filter(|i| i.email == user.doc.email && !i.expired() && scope_exists(&snap, i))
        .ok_or_else(|| ApiError::not_found("invitation"))?;
    Ok((inv, user.id.clone()))
}

async fn my_invitations(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<InvitationList>> {
    let snap = st.config.snapshot();
    let Some(user) = snap.users.get(&p.id).filter(|_| p.kind == PrincipalKind::User) else {
        return Ok(Json(InvitationList { items: Vec::new() }));
    };
    let rows = st.store.invitations_for_email(&user.doc.email).await?;
    let live = rows.iter().filter(|i| scope_exists(&snap, i));
    Ok(Json(InvitationList { items: live.map(|i| view(&snap, i)).collect::<ApiResult<_>>()? }))
}

async fn accept_my_invitation(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let (inv, user_id) = my_invitation(&st, &p, &id).await?;
    let mut tx = st.store.begin().await?;
    grant(&mut tx, &inv, &user_id, audit_by(&p, "accept_invite", &inv)).await?;
    st.commit(tx).await?;
    st.store.invitation_delete(&inv.id).await?;
    let _ = st.store.audit(&audit_by(&p, "accept_invite", &inv)).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn decline_my_invitation(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let (inv, _) = my_invitation(&st, &p, &id).await?;
    st.store.invitation_delete(&inv.id).await?;
    let _ = st.store.audit(&audit_by(&p, "decline_invite", &inv)).await;
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let members = api.schema::<MemberList>();
    let role = api.schema::<MemberRoleInput>();
    let inv_input = api.schema::<InvitationInput>();
    let inv_list = api.schema::<InvitationList>();
    let sent = api.schema::<InvitationSent>();
    let token = api.schema::<InvitationTokenInput>();
    let preview = api.schema::<InvitationPreview>();
    let accept = api.schema::<InvitationAcceptInput>();
    let accepted = api.schema::<InvitationAccepted>();

    let org_members = "/orgs/{org}/members";
    let ws_members = "/orgs/{org}/workspaces/{ws}/members";
    api.add(
        api.op("get", org_members, "listOrgMembers", "members", "Members of the organisation and its workspaces")
            .ok(members.clone()),
    );
    api.add(
        api.op(
            "get",
            ws_members,
            "listWorkspaceMembers",
            "members",
            "Members of the workspace, with the organisation's roles that apply to it",
        )
        .ok(members),
    );
    api.add(
        api.op(
            "put",
            &format!("{org_members}/{{principal}}"),
            "setOrgMemberRole",
            "members",
            "Give a member one role in the organisation, replacing the roles they had there, or none",
        )
        .body(role.clone())
        .no_content(),
    );
    api.add(
        api.op(
            "delete",
            &format!("{org_members}/{{principal}}"),
            "removeOrgMember",
            "members",
            "Take every role in the organisation and its workspaces away from a member",
        )
        .no_content(),
    );
    api.add(
        api.op(
            "put",
            &format!("{ws_members}/{{principal}}"),
            "setWorkspaceMemberRole",
            "members",
            "Give a member one role in the workspace, replacing the roles they had there, or none",
        )
        .body(role)
        .no_content(),
    );
    api.add(
        api.op(
            "delete",
            &format!("{ws_members}/{{principal}}"),
            "removeWorkspaceMember",
            "members",
            "Take a member's roles in the workspace away",
        )
        .no_content(),
    );

    let invitations = "/orgs/{org}/invitations";
    api.add(
        api.op(
            "get",
            invitations,
            "listInvitations",
            "invitations",
            "Invitations to the organisation and its workspaces",
        )
        .query("workspace_id", "Only the invitations to this workspace.")
        .ok(inv_list.clone()),
    );
    api.add(
        api.op(
            "post",
            invitations,
            "createInvitation",
            "invitations",
            "Invite someone by email. Without email, the link's token is returned once.",
        )
        .body(inv_input)
        .created(sent.clone()),
    );
    api.add(
        api.op(
            "post",
            &format!("{invitations}/{{id}}/resend"),
            "resendInvitation",
            "invitations",
            "Send the invitation again, with a new link",
        )
        .ok(sent),
    );
    api.add(
        api.op("delete", &format!("{invitations}/{{id}}"), "revokeInvitation", "invitations", "Revoke an invitation")
            .no_content(),
    );
    api.add(
        api.op("post", "/session/invitation/lookup", "previewInvitation", "session", "What an invitation link offers")
            .public()
            .body(token)
            .ok(preview),
    );
    api.add(
        api.op(
            "post",
            "/session/invitation/accept",
            "acceptInvitation",
            "session",
            "Accept an invitation link: signed in to the invited account, or with a password for a new one",
        )
        .public()
        .body(accept)
        .ok(accepted),
    );
    api.add(
        api.op("get", "/session/invitations", "listMyInvitations", "session", "The caller's open invitations")
            .ok(inv_list),
    );
    api.add(
        api.op(
            "post",
            "/session/invitations/{id}/accept",
            "acceptMyInvitation",
            "session",
            "Accept one of the caller's invitations",
        )
        .no_content(),
    );
    api.add(
        api.op(
            "post",
            "/session/invitations/{id}/decline",
            "declineMyInvitation",
            "session",
            "Decline one of the caller's invitations",
        )
        .no_content(),
    );

    router
        .route(org_members, get(list_org_members))
        .route(ws_members, get(list_workspace_members))
        .route(&format!("{org_members}/{{principal}}"), put(set_org_member).delete(remove_org_member))
        .route(&format!("{ws_members}/{{principal}}"), put(set_workspace_member).delete(remove_workspace_member))
        .route(invitations, get(list_invitations).post(invite))
        .route(&format!("{invitations}/{{id}}"), axum::routing::delete(revoke_invitation))
        .route(&format!("{invitations}/{{id}}/resend"), post(resend_invitation))
        .route("/session/invitation/lookup", post(preview_invitation))
        .route("/session/invitation/accept", post(accept_invitation))
        .route("/session/invitations", get(my_invitations))
        .route("/session/invitations/{id}/accept", post(accept_my_invitation))
        .route("/session/invitations/{id}/decline", post(decline_my_invitation))
}
