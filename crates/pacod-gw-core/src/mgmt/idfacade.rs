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

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::AdminState;
use super::auth::{Principal, role_name};
use super::crud::{Item, ListQuery, Page};
use super::error::{ApiError, ApiResult};
use super::resources::access::{AdminTokenInput, AdminTokenView, AdminTokenWithSecret, UserInput, UserView};
use crate::model::{Meta, RoleBinding, RoleName, RoleScope, Status};

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
        .with_state(state)
}

/// Whether a request path is one of the facades'.
pub fn takes(path: &str) -> bool {
    ["/api/v1/users", "/api/v1/role-bindings", "/api/v1/admin-tokens"]
        .iter()
        .any(|p| path == *p || path.strip_prefix(p).is_some_and(|rest| rest.starts_with('/')))
}

// --- calling pacod-id --------------------------------------------------------------------------

/// What a pacod-id refusal concerns, for the gateway's wording.
#[derive(Clone, Copy)]
enum What {
    User,
    Binding,
    Token,
}

impl What {
    fn not_found(self) -> ApiError {
        ApiError::not_found(match self {
            What::User => "user",
            What::Binding => "role binding",
            What::Token => "admin token",
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
        "not_found" | "principal_unknown" if matches!(what, What::Binding) && code == "principal_unknown" => {
            ApiError::invalid("principal must be the id of a user or an admin token")
        }
        "not_found" | "not_a_member" => what.not_found(),
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
        "scope_unknown" => ApiError::invalid("the organisation or workspace of the scope does not exist"),
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
    let mut entry = p.audit(None, None);
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
