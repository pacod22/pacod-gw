//! The identity facades (identity concept 13.2): with `PGW_AUTH=oidc`, the console keeps
//! calling the core for users, role bindings and admin tokens, and the core answers from
//! pacod-id's admin API, called with the caller's own token exchanged for one for pacod-id.
//! The gateway's check runs first, with today's answers; pacod-id's rules apply on top.
//!
//! The answers keep the gateway's shapes. What pacod-id has no counterpart for: `version` is 1
//! and is required on replace but not compared; a user's `updated_at` is its `created_at`;
//! role bindings come from the core's copy of the feed (pacod-id lists no one's but the
//! caller's), with their creation time from the id.
//!
//! An admin token is a platform service account with one `pidt_` token; its id is the
//! service account's (`svc_…`).
//!
//! Members and invitations of organisations and workspaces are pacod-id's scope members and
//! invitations at `organisation:<org>` and `gw.workspace:<ws>`. A scope the gateway has but
//! pacod-id does not know yet is still in the identity outbox: it is delivered on the spot and
//! the call made again.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::AdminState;
use super::auth::{Principal, identity_unavailable, role_name};
use super::crud::{Item, ListQuery, Page, Params, ScopeKind, resolve_scope};
use super::error::{ApiError, ApiResult};
use super::members::{
    InvitationInput, InvitationList, InvitationSent, InvitationView, Member, MemberList, MemberRole, MemberRoleInput,
    org_name, role_label, ws_name,
};
use super::resources::access::{AdminTokenInput, AdminTokenView, AdminTokenWithSecret, UserInput, UserView};
use super::session::RoleView;
use crate::model::{Meta, RoleBinding, RoleName, RoleScope, Status};
use crate::snapshot::Snapshot;

/// The routes the facades take over in OIDC mode, with the same paths as the CRUD.
pub fn router(state: AdminState) -> Router {
    Router::new()
        .route("/api/v1/users", get(users_list).post(user_create))
        .route("/api/v1/users/{id}", get(user_read).put(user_replace).delete(user_delete))
        .route("/api/v1/users/{id}/verification", post(user_verification))
        .route("/api/v1/users/{id}/password-reset", post(user_password_reset))
        .route("/api/v1/users/{id}/email-confirmation", post(user_email_confirmation))
        .route("/api/v1/role-bindings", get(bindings_list).post(binding_create))
        .route("/api/v1/role-bindings/{id}", get(binding_read).delete(binding_delete))
        .route("/api/v1/admin-tokens", get(tokens_list).post(token_create))
        .route("/api/v1/admin-tokens/{id}", get(token_read).put(token_replace).delete(token_delete))
        .route("/api/v1/orgs/{org}/members", get(members_list))
        .route("/api/v1/orgs/{org}/members/{principal}", put(member_set).delete(member_remove))
        .route("/api/v1/orgs/{org}/workspaces/{ws}/members", get(members_list))
        .route("/api/v1/orgs/{org}/workspaces/{ws}/members/{principal}", put(member_set).delete(member_remove))
        .route("/api/v1/orgs/{org}/invitations", get(invitations_list).post(invitation_create))
        .route("/api/v1/orgs/{org}/invitations/{id}", delete(invitation_revoke))
        .route("/api/v1/orgs/{org}/invitations/{id}/resend", post(invitation_resend))
        .with_state(state)
}

/// Whether a request path is one of the facades'.
pub fn takes(path: &str) -> bool {
    if ["/api/v1/users", "/api/v1/role-bindings", "/api/v1/admin-tokens"]
        .iter()
        .any(|p| path == *p || path.strip_prefix(p).is_some_and(|rest| rest.starts_with('/')))
    {
        return true;
    }
    // Under an organisation: its members, a workspace's members, its invitations. The
    // organisation and its workspaces themselves stay with the CRUD.
    let Some(rest) = path.strip_prefix("/api/v1/orgs/") else { return false };
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return false;
    }
    matches!(
        segments.as_slice(),
        [_, "members"]
            | [_, "members", _]
            | [_, "workspaces", _, "members"]
            | [_, "workspaces", _, "members", _]
            | [_, "invitations"]
            | [_, "invitations", _]
            | [_, "invitations", _, "resend"]
    )
}

// --- calling pacod-id --------------------------------------------------------------------------

/// What a pacod-id refusal concerns, for the gateway's wording.
#[derive(Clone, Copy)]
enum What {
    User,
    Binding,
    Token,
    Member,
    Invitation,
}

impl What {
    fn not_found(self) -> ApiError {
        ApiError::not_found(match self {
            What::User => "user",
            What::Binding => "role binding",
            What::Token => "admin token",
            What::Member => "member",
            What::Invitation => "invitation",
        })
    }
}

/// pacod-id's refusal in the gateway's words, so that the console shows what it shows today
/// (`ui/src/lib/api/errors.ts` picks by `details.code`, then by type).
fn adjust(e: ApiError, what: What) -> ApiError {
    let details = e.details.clone().unwrap_or(Value::Null);
    let code = details["code"].as_str().unwrap_or_default();
    let role = details["role"].as_str().unwrap_or_default();
    match code {
        "principal_unknown" if matches!(what, What::Binding) => {
            ApiError::invalid("principal must be the id of a user or an admin token")
        }
        "not_found" | "not_a_member" | "principal_unknown" => what.not_found(),
        "not_allowed" => ApiError::forbidden().with_details(json!({ "code": "not_allowed" })),
        "holds_more_rights" => {
            ApiError::new(StatusCode::FORBIDDEN, "forbidden", "this account holds rights you cannot give")
                .with_details(json!({ "code": "holds_more_rights" }))
        }
        "last_holder" if role == "gw.org_admin" => ApiError::conflict(
            "the organisation would have no organisation admin left; make someone else its admin first",
        )
        .with_details(json!({ "code": "last_org_admin" })),
        "last_holder" if role == "gw.platform_admin" => {
            ApiError::conflict("this is the last platform admin; give the role to someone else first")
                .with_details(json!({ "code": "last_holder" }))
        }
        "last_holder" => {
            ApiError::conflict("this is the last identity admin at pacod-id; give the role to someone else first")
                .with_details(json!({ "code": "last_holder" }))
        }
        "email_taken" => ApiError::conflict("a user with this email address already exists here"),
        "binding_exists" => ApiError::conflict("this role binding already exists here"),
        "already_verified" => ApiError::conflict("this user's email address is already confirmed"),
        "account_disabled" => ApiError::conflict("this user is disabled; enable the account first"),
        "scope_unknown" if matches!(what, What::Binding) => {
            ApiError::invalid("the organisation or workspace of the scope does not exist")
        }
        "scope_unknown" => ApiError::not_found("organisation"),
        "invitation_exists" => {
            let message = if role.starts_with("gw.") {
                "an invitation with a role you cannot give is pending for this address here"
            } else {
                "an invitation of another application is pending for this address here"
            };
            ApiError::conflict(message).with_details(json!({ "code": "invitation_exists", "role": role }))
        }
        "bad_email" => ApiError::invalid("email must be an email address"),
        "password_too_short" => ApiError::invalid(format!(
            "password must be at least {} characters",
            details["min_length"].as_i64().unwrap_or(12)
        )),
        "password_too_long" => ApiError::invalid("password must be at most 128 characters"),
        "password_too_common" => ApiError::invalid("password is too common; choose another"),
        "bad_name" => ApiError::invalid("name must be 1 to 100 characters"),
        "bad_expiry" => ApiError::invalid("expires_at must be in the future"),
        "email_unavailable" => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "email_unavailable",
            "this installation does not send email; an administrator can help instead",
        ),
        // pacod-id refused what the core sent it: the core's fault, not the caller's.
        "bad_request" | "bad_scope" | "bad_locale" | "bad_status" | "invalid_token" | "unauthorized" => {
            tracing::error!(%code, message = %e.message, "pacod-id refused a facade's request");
            ApiError::internal("a facade's request was refused by pacod-id")
        }
        _ => e,
    }
}

async fn call(
    st: &AdminState,
    p: &Principal,
    method: Method,
    path: &str,
    body: Option<Value>,
    what: What,
) -> ApiResult<Value> {
    let id = st.identity.as_ref().ok_or_else(|| ApiError::internal("a facade in local mode"))?;
    id.admin(p.bearer()?, method, path, body.as_ref()).await.map_err(|e| adjust(e, what))
}

/// Waits for this instance's copy to have a write's `seq`, when the answer carries one.
async fn written(st: &AdminState, answer: &Value) {
    st.identity_written(answer["seq"].as_i64()).await;
}

fn need_platform_admin(p: &Principal) -> ApiResult<()> {
    if p.is_platform_admin() { Ok(()) } else { Err(ApiError::forbidden()) }
}

fn no_dry_run(dry_run: Option<&str>) -> ApiResult<()> {
    match dry_run {
        Some("true") => {
            Err(ApiError::invalid("this change has no dry run with pacod-id: without dry_run it would be made"))
        }
        _ => Ok(()),
    }
}

fn audit(st: &AdminState, p: &Principal, action: &str, kind: &str, object: &str, detail: Value) {
    audit_in(st, p, None, None, action, kind, object, detail);
}

/// An audit entry in an organisation's (or a workspace's) slice of the log, where its admins
/// see it.
#[allow(clippy::too_many_arguments)]
fn audit_in(
    st: &AdminState,
    p: &Principal,
    org: Option<&str>,
    ws: Option<&str>,
    action: &str,
    kind: &str,
    object: &str,
    detail: Value,
) {
    let mut entry = p.audit(org, ws);
    entry.action = action.into();
    entry.kind = kind.into();
    entry.object_id = object.into();
    entry.detail = detail;
    let store = st.store.clone();
    tokio::spawn(async move {
        let _ = store.audit(&entry).await;
    });
}

fn time(v: &Value) -> DateTime<Utc> {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or_else(Utc::now)
}

fn meta(id: &str, created: DateTime<Utc>) -> Meta {
    Meta { id: id.to_owned(), version: 1, created_at: created, updated_at: created }
}

fn status_of(v: &Value) -> Status {
    if v.as_str() == Some("disabled") { Status::Disabled } else { Status::Active }
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Active => "active",
        Status::Disabled => "disabled",
    }
}

/// An in-memory page over `items` sorted by `key`, the cursor being the last key.
fn page<T>(mut items: Vec<(String, T)>, q: &ListQuery) -> ApiResult<Page<T>> {
    match q.order.as_deref() {
        None | Some("asc") => items.sort_by(|a, b| a.0.cmp(&b.0)),
        Some("desc") => items.sort_by(|a, b| b.0.cmp(&a.0)),
        Some(_) => return Err(ApiError::bad_request("order must be asc or desc")),
    }
    let desc = q.order.as_deref() == Some("desc");
    let start = q.cursor.as_deref().map_or(0, |c| {
        items.iter().position(|(k, _)| if desc { k.as_str() < c } else { k.as_str() > c }).unwrap_or(items.len())
    });
    let limit = usize::try_from(q.limit()).unwrap_or(100);
    let rest: Vec<(String, T)> = items.into_iter().skip(start).collect();
    let more = rest.len() > limit;
    let page: Vec<(String, T)> = rest.into_iter().take(limit).collect();
    let next_cursor = if more { page.last().map(|(k, _)| k.clone()) } else { None };
    Ok(Page { items: page.into_iter().map(|(_, t)| t).collect(), next_cursor })
}

#[derive(Debug, Default, Deserialize)]
struct DryRun {
    dry_run: Option<String>,
}

// --- users -------------------------------------------------------------------------------------

fn user_item(a: &Value) -> Item<UserView> {
    let email = a["email"].as_str().unwrap_or_default().to_owned();
    let name = a["display_name"].as_str().filter(|n| !n.is_empty()).map_or_else(|| email.clone(), str::to_owned);
    Item {
        meta: meta(a["id"].as_str().unwrap_or_default(), time(&a["created_at"])),
        data: UserView {
            email,
            display_name: name,
            status: status_of(&a["status"]),
            email_verified: a["email_verified"].as_bool().unwrap_or(false),
        },
    }
}

async fn users_list(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Page<Item<UserView>>>> {
    need_platform_admin(&p)?;
    if !matches!(q.sort.as_deref(), None | Some("name")) || q.order.as_deref().is_some_and(|o| o != "asc") {
        return Err(ApiError::bad_request("with pacod-id, users are listed by email only"));
    }
    let mut path = format!("/accounts?limit={}", q.limit());
    if let Some(c) = &q.cursor {
        path.push_str(&format!("&after={}", urlencode(c)));
    }
    if let Some(term) = q.q.as_deref().filter(|t| !t.is_empty()) {
        path.push_str(&format!("&q={}", urlencode(term)));
    }
    let answer = call(&st, &p, Method::GET, &path, None, What::User).await?;
    let wanted = q.status.as_deref();
    let items = answer["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| wanted.is_none_or(|w| a["status"] == w))
        .map(user_item)
        .collect();
    Ok(Json(Page { items, next_cursor: answer["next"].as_str().map(str::to_owned) }))
}

async fn user_read(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<Json<Item<UserView>>> {
    need_platform_admin(&p)?;
    let a = call(&st, &p, Method::GET, &format!("/accounts/{}", urlencode(&id)), None, What::User).await?;
    Ok(Json(user_item(&a)))
}

fn check_email(email: &str) -> ApiResult<()> {
    let ok = email.len() <= 254 && email.contains('@') && !email.chars().any(char::is_whitespace);
    if ok { Ok(()) } else { Err(ApiError::invalid("email must be an email address")) }
}

async fn user_create(
    State(st): State<AdminState>,
    p: Principal,
    Json(input): Json<UserInput>,
) -> ApiResult<(StatusCode, Json<Item<UserView>>)> {
    need_platform_admin(&p)?;
    let email = input.email.trim().to_lowercase();
    check_email(&email)?;
    let password =
        input.password.clone().filter(|p| !p.is_empty()).ok_or_else(|| ApiError::invalid("password is required"))?;
    let mut body = json!({ "email": email, "password": password });
    if !input.display_name.trim().is_empty() {
        body["display_name"] = json!(input.display_name.trim());
    }
    let mut account = call(&st, &p, Method::POST, "/accounts", Some(body), What::User).await?;
    let id = account["id"].as_str().unwrap_or_default().to_owned();
    if input.status == Status::Disabled {
        let changed =
            call(&st, &p, Method::PATCH, &format!("/accounts/{id}"), Some(json!({ "status": "disabled" })), What::User)
                .await
                .map_err(|e| ApiError { message: format!("the user was created, but: {}", e.message), ..e })?;
        written(&st, &changed).await;
        account = changed;
    }
    audit(&st, &p, "create", "user", &id, json!({ "email": email }));
    Ok((StatusCode::CREATED, Json(user_item(&account))))
}

#[derive(Debug, Deserialize)]
struct UserReplace {
    #[serde(flatten)]
    input: UserInput,
    version: Option<i32>,
}

async fn user_replace(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
    Query(d): Query<DryRun>,
    Json(body): Json<UserReplace>,
) -> ApiResult<Json<Item<UserView>>> {
    need_platform_admin(&p)?;
    no_dry_run(d.dry_run.as_deref())?;
    if body.version.is_none() {
        return Err(super::crud::version_required());
    }
    let path = format!("/accounts/{}", urlencode(&id));
    let now = call(&st, &p, Method::GET, &path, None, What::User).await?;
    let current = user_item(&now);
    let input = body.input;
    let email = input.email.trim().to_lowercase();
    let mut patch = serde_json::Map::new();
    if email != current.data.email {
        check_email(&email)?;
        patch.insert("email".into(), json!(email));
    }
    let name = if input.display_name.trim().is_empty() { email.clone() } else { input.display_name.trim().to_owned() };
    if name != current.data.display_name {
        patch.insert("display_name".into(), json!(if name == email { String::new() } else { name }));
    }
    if input.status != current.data.status {
        patch.insert("status".into(), json!(status_str(input.status)));
    }
    if let Some(pw) = input.password.filter(|p| !p.is_empty()) {
        patch.insert("password".into(), json!(pw));
    }
    if patch.is_empty() {
        return Ok(Json(current));
    }
    let changed = call(&st, &p, Method::PATCH, &path, Some(Value::Object(patch.clone())), What::User).await?;
    written(&st, &changed).await;
    let fields: Vec<&String> = patch.keys().filter(|k| *k != "password").collect();
    audit(&st, &p, "update", "user", &id, json!({ "changed": fields, "password": patch.contains_key("password") }));
    Ok(Json(user_item(&changed)))
}

async fn user_delete(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
    Query(d): Query<DryRun>,
) -> ApiResult<StatusCode> {
    need_platform_admin(&p)?;
    no_dry_run(d.dry_run.as_deref())?;
    let answer = call(&st, &p, Method::DELETE, &format!("/accounts/{}", urlencode(&id)), None, What::User).await?;
    written(&st, &answer).await;
    audit(&st, &p, "delete", "user", &id, json!({}));
    Ok(StatusCode::NO_CONTENT)
}

async fn account_mail(st: &AdminState, p: &Principal, id: &str, what: &str) -> ApiResult<StatusCode> {
    need_platform_admin(p)?;
    call(st, p, Method::POST, &format!("/accounts/{}/{what}", urlencode(id)), None, What::User).await?;
    audit(st, p, what, "user", id, json!({}));
    Ok(StatusCode::NO_CONTENT)
}

async fn user_verification(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    account_mail(&st, &p, &id, "verification").await
}

async fn user_password_reset(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    account_mail(&st, &p, &id, "password-reset").await
}

async fn user_email_confirmation(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    account_mail(&st, &p, &id, "email-confirmation").await
}

// --- role bindings ------------------------------------------------------------------------------

/// The gateway's view of a pacod-id scope: `organisation:o`, `gw.workspace:w`, `platform`.
fn role_scope(s: &pacod_id_authz::ScopeRef) -> Option<RoleScope> {
    match (s.kind.as_str(), s.id.as_deref()) {
        ("platform", _) => Some(RoleScope::Platform),
        ("organisation", Some(o)) => Some(RoleScope::Organisation(o.to_owned())),
        ("gw.workspace", Some(w)) => Some(RoleScope::Workspace(w.to_owned())),
        _ => None,
    }
}

fn scope_string(s: &RoleScope) -> String {
    match s {
        RoleScope::Platform => "platform".into(),
        RoleScope::Organisation(o) => format!("organisation:{o}"),
        RoleScope::Workspace(w) => format!("gw.workspace:{w}"),
    }
}

fn gw_role(role: &str) -> Option<RoleName> {
    serde_json::from_value(Value::from(role.strip_prefix("gw.")?)).ok()
}

/// When a binding was made, from its id (`rb_<UUIDv7>`): the feed carries no timestamps.
fn created_from_id(id: &str) -> DateTime<Utc> {
    id.rsplit('_')
        .next()
        .and_then(|h| u64::from_str_radix(h.get(..12)?, 16).ok())
        .and_then(|ms| DateTime::from_timestamp_millis(i64::try_from(ms).ok()?))
        .unwrap_or_else(Utc::now)
}

/// The gateway's bindings in the core's copy of the feed, visible to `p` as today.
fn bindings_seen(st: &AdminState, p: &Principal) -> ApiResult<Vec<Item<RoleBinding>>> {
    let id = st.identity.as_ref().ok_or_else(|| ApiError::internal("a facade in local mode"))?;
    let feed = id.cache.snapshot();
    let snap = st.config.snapshot();
    let mut out = Vec::new();
    for (principal, held) in &feed.bindings {
        for h in held {
            let (Some(role), Some(scope)) = (gw_role(&h.role), h.path.last().and_then(role_scope)) else { continue };
            let doc = RoleBinding { principal: principal.clone(), role, scope };
            if doc.principal == p.id || <RoleBinding as super::crud::Resource>::authorize(p, &doc, &snap) {
                out.push(Item { meta: meta(&h.id, created_from_id(&h.id)), data: doc });
            }
        }
    }
    Ok(out)
}

async fn bindings_list(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Page<Item<RoleBinding>>>> {
    let items = bindings_seen(&st, &p)?;
    let keyed = items
        .into_iter()
        .filter(|b| q.q.as_deref().is_none_or(|t| b.data.principal.contains(t)))
        .map(|b| {
            let key = match q.sort.as_deref() {
                Some("name") => format!("{}|{:?}|{}", b.data.principal, b.data.role, scope_string(&b.data.scope)),
                _ => b.meta.id.clone(),
            };
            (key, b)
        })
        .collect();
    Ok(Json(page(keyed, &q)?))
}

async fn binding_read(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<Json<Item<RoleBinding>>> {
    bindings_seen(&st, &p)?.into_iter().find(|b| b.meta.id == id).map(Json).ok_or_else(|| What::Binding.not_found())
}

async fn binding_create(
    State(st): State<AdminState>,
    p: Principal,
    Json(input): Json<RoleBinding>,
) -> ApiResult<(StatusCode, Json<Item<RoleBinding>>)> {
    if !input.is_consistent() {
        return Err(ApiError::invalid("this role cannot be given in this kind of scope"));
    }
    let snap = st.config.snapshot();
    let exists = match &input.scope {
        RoleScope::Platform => true,
        RoleScope::Organisation(o) => snap.organisations.contains_key(o),
        RoleScope::Workspace(w) => snap.workspaces.contains_key(w),
    };
    if !exists {
        return Err(ApiError::invalid("the organisation or workspace of the scope does not exist"));
    }
    if !<RoleBinding as super::crud::Resource>::authorize(&p, &input, &snap) {
        return Err(ApiError::forbidden());
    }
    let body =
        json!({ "principal": input.principal, "role": role_name(input.role), "scope": scope_string(&input.scope) });
    let made = call(&st, &p, Method::POST, "/bindings", Some(body), What::Binding).await?;
    written(&st, &made).await;
    let id = made["id"].as_str().unwrap_or_default();
    audit(
        &st,
        &p,
        "create",
        "role_binding",
        id,
        json!({ "principal": input.principal, "role": role_name(input.role), "scope": scope_string(&input.scope) }),
    );
    Ok((StatusCode::CREATED, Json(Item { meta: meta(id, time(&made["created_at"])), data: input })))
}

async fn binding_delete(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
    Query(d): Query<DryRun>,
) -> ApiResult<StatusCode> {
    no_dry_run(d.dry_run.as_deref())?;
    let snap = st.config.snapshot();
    let binding =
        bindings_seen(&st, &p)?.into_iter().find(|b| b.meta.id == id).ok_or_else(|| What::Binding.not_found())?;
    if !<RoleBinding as super::crud::Resource>::authorize(&p, &binding.data, &snap) {
        return Err(ApiError::forbidden());
    }
    let answer = call(&st, &p, Method::DELETE, &format!("/bindings/{}", urlencode(&id)), None, What::Binding).await?;
    written(&st, &answer).await;
    audit(&st, &p, "delete", "role_binding", &id, json!({ "principal": binding.data.principal }));
    Ok(StatusCode::NO_CONTENT)
}

// --- admin tokens -------------------------------------------------------------------------------

/// An admin token from its service account and the account's tokens: the newest active one,
/// else the newest.
fn token_item(sa: &Value, tokens: &[Value]) -> Item<AdminTokenView> {
    let newest = |active: bool| {
        tokens
            .iter()
            .filter(|t| !active || t["status"] == "active")
            .max_by(|a, b| a["created_at"].as_str().cmp(&b["created_at"].as_str()))
    };
    let token = newest(true).or_else(|| newest(false));
    Item {
        meta: meta(sa["id"].as_str().unwrap_or_default(), time(&sa["created_at"])),
        data: AdminTokenView {
            name: sa["name"].as_str().unwrap_or_default().to_owned(),
            hint: token.and_then(|t| t["hint"].as_str()).unwrap_or_default().to_owned(),
            expires_at: token.and_then(|t| t["expires_at"].as_str()).and_then(|s| s.parse().ok()),
            status: status_of(&sa["status"]),
        },
    }
}

async fn tokens_of(st: &AdminState, p: &Principal, sa: &str) -> ApiResult<Vec<Value>> {
    let answer =
        call(st, p, Method::GET, &format!("/service-accounts/{}/tokens", urlencode(sa)), None, What::Token).await?;
    Ok(answer["tokens"].as_array().cloned().unwrap_or_default())
}

async fn platform_accounts(st: &AdminState, p: &Principal) -> ApiResult<Vec<Value>> {
    let answer = call(st, p, Method::GET, "/service-accounts", None, What::Token).await?;
    Ok(answer["service_accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["organisation_id"].is_null())
        .cloned()
        .collect())
}

async fn tokens_list(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Page<Item<AdminTokenView>>>> {
    need_platform_admin(&p)?;
    let mut keyed = Vec::new();
    for sa in platform_accounts(&st, &p).await? {
        let item = token_item(&sa, &tokens_of(&st, &p, sa["id"].as_str().unwrap_or_default()).await?);
        if q.q.as_deref().is_some_and(|t| !item.data.name.to_lowercase().contains(&t.to_lowercase()))
            || q.status.as_deref().is_some_and(|s| s != status_str(item.data.status))
        {
            continue;
        }
        let key = match q.sort.as_deref() {
            Some("name") => format!("{}|{}", item.data.name.to_lowercase(), item.meta.id),
            _ => item.meta.id.clone(),
        };
        keyed.push((key, item));
    }
    Ok(Json(page(keyed, &q)?))
}

async fn token_read(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<Json<Item<AdminTokenView>>> {
    need_platform_admin(&p)?;
    let sa = call(&st, &p, Method::GET, &format!("/service-accounts/{}", urlencode(&id)), None, What::Token).await?;
    if !sa["organisation_id"].is_null() {
        return Err(What::Token.not_found());
    }
    Ok(Json(token_item(&sa, &tokens_of(&st, &p, &id).await?)))
}

fn check_token_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError::invalid("name must be 1 to 100 characters"));
    }
    Ok(name.to_owned())
}

async fn token_create(
    State(st): State<AdminState>,
    p: Principal,
    Json(input): Json<AdminTokenInput>,
) -> ApiResult<(StatusCode, Json<AdminTokenWithSecret>)> {
    need_platform_admin(&p)?;
    let name = check_token_name(&input.name)?;
    if input.expires_at.is_some_and(|e| e <= Utc::now()) {
        return Err(ApiError::invalid("expires_at must be in the future"));
    }
    if platform_accounts(&st, &p)
        .await?
        .iter()
        .any(|s| s["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(&name)))
    {
        return Err(ApiError::conflict(format!("an admin token named \"{name}\" already exists here")));
    }
    let sa = call(&st, &p, Method::POST, "/service-accounts", Some(json!({ "name": name })), What::Token).await?;
    let svc = sa["id"].as_str().unwrap_or_default().to_owned();
    let body = match input.expires_at {
        Some(e) => json!({ "expires_at": e }),
        None => json!({}),
    };
    let made =
        match call(&st, &p, Method::POST, &format!("/service-accounts/{svc}/tokens"), Some(body), What::Token).await {
            Ok(t) => t,
            Err(e) => {
                // No service account without its token.
                let _ = call(&st, &p, Method::DELETE, &format!("/service-accounts/{svc}"), None, What::Token).await;
                return Err(e);
            }
        };
    let secret = made["token"].as_str().unwrap_or_default().to_owned();
    audit(&st, &p, "create", "admin_token", &svc, json!({ "name": name }));
    let token = token_item(
        &sa,
        std::slice::from_ref(
            &json!({ "status": "active", "hint": made["hint"], "expires_at": made["expires_at"], "created_at": made["created_at"] }),
        ),
    );
    Ok((StatusCode::CREATED, Json(AdminTokenWithSecret { token, secret })))
}

#[derive(Debug, Deserialize)]
struct TokenReplace {
    #[serde(flatten)]
    input: AdminTokenInput,
    version: Option<i32>,
}

async fn token_replace(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
    Query(d): Query<DryRun>,
    Json(body): Json<TokenReplace>,
) -> ApiResult<Json<Item<AdminTokenView>>> {
    need_platform_admin(&p)?;
    no_dry_run(d.dry_run.as_deref())?;
    if body.version.is_none() {
        return Err(super::crud::version_required());
    }
    let path = format!("/service-accounts/{}", urlencode(&id));
    let sa = call(&st, &p, Method::GET, &path, None, What::Token).await?;
    if !sa["organisation_id"].is_null() {
        return Err(What::Token.not_found());
    }
    let current = token_item(&sa, &tokens_of(&st, &p, &id).await?);
    // The expiry belongs to the token, which pacod-id does not change: compared to the minute,
    // as the console shows it.
    let minute = |t: Option<DateTime<Utc>>| t.map(|t| t.timestamp() / 60);
    if minute(body.input.expires_at) != minute(current.data.expires_at) {
        return Err(ApiError::invalid("expires_at cannot be changed; create a new token"));
    }
    let name = check_token_name(&body.input.name)?;
    let mut patch = serde_json::Map::new();
    if name != current.data.name {
        patch.insert("name".into(), json!(name));
    }
    if body.input.status != current.data.status {
        patch.insert("status".into(), json!(status_str(body.input.status)));
    }
    if patch.is_empty() {
        return Ok(Json(current));
    }
    let changed = call(&st, &p, Method::PATCH, &path, Some(Value::Object(patch.clone())), What::Token).await?;
    written(&st, &changed).await;
    audit(&st, &p, "update", "admin_token", &id, json!({ "changed": patch.keys().collect::<Vec<_>>() }));
    Ok(Json(Item { data: AdminTokenView { name, status: body.input.status, ..current.data }, meta: current.meta }))
}

async fn token_delete(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
    Query(d): Query<DryRun>,
) -> ApiResult<StatusCode> {
    need_platform_admin(&p)?;
    no_dry_run(d.dry_run.as_deref())?;
    let path = format!("/service-accounts/{}", urlencode(&id));
    let sa = call(&st, &p, Method::GET, &path, None, What::Token).await?;
    if !sa["organisation_id"].is_null() {
        return Err(What::Token.not_found());
    }
    let answer = call(&st, &p, Method::DELETE, &path, None, What::Token).await?;
    written(&st, &answer).await;
    audit(&st, &p, "delete", "admin_token", &id, json!({}));
    Ok(StatusCode::NO_CONTENT)
}

// --- members and invitations ----------------------------------------------------------------------

/// A call about a scope the gateway has (`target`: `organisation:<org>`, `gw.workspace:<ws>`).
/// pacod-id not knowing it means its registration is still in the identity outbox: that is
/// delivered now (or by the deliverer already at it, for up to three seconds) and the call
/// made once more. Still waiting after that: pacod-id is not taking it, 503.
async fn call_at(
    st: &AdminState,
    p: &Principal,
    method: Method,
    path: &str,
    body: Option<Value>,
    what: What,
    target: &str,
) -> ApiResult<Value> {
    let id = st.identity.as_ref().ok_or_else(|| ApiError::internal("a facade in local mode"))?;
    let unknown = |e: &ApiError| e.details.as_ref().is_some_and(|d| d["code"] == "scope_unknown");
    match id.admin(p.bearer()?, method.clone(), path, body.as_ref()).await {
        Err(e) if unknown(&e) => {}
        r => return r.map_err(|e| adjust(e, what)),
    }
    let mut pending = true;
    for _ in 0..30 {
        pending = st.store.idp_targets().await.map(|t| t.contains(target)).unwrap_or(false);
        if !pending {
            break;
        }
        if id.deliver_outbox(&st.store).await == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    match id.admin(p.bearer()?, method, path, body.as_ref()).await {
        Err(e) if unknown(&e) && pending => {
            tracing::warn!(%target, "pacod-id does not know a scope whose registration is still waiting");
            Err(identity_unavailable())
        }
        Err(e) if unknown(&e) => {
            Err(ApiError::not_found(if target.starts_with("organisation:") { "organisation" } else { "workspace" }))
        }
        r => r.map_err(|e| adjust(e, what)),
    }
}

/// The scope in a members or invitations path: the organisation, or one of its workspaces.
struct At {
    org: String,
    ws: Option<String>,
}

impl At {
    fn of(st: &AdminState, params: &std::collections::HashMap<String, String>) -> ApiResult<At> {
        let kind = if params.contains_key("ws") { ScopeKind::Workspace } else { ScopeKind::Organisation };
        let scope = resolve_scope(kind, params, &st.config.snapshot())?;
        Ok(At { org: scope.org().to_owned(), ws: scope.ws.clone() })
    }

    fn scope(&self) -> RoleScope {
        match &self.ws {
            Some(w) => RoleScope::Workspace(w.clone()),
            None => RoleScope::Organisation(self.org.clone()),
        }
    }

    /// pacod-id's members path of the scope.
    fn members(&self) -> String {
        match &self.ws {
            Some(w) => format!("/scopes/gw.workspace/{}/members", urlencode(w)),
            None => format!("/scopes/organisation/{}/members", urlencode(&self.org)),
        }
    }
}

/// A pacod-id scope member as the gateway shows one; `None` without a gateway role.
fn member(snap: &Snapshot, m: &Value) -> Option<Member> {
    let mut roles: Vec<MemberRole> = m["roles"]
        .as_array()?
        .iter()
        .filter_map(|r| {
            let role = gw_role(r["role"].as_str()?)?;
            let scope = role_scope(&r["scope"].as_str()?.parse().ok()?)?;
            Some(MemberRole {
                binding_id: r["binding"].as_str()?.to_owned(),
                role,
                scope_name: RoleView::new(snap, role, &scope).scope_name,
                scope,
                inherited: r["inherited"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    if roles.is_empty() {
        return None;
    }
    // The organisation's roles first, then the workspaces' by name.
    roles.sort_by_key(|r| (!matches!(r.scope, RoleScope::Organisation(_)), r.scope_name.clone()));
    let email = m["email"].as_str().map(str::to_owned);
    Some(Member {
        principal_id: m["principal"].as_str()?.to_owned(),
        kind: if m["principal_kind"] == "service" { "token" } else { "user" }.into(),
        name: m["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .or_else(|| email.clone())
            .unwrap_or_default(),
        email,
        status: status_of(&m["status"]),
        roles,
    })
}

async fn members_list(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<MemberList>> {
    let at = At::of(&st, &params)?;
    let allowed = match &at.ws {
        Some(w) => p.can_read_workspace(w, &at.org),
        None => p.can_read_org(&at.org),
    };
    if !allowed {
        return Err(ApiError::forbidden());
    }
    let answer = call_at(&st, &p, Method::GET, &at.members(), None, What::Member, &scope_string(&at.scope())).await?;
    let snap = st.config.snapshot();
    let mut items: Vec<Member> =
        answer["members"].as_array().into_iter().flatten().filter_map(|m| member(&snap, m)).collect();
    items.sort_by_key(|m| (m.name.to_lowercase(), m.principal_id.clone()));
    Ok(Json(MemberList { items }))
}

async fn member_set(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<MemberRoleInput>,
) -> ApiResult<StatusCode> {
    let at = At::of(&st, &params)?;
    if !p.can_admin_org(&at.org) {
        return Err(ApiError::forbidden());
    }
    let principal = params.get("principal").ok_or_else(|| What::Member.not_found())?;
    let scope = at.scope();
    if input
        .role
        .is_some_and(|role| !(RoleBinding { principal: principal.clone(), role, scope: scope.clone() }).is_consistent())
    {
        return Err(ApiError::invalid(
            "this role cannot be given here: workspace admins belong to a workspace, organisation admins to the organisation",
        ));
    }
    let path = format!("{}/{}", at.members(), urlencode(principal));
    let body = json!({ "role": input.role.map(role_name) });
    let answer = call_at(&st, &p, Method::PUT, &path, Some(body), What::Member, &scope_string(&scope)).await?;
    written(&st, &answer).await;
    audit_in(
        &st,
        &p,
        Some(&at.org),
        at.ws.as_deref(),
        "set_member_role",
        "member",
        principal,
        json!({ "role": input.role.map(role_name), "scope": scope_string(&scope) }),
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn member_remove(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<StatusCode> {
    let at = At::of(&st, &params)?;
    if !p.can_admin_org(&at.org) {
        return Err(ApiError::forbidden());
    }
    let principal = params.get("principal").ok_or_else(|| What::Member.not_found())?;
    // In the organisation: there and in all its workspaces. In a workspace: there only.
    let mut path = format!("{}/{}", at.members(), urlencode(principal));
    if at.ws.is_some() {
        path.push_str("?below=false");
    }
    let answer = call_at(&st, &p, Method::DELETE, &path, None, What::Member, &scope_string(&at.scope())).await?;
    written(&st, &answer).await;
    audit_in(
        &st,
        &p,
        Some(&at.org),
        at.ws.as_deref(),
        "remove_member",
        "member",
        principal,
        json!({ "scope": scope_string(&at.scope()) }),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// A pacod-id invitation as the gateway shows one; `None` for another product's.
fn invitation(snap: &Snapshot, org: &str, v: &Value) -> Option<InvitationView> {
    let role = gw_role(v["role"].as_str()?)?;
    let scope: pacod_id_authz::ScopeRef = v["scope"].as_str()?.parse().ok()?;
    let workspace_id = match role_scope(&scope)? {
        RoleScope::Workspace(w) => Some(w),
        _ => None,
    };
    let inviter = v["inviter_name"].as_str().filter(|n| !n.is_empty()).or_else(|| v["invited_by"].as_str());
    Some(InvitationView {
        id: v["id"].as_str()?.to_owned(),
        email: v["email"].as_str()?.to_owned(),
        role,
        organisation_id: org.to_owned(),
        organisation_name: org_name(snap, org),
        workspace_name: ws_name(snap, workspace_id.as_deref()),
        workspace_id,
        invited_by: inviter.unwrap_or_default().to_owned(),
        emailed: v["emailed"].as_bool().unwrap_or(false),
        created_at: time(&v["created_at"]),
        expires_at: time(&v["expires_at"]),
        expired: v["expired"].as_bool().unwrap_or(false),
    })
}

fn sent(snap: &Snapshot, org: &str, v: &Value) -> ApiResult<InvitationSent> {
    let view = invitation(snap, org, v)
        .ok_or_else(|| ApiError::internal("pacod-id answered an invitation of another kind"))?;
    // Mail is queued at pacod-id: a sending failure never shows here.
    Ok(InvitationSent { invitation: view, token: v["token"].as_str().map(str::to_owned), email_error: None })
}

#[derive(Debug, Deserialize)]
struct InvitationQuery {
    workspace_id: Option<String>,
}

/// The organisation's invitations at pacod-id, its workspaces' included.
async fn org_invitations(st: &AdminState, p: &Principal, org: &str) -> ApiResult<Vec<Value>> {
    let target = format!("organisation:{org}");
    let answer = call_at(
        st,
        p,
        Method::GET,
        &format!("/invitations?scope={}", urlencode(&target)),
        None,
        What::Invitation,
        &target,
    )
    .await?;
    Ok(answer["invitations"].as_array().cloned().unwrap_or_default())
}

async fn invitations_list(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Query(q): Query<InvitationQuery>,
) -> ApiResult<Json<InvitationList>> {
    let at = At::of(&st, &params)?;
    if !p.can_admin_org(&at.org) {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let rows = match q.workspace_id.as_deref().filter(|w| !w.is_empty()) {
        Some(w) if !snap.workspaces.get(w).is_some_and(|x| x.organisation_id == at.org) => Vec::new(),
        Some(w) => {
            let target = format!("gw.workspace:{w}");
            let path = format!("/invitations?scope={}&below=false", urlencode(&target));
            let answer = call_at(&st, &p, Method::GET, &path, None, What::Invitation, &target).await?;
            answer["invitations"].as_array().cloned().unwrap_or_default()
        }
        None => org_invitations(&st, &p, &at.org).await?,
    };
    let mut items: Vec<InvitationView> = rows.iter().filter_map(|v| invitation(&snap, &at.org, v)).collect();
    items.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    Ok(Json(InvitationList { items }))
}

async fn invitation_create(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<InvitationInput>,
) -> ApiResult<(StatusCode, Json<InvitationSent>)> {
    let at = At::of(&st, &params)?;
    let org = at.org.as_str();
    if !p.can_admin_org(org) {
        return Err(ApiError::forbidden());
    }
    let email = input.email.trim().to_lowercase();
    if !email.contains('@') || email.len() > 254 || email.chars().any(char::is_whitespace) {
        return Err(ApiError::invalid("email must be an email address"));
    }
    let snap = st.config.snapshot();
    let ws = input.workspace_id.as_deref().filter(|w| !w.is_empty());
    if let Some(w) = ws
        && !snap.workspaces.get(w).is_some_and(|x| x.organisation_id == org)
    {
        return Err(ApiError::invalid("workspace_id must be a workspace of this organisation"));
    }
    let scope = match ws {
        Some(w) => RoleScope::Workspace(w.to_owned()),
        None => RoleScope::Organisation(org.to_owned()),
    };
    if !(RoleBinding { principal: String::new(), role: input.role, scope: scope.clone() }).is_consistent() {
        return Err(ApiError::invalid(
            "this role cannot be given here: workspace admins are invited to a workspace, organisation admins to the organisation",
        ));
    }
    // How the mail and pacod-id's page name the role and the place: the place by its names
    // only, as they read the same in every language.
    let place = match ws {
        Some(_) => format!("{} ({})", ws_name(&snap, ws).unwrap_or_default(), org_name(&snap, org)),
        None => org_name(&snap, org),
    };
    let target = scope_string(&scope);
    let body = json!({
        "email": email, "role": role_name(input.role), "scope": target,
        "labels": { "role": role_label(input.role), "scope": place },
    });
    let made = call_at(&st, &p, Method::POST, "/invitations", Some(body), What::Invitation, &target).await?;
    let out = sent(&snap, org, &made)?;
    audit_in(
        &st,
        &p,
        Some(org),
        ws,
        "invite",
        "invitation",
        &out.invitation.id,
        json!({ "email": email, "role": role_name(input.role) }),
    );
    Ok((StatusCode::CREATED, Json(out)))
}

/// The invitation `{id}` of the organisation in the path, for one of its admins.
async fn org_invitation(
    st: &AdminState,
    p: &Principal,
    params: &std::collections::HashMap<String, String>,
) -> ApiResult<(At, Value)> {
    let at = At::of(st, params)?;
    if !p.can_admin_org(&at.org) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| What::Invitation.not_found())?;
    let found = org_invitations(st, p, &at.org).await?.into_iter().find(|v| v["id"] == id.as_str());
    Ok((at, found.ok_or_else(|| What::Invitation.not_found())?))
}

async fn invitation_resend(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<InvitationSent>> {
    let (at, inv) = org_invitation(&st, &p, &params).await?;
    let id = inv["id"].as_str().unwrap_or_default();
    let path = format!("/invitations/{}/resend", urlencode(id));
    let again = call(&st, &p, Method::POST, &path, None, What::Invitation).await?;
    let out = sent(&st.config.snapshot(), &at.org, &again)?;
    audit_in(
        &st,
        &p,
        Some(&at.org),
        out.invitation.workspace_id.as_deref(),
        "resend_invite",
        "invitation",
        id,
        json!({ "email": out.invitation.email, "role": inv["role"] }),
    );
    Ok(Json(out))
}

async fn invitation_revoke(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<StatusCode> {
    let (at, inv) = org_invitation(&st, &p, &params).await?;
    let id = inv["id"].as_str().unwrap_or_default();
    call(&st, &p, Method::DELETE, &format!("/invitations/{}", urlencode(id)), None, What::Invitation).await?;
    let ws = invitation(&st.config.snapshot(), &at.org, &inv).and_then(|v| v.workspace_id);
    audit_in(
        &st,
        &p,
        Some(&at.org),
        ws.as_deref(),
        "revoke_invite",
        "invitation",
        id,
        json!({ "email": inv["email"], "role": inv["role"] }),
    );
    Ok(StatusCode::NO_CONTENT)
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Hands the facades' paths to their router (built once, in OIDC mode only); everything else
/// passes on.
pub async fn intercept(
    facades: Router,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use tower::ServiceExt;
    if !takes(req.uri().path()) {
        return next.run(req).await;
    }
    // A request of its own: the outer router's route and path parameters stay behind (they
    // would be read as the facade's), the client address comes along.
    let (parts, body) = req.into_parts();
    let mut fresh = axum::http::Request::builder().method(parts.method).uri(parts.uri).version(parts.version);
    if let Some(h) = fresh.headers_mut() {
        *h = parts.headers;
    }
    let mut fresh = fresh.body(body).expect("the parts of a valid request");
    if let Some(addr) = parts.extensions.get::<axum::extract::ConnectInfo<std::net::SocketAddr>>() {
        fresh.extensions_mut().insert(*addr);
    }
    match facades.oneshot(fresh).await {
        Ok(r) => r,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::takes;

    #[test]
    fn the_facades_take_their_paths_and_leave_the_rest() {
        for (path, taken) in [
            ("/api/v1/users", true),
            ("/api/v1/users/usr_1/verification", true),
            ("/api/v1/usersx", false),
            ("/api/v1/role-bindings/rb_1", true),
            ("/api/v1/admin-tokens", true),
            ("/api/v1/orgs", false),
            ("/api/v1/orgs/org_1", false),
            ("/api/v1/orgs/org_1/members", true),
            ("/api/v1/orgs/org_1/members/usr_1", true),
            ("/api/v1/orgs/org_1/members/usr_1/x", false),
            ("/api/v1/orgs/org_1/workspaces", false),
            ("/api/v1/orgs/org_1/workspaces/ws_1", false),
            ("/api/v1/orgs/org_1/workspaces/ws_1/members", true),
            ("/api/v1/orgs/org_1/workspaces/ws_1/members/svc_1", true),
            ("/api/v1/orgs/org_1/workspaces/ws_1/keys", false),
            ("/api/v1/orgs/org_1/invitations", true),
            ("/api/v1/orgs/org_1/invitations/inv_1", true),
            ("/api/v1/orgs/org_1/invitations/inv_1/resend", true),
            ("/api/v1/orgs/org_1/invitations/inv_1/other", false),
            ("/api/v1/orgs//members", false),
            ("/api/v1/orgs/org_1/limits", false),
            ("/api/v1/session/invitations", false),
        ] {
            assert_eq!(takes(path), taken, "{path}");
        }
    }
}
