//! The signed-in person's own account (the `/account` pages): profile, password,
//! address, sessions, organisations, and deleting the account. The session cookie says who; writes
//! come from the realm's own pages only. Changing the address and deleting the account need a
//! recent sign-in (the password again, or a sign-in in the last ten minutes).

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use pacod_id_authz::{ScopeRef, Subject};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::authz::bindings::subject_bindings;
use crate::config::Realm;
use crate::crypto::{sha256, tokens};
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::mail::{self, Template};
use crate::oidc::flows::{hash_password, same_origin, verify_password};
use crate::oidc::{SID, browser_session, clear_cookie, realm_by_host};
use crate::store::authz as db;
use crate::store::{Account, AuditEntry, Session, StoreError, changes};
use crate::{oidc, policy};

/// How long a sign-in counts as recent for the account's most sensitive changes.
pub(crate) const RECENT: Duration = Duration::minutes(10);
/// Wrong passwords on the account pages before a 15-minute pause.
const TRIES: i32 = 10;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/ui-api/v1/realm", get(realm_info))
        .route("/ui-api/v1/account", get(read).patch(update).delete(remove))
        .route("/ui-api/v1/account/reauth", post(reauth))
        .route("/ui-api/v1/account/sign-in", post(start_sign_in))
        .route("/ui-api/v1/account/password", post(set_password))
        .route("/ui-api/v1/account/email", post(change_email))
        .route("/ui-api/v1/account/sessions", get(sessions))
        .route("/ui-api/v1/account/sessions/others", post(end_others))
        .route("/ui-api/v1/account/sessions/{sid}", delete(end_one))
        .route("/ui-api/v1/account/organisations", get(organisations))
}

struct Me<'a> {
    realm: &'a Realm,
    session: Session,
    account: Account,
}

/// The signed-in person of this realm; writes only from the realm's own pages.
async fn me<'a>(st: &'a Shared, headers: &HeaderMap, write: bool) -> ApiResult<Me<'a>> {
    let realm = realm_by_host(st, headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    if write {
        same_origin(realm, headers)?;
    }
    let (session, account) = browser_session(st, realm, headers)
        .await?
        .ok_or_else(|| ApiError::unauthorized("sign in first").with_details(json!({ "code": "sign_in_required" })))?;
    Ok(Me { realm, session, account })
}

fn recent(s: &Session) -> bool {
    s.auth_time > Utc::now() - RECENT
}

fn need_recent(s: &Session) -> ApiResult<()> {
    if recent(s) {
        Ok(())
    } else {
        Err(ApiError::forbidden("enter your password again first").with_details(json!({ "code": "reauth_required" })))
    }
}

fn audit(m: &Me<'_>, action: &str) -> AuditEntry {
    AuditEntry {
        realm: Some(m.realm.name.clone()),
        ..AuditEntry::new(action).by(&m.account.id, "user").subject(&m.account.id)
    }
}

/// Checks the account's password, with its own counter against guessing.
async fn check_password(st: &Shared, account: &Account, password: &str) -> ApiResult<()> {
    if st.store.throttle_locked("account_password", &account.id).await?.is_some() {
        return Err(ApiError::too_many("too many attempts; try again in 15 minutes")
            .with_details(json!({ "code": "too_many_attempts" })));
    }
    // Counted before the hashing, so that parallel attempts cannot all pass the check above.
    if !st.store.throttle_take("account_password", &account.id, 15 * 60, TRIES, 15 * 60).await? {
        return Err(ApiError::too_many("too many attempts; try again in 15 minutes")
            .with_details(json!({ "code": "too_many_attempts" })));
    }
    let stored = account.password_hash.clone().unwrap_or_else(|| st.dummy_hash.clone());
    let ok = verify_password(st, password, &stored).await?.ok && account.password_hash.is_some();
    if !ok {
        return Err(
            ApiError::unauthorized("the password is wrong").with_details(json!({ "code": "invalid_credentials" }))
        );
    }
    st.store.throttle_clear("account_password", &account.id).await?;
    Ok(())
}

#[derive(Deserialize)]
struct StartSignIn {
    /// The page to come back to: a path of this realm's pages.
    #[serde(default)]
    return_to: Option<String>,
    /// Start with "create an account".
    #[serde(default)]
    create: bool,
    #[serde(default)]
    login_hint: Option<String>,
}

/// A sign-in for the realm's own pages (the account, the admin pages, an invitation): a flow
/// without a client, which ends on the page it started from instead of with a code.
async fn start_sign_in(
    State(st): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<StartSignIn>,
) -> ApiResult<Response> {
    let realm = realm_by_host(&st, &headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    same_origin(realm, &headers)?;
    let back = body.return_to.unwrap_or_else(|| "/account".into());
    // Only a path of these pages: no scheme, no other host.
    if !back.starts_with('/')
        || back.starts_with("//")
        || back.contains('\\')
        || back.starts_with("/ui-api/")
        || back.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(
            ApiError::invalid("return_to is a path of these pages").with_details(json!({ "code": "bad_return_to" }))
        );
    }
    let request = oidc::Request {
        redirect_uri: back,
        scope: String::new(),
        state: None,
        nonce: None,
        code_challenge: String::new(),
        prompt: vec![],
        max_age: None,
        login_hint: body.login_hint,
        ui_locales: None,
        remember_me: false,
    };
    let flow_id = tokens::new_id("flw");
    let browser = tokens::opaque();
    st.store
        .flow_create(
            &flow_id,
            &realm.name,
            "",
            &serde_json::to_value(&request).map_err(ApiError::internal)?,
            &sha256(browser.as_bytes()),
            "start",
            None,
            Utc::now() + oidc::authorize::FLOW_LIFETIME,
        )
        .await?;
    let page = if body.create { "signup" } else { "login" };
    let mut r =
        Json(json!({ "next": "redirect", "to": format!("{}/{page}?flow={flow_id}", realm.issuer) })).into_response();
    r.headers_mut().append(
        header::SET_COOKIE,
        oidc::set_cookie(realm, oidc::FLOW, &browser, Some(oidc::authorize::FLOW_LIFETIME.num_seconds())),
    );
    Ok(r)
}

/// What the hosted pages need to know about the realm before anyone signs in.
async fn realm_info(State(st): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    let realm = realm_by_host(&st, &headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    Ok(Json(json!({
        "name": realm.name,
        "issuer": realm.issuer,
        "brand": realm.brand,
        "locales": realm.locales,
        "default_locale": realm.default_locale,
        "self_signup": realm.self_signup,
        "google": realm.google.is_some(),
        "terms_url": realm.terms_url,
        "privacy_url": realm.privacy_url,
        "password_min_length": realm.password_min_length,
        "role_holder_password_min_length": realm.role_holder_password_min_length.max(realm.password_min_length),
    }))
    .into_response())
}

async fn read(State(st): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    let m = me(&st, &headers, false).await?;
    let a = &m.account;
    let catalog = st.catalog.get();
    let mut conn = st.store.pool().acquire().await?;
    let subject = Subject::user(subject_bindings(&mut conn, &a.id).await?);
    let platform = [ScopeRef::platform()];
    let manages = m.realm.accounts_managed_by.iter().any(|r| catalog.has_role(&subject, r, &ScopeRef::platform()));
    Ok(Json(json!({
        "account": {
            "id": a.id, "email": a.email, "display_name": a.display_name, "locale": a.locale,
            "email_verified": a.is_verified(), "has_password": a.password_hash.is_some(),
            "created_at": a.created_at,
        },
        "session": { "id": m.session.id, "auth_time": m.session.auth_time, "recent": recent(&m.session) },
        // Which admin pages to show; the admin API decides again on every call.
        "admin": {
            "accounts": catalog.allowed(&subject, "id.accounts.read", &platform) || manages,
            "service_accounts": catalog.allowed(&subject, "id.service_accounts.manage", &platform),
            "audit": catalog.allowed(&subject, "id.audit.read", &platform),
        },
    }))
    .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    display_name: Option<String>,
    locale: Option<String>,
}

async fn update(State(st): State<Shared>, headers: HeaderMap, Json(body): Json<Profile>) -> ApiResult<Response> {
    let m = me(&st, &headers, true).await?;
    let name = body.display_name.map_or(m.account.display_name.clone(), |n| n.trim().chars().take(200).collect());
    let locale = match body.locale {
        Some(l) if m.realm.locales.contains(&l) => l,
        Some(_) => {
            return Err(ApiError::invalid("that language is not offered").with_details(json!({ "code": "bad_locale" })));
        }
        None => m.account.locale.clone(),
    };
    // Nothing changed, nothing to tell: the feed carries changes only.
    if name == m.account.display_name && locale == m.account.locale {
        return Ok(Json(json!({ "display_name": name, "locale": locale, "seq": null })).into_response());
    }
    st.store.account_set_profile(&m.account.id, &name, &locale).await?;
    let mut tx = st.store.begin().await?;
    let seq =
        changes::record(&mut tx, "account.updated", None, Some(&m.account.id), json!({ "sub": m.account.id })).await?;
    tx.commit().await?;
    st.store.audit(&audit(&m, "profile_updated")).await?;
    Ok(Json(json!({ "display_name": name, "locale": locale, "seq": seq })).into_response())
}

#[derive(Deserialize)]
struct PasswordOnly {
    password: String,
}

/// The password again: the session counts as a recent sign-in for ten minutes.
async fn reauth(State(st): State<Shared>, headers: HeaderMap, Json(body): Json<PasswordOnly>) -> ApiResult<StatusCode> {
    let m = me(&st, &headers, true).await?;
    check_password(&st, &m.account, &body.password).await?;
    st.store.session_reauthenticated(&m.session.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct NewPassword {
    #[serde(default)]
    current: Option<String>,
    password: String,
}

/// A new password: with the current one, or after a recent sign-in when there is none yet
/// (an account made with Google). The other sessions end; this one stays.
async fn set_password(
    State(st): State<Shared>,
    headers: HeaderMap,
    Json(body): Json<NewPassword>,
) -> ApiResult<Response> {
    let m = me(&st, &headers, true).await?;
    match &m.account.password_hash {
        Some(_) => check_password(&st, &m.account, body.current.as_deref().unwrap_or_default()).await?,
        None => need_recent(&m.session)?,
    }
    let min = policy::min_length(&st.store, m.realm, Some(&m.account)).await?;
    policy::check(&body.password, min).map_err(|e| {
        ApiError::invalid("choose another password").with_details(json!({ "code": e.code(), "min_length": min }))
    })?;
    let hash = hash_password(&st, &body.password).await?;
    st.store.account_set_password(&m.account.id, &hash).await?;
    st.store.account_tokens_delete(&m.account.id, "reset_password").await?;
    st.store.account_tokens_delete(&m.account.id, "change_email").await?;
    let ended = oidc::logout::end_others(&st, &m.account.id, &m.session.id, "password_changed").await?;
    st.store.session_reauthenticated(&m.session.id).await?;
    let params = BTreeMap::from([
        ("when".to_owned(), Utc::now().format("%Y-%m-%d %H:%M UTC").to_string()),
        ("link".to_owned(), format!("{}/forgot-password", m.realm.issuer)),
    ]);
    mail::enqueue(&st, Template::PasswordChanged, &m.realm.name, &m.account.email, params).await?;
    st.store.audit(&audit(&m, "password_changed")).await?;
    Ok(Json(json!({ "other_sessions_ended": ended })).into_response())
}

#[derive(Deserialize)]
struct NewEmail {
    email: String,
}

/// A new address: a link goes to it, and the change happens when the link is used.
async fn change_email(State(st): State<Shared>, headers: HeaderMap, Json(body): Json<NewEmail>) -> ApiResult<Response> {
    let m = me(&st, &headers, true).await?;
    need_recent(&m.session)?;
    let email = crate::store::normalize_email(&body.email);
    if !oidc::account::plausible_email(&email) {
        return Err(ApiError::invalid("that is not an email address").with_details(json!({ "code": "bad_email" })));
    }
    if email == m.account.email {
        return Err(ApiError::invalid("that is your address already").with_details(json!({ "code": "same_email" })));
    }
    // Counted first, and the same answer whether or not another account has the address: the
    // endpoint must not tell which addresses have accounts. A taken address gets
    // no link; confirming one would fail anyway.
    let hit = st.store.throttle_hit("mail_change_email", &m.account.id, 15 * 60, 4, 15 * 60).await?;
    let taken = st.store.account_by_email(&email).await?.is_some();
    if hit.count <= 3 && !taken {
        let token = tokens::opaque();
        let expires = Utc::now() + Duration::hours(i64::from(m.realm.verification_link_hours));
        st.store
            .account_token_create(
                &sha256(token.as_bytes()),
                "change_email",
                &m.account.id,
                &email,
                &m.realm.name,
                expires,
            )
            .await?;
        let params = BTreeMap::from([
            ("email".to_owned(), email.clone()),
            ("hours".to_owned(), m.realm.verification_link_hours.to_string()),
            ("link".to_owned(), format!("{}/verify-email?token={token}", m.realm.issuer)),
        ]);
        mail::enqueue(&st, Template::VerifyEmail, &m.realm.name, &email, params).await?;
    }
    st.store.audit(&audit(&m, "email_change_requested")).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "email": email }))).into_response())
}

/// Applies an address change from its link (called by the link page's confirmation).
pub async fn confirm_change(st: &Shared, realm: &Realm, hash: &[u8]) -> ApiResult<Option<String>> {
    let Some(link) = st.store.account_token_peek(hash, "change_email").await? else { return Ok(None) };
    let account = st
        .store
        .account(&link.account_id)
        .await?
        .filter(|a| a.is_active() && link.realm == realm.name)
        .ok_or_else(|| ApiError::new(StatusCode::GONE, "link_invalid", "the link is used, expired or replaced"))?;
    // Taken once: of two confirmations at the same moment, one changes the address.
    if st.store.account_token_take(hash, "change_email").await?.is_none() {
        return Err(ApiError::new(StatusCode::GONE, "link_invalid", "the link is used, expired or replaced"));
    }
    match st.store.account_set_email(&account.id, &link.email, Some("verified")).await {
        Err(StoreError::Conflict(m)) => {
            return Err(ApiError::conflict(m).with_details(json!({ "code": "email_taken" })));
        }
        r => r?,
    }
    let params = BTreeMap::from([("new_email".to_owned(), link.email.clone())]);
    mail::enqueue(st, Template::EmailChanged, &realm.name, &account.email, params).await?;
    let mut tx = st.store.begin().await?;
    changes::record(&mut tx, "account.updated", None, Some(&account.id), json!({ "sub": account.id })).await?;
    tx.commit().await?;
    st.store
        .audit(&AuditEntry {
            realm: Some(realm.name.clone()),
            ..AuditEntry::new("email_changed").by(&account.id, "user").subject(&account.id)
        })
        .await?;
    Ok(Some(link.email))
}

async fn sessions(State(st): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    let m = me(&st, &headers, false).await?;
    let list: Vec<Value> = st
        .store
        .sessions_of(&m.account.id)
        .await?
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id, "current": s.id == m.session.id, "device": s.device_label, "ip": s.ip,
                "realm": s.realm, "created_at": s.created_at, "last_seen_at": s.last_seen_at,
                "expires_at": s.expires_at, "remember_me": s.remember_me,
            })
        })
        .collect();
    Ok(Json(json!({ "sessions": list })).into_response())
}

/// Ends one of the person's sessions; ending this one signs this browser out.
async fn end_one(State(st): State<Shared>, headers: HeaderMap, Path(sid): Path<String>) -> ApiResult<Response> {
    let m = me(&st, &headers, true).await?;
    let mine = st.store.session(&sid).await?.is_some_and(|s| s.account_id == m.account.id);
    if !mine {
        return Err(ApiError::not_found("no such session"));
    }
    oidc::logout::end_session(&st, &sid, "ended_by_owner").await?;
    st.store.audit(&audit(&m, "session_ended").detail(json!({ "sid": sid }))).await?;
    let mut r = StatusCode::NO_CONTENT.into_response();
    if sid == m.session.id {
        r.headers_mut().append(header::SET_COOKIE, clear_cookie(m.realm, SID));
    }
    Ok(r)
}

async fn end_others(State(st): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    let m = me(&st, &headers, true).await?;
    let ended = oidc::logout::end_others(&st, &m.account.id, &m.session.id, "ended_by_owner").await?;
    st.store.audit(&audit(&m, "other_sessions_ended").detail(json!({ "count": ended }))).await?;
    Ok(Json(json!({ "ended": ended })).into_response())
}

/// The person's roles, with the names of the scopes they are held at.
async fn organisations(State(st): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    let m = me(&st, &headers, false).await?;
    let mut conn = st.store.pool().acquire().await?;
    let mut out = Vec::new();
    for b in db::bindings_of(&mut conn, &m.account.id).await? {
        let Some(path) = db::path_of(&mut conn, &b.scope()).await? else { continue };
        let mut names = Vec::new();
        for s in &path {
            let name = match (s.kind.as_str(), s.id.as_deref()) {
                (pacod_id_authz::PLATFORM, _) => String::new(),
                (pacod_id_authz::ORGANISATION, Some(id)) => db::organisation(&mut conn, id)
                    .await?
                    .map(|o| if o.display_name.is_empty() { o.slug } else { o.display_name })
                    .unwrap_or_default(),
                (kind, Some(id)) => db::scope(&mut conn, kind, id)
                    .await?
                    .map(|x| x.display_name)
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| id.to_owned()),
                _ => String::new(),
            };
            names.push(json!({ "scope": s.to_string(), "name": name }));
        }
        out.push(json!({
            "binding": b.id, "role": b.role, "product": b.product(), "scope": b.scope().to_string(),
            "path": names, "since": b.created_at,
        }));
    }
    Ok(Json(json!({ "roles": out })).into_response())
}

/// Deletes the person's own account, after a recent sign-in.
async fn remove(State(st): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    let m = me(&st, &headers, true).await?;
    need_recent(&m.session)?;
    let mut conn = st.store.pool().acquire().await?;
    let subject = Subject::user(subject_bindings(&mut conn, &m.account.id).await?);
    drop(conn);
    let seq = crate::admin::deletion::delete_account(&st, &m.account, &subject, &m.account.id).await?;
    st.store
        .audit(&AuditEntry {
            realm: Some(m.realm.name.clone()),
            ..AuditEntry::new("account_deleted").by(crate::admin::deletion::DELETED, "user")
        })
        .await?;
    let mut r = Json(json!({ "seq": seq })).into_response();
    r.headers_mut().append(header::SET_COOKIE, clear_cookie(m.realm, SID));
    Ok(r)
}
