//! The sign-in flow behind /oidc/authorize, as the hosted UI drives it (/ui-api/v1/flows).
//!
//! Every call checks that the flow belongs to this browser (its cookie) and, for writes, that
//! the request comes from the realm's own pages (Origin) as JSON: the UI's CSRF protection.
//! Throttles are counted before any password is hashed.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get, routing::post};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use url::Url;

use super::{Caller, FLOW, Request, cookie_name, read_cookie, realm_by_host, sign_in};
use crate::config::{Realm, authority};
use crate::crypto::{ct_eq, password, sha256, tokens};
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::mail::{self, Template};
use crate::policy;
use crate::store::{Account, AuditEntry, Flow};

/// Sign-in failures per account (from browsers it has not signed in on) or per known browser.
const ACCOUNT_LIMIT: i32 = 10;
/// Sign-in failures per client address.
const ADDRESS_LIMIT: i32 = 30;
const WINDOW: u32 = 15 * 60;
const LOCK: u32 = 15 * 60;
/// Mail requests per purpose and address.
pub const MAIL_LIMIT: i32 = 3;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/ui-api/v1/flows/{id}", get(show))
        .route("/ui-api/v1/flows/{id}/password", post(password_step))
        .route("/ui-api/v1/flows/{id}/password-change", post(password_change))
        .route("/ui-api/v1/flows/{id}/resume", post(resume))
}

/// The realm's own origin, `scheme://host[:port]`.
fn origin(realm: &Realm) -> String {
    let u = Url::parse(&realm.issuer).expect("checked at start");
    format!("{}://{}", u.scheme(), authority(&u))
}

/// A write from the realm's own pages: same Origin. Browsers send Origin on every POST.
pub fn same_origin(realm: &Realm, headers: &HeaderMap) -> ApiResult<()> {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(o) if o == origin(realm) => Ok(()),
        _ => Err(ApiError::forbidden("requests come from the sign-in pages only")
            .with_details(json!({ "code": "cross_origin" }))),
    }
}

/// The flow, when it belongs to this realm and this browser (for the account endpoints).
pub async fn flow_of_pub<'a>(st: &'a Shared, headers: &HeaderMap, id: &str) -> ApiResult<(&'a Realm, Flow)> {
    flow_of(st, headers, id).await
}

/// The flow, when it belongs to this realm and this browser.
async fn flow_of<'a>(st: &'a Shared, headers: &HeaderMap, id: &str) -> ApiResult<(&'a Realm, Flow)> {
    let realm = realm_by_host(st, headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    let expired = || {
        ApiError::new(StatusCode::GONE, "flow_expired", "the sign-in took too long; start again from the application")
    };
    let flow = st.store.flow(id).await?.filter(|f| f.realm == realm.name).ok_or_else(expired)?;
    let cookie = read_cookie(headers, &cookie_name(realm, FLOW)).ok_or_else(expired)?;
    if !ct_eq(&sha256(cookie.as_bytes()), &flow.browser_hash) {
        return Err(expired());
    }
    Ok((realm, flow))
}

/// Hashes a password, a few at a time (each needs 19 MiB or more).
pub async fn hash_password(st: &Shared, pw: &str) -> ApiResult<String> {
    let _permit = st.hashing.acquire().await.map_err(ApiError::internal)?;
    let (pw, cost) = (pw.to_owned(), st.boot.password_cost);
    tokio::task::spawn_blocking(move || password::hash(&pw, cost))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)
}

pub async fn verify_password(st: &Shared, pw: &str, stored: &str) -> ApiResult<password::Checked> {
    let _permit = st.hashing.acquire().await.map_err(ApiError::internal)?;
    let (pw, stored, cost) = (pw.to_owned(), stored.to_owned(), st.boot.password_cost);
    tokio::task::spawn_blocking(move || password::verify(&pw, &stored, cost)).await.map_err(ApiError::internal)
}

fn too_many() -> ApiError {
    ApiError::too_many("too many attempts; try again in 15 minutes")
        .with_details(json!({ "code": "too_many_attempts" }))
}

/// The answer that moves the browser on, with its cookies.
fn moved(to: &str, cookies: Vec<axum::http::HeaderValue>) -> Response {
    let mut r = Json(json!({ "next": "redirect", "to": to })).into_response();
    for c in cookies {
        r.headers_mut().append(header::SET_COOKIE, c);
    }
    super::no_store(r.headers_mut());
    r
}

async fn show(State(st): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult<Response> {
    let (realm, flow) = flow_of(&st, &headers, &id).await?;
    let client = st.config.clients.get(&flow.client_id);
    let request: Request = serde_json::from_value(flow.request.clone()).map_err(ApiError::internal)?;
    let self_signup = client.and_then(|c| c.self_signup).unwrap_or(realm.self_signup);
    Ok(Json(json!({
        "id": flow.id,
        "stage": flow.stage,
        "realm": realm.name,
        "brand": realm.brand,
        "locales": realm.locales,
        "default_locale": realm.default_locale,
        "ui_locales": request.ui_locales,
        "login_hint": request.login_hint,
        "client": flow.client_id,
        "self_signup": self_signup,
        "password_min_length": realm.password_min_length,
        "terms_url": realm.terms_url,
        "privacy_url": realm.privacy_url,
        "google": realm.google.is_some(),
        "link_email": flow.request["google_pending"]["email"],
    }))
    .into_response())
}

#[derive(Deserialize)]
struct PasswordStep {
    email: String,
    password: String,
    #[serde(default)]
    remember_me: bool,
}

/// What follows a proven password: a detour (confirm the address, choose a longer password),
/// or the sign-in itself.
async fn after_password(
    st: &Shared,
    realm: &Realm,
    flow: &Flow,
    account: &Account,
    remember_me: bool,
    caller: &Caller,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let mut request: Request = serde_json::from_value(flow.request.clone()).map_err(ApiError::internal)?;
    request.remember_me = remember_me;
    let store_request = |r: &Request| serde_json::to_value(r).map_err(ApiError::internal);
    if realm.require_verified_email && st.mail.is_some() && !account.is_verified() {
        send_verification(st, realm, account).await?;
        st.store.flow_set_request(&flow.id, &store_request(&request)?, "verify_email", Some(&account.id)).await?;
        return Ok(Json(json!({ "next": "verify_email", "email": account.email })).into_response());
    }
    if account.password_change_due {
        let min = policy::min_length(&st.store, realm, Some(account)).await?;
        st.store.flow_set_request(&flow.id, &store_request(&request)?, "change_password", Some(&account.id)).await?;
        return Ok(Json(json!({ "next": "change_password", "min_length": min })).into_response());
    }
    let done = sign_in(st, realm, flow, account, remember_me, &["pwd".to_owned()], caller, headers).await?;
    Ok(moved(&done.to, done.cookies))
}

/// Mails a confirmation link, at most three in 15 minutes per address.
pub async fn send_verification(st: &Shared, realm: &Realm, account: &Account) -> ApiResult<()> {
    let hit = st.store.throttle_hit("mail_verify_email", &account.email, WINDOW, MAIL_LIMIT + 1, WINDOW).await?;
    if hit.count > MAIL_LIMIT {
        return Ok(());
    }
    let token = tokens::opaque();
    let expires = Utc::now() + Duration::hours(i64::from(realm.verification_link_hours));
    st.store
        .account_token_create(
            &sha256(token.as_bytes()),
            "verify_email",
            &account.id,
            &account.email,
            &realm.name,
            expires,
        )
        .await?;
    let params = BTreeMap::from([
        ("email".to_owned(), account.email.clone()),
        ("hours".to_owned(), realm.verification_link_hours.to_string()),
        ("link".to_owned(), format!("{}/verify-email?token={token}", realm.issuer)),
    ]);
    mail::enqueue(st, Template::VerifyEmail, &realm.name, &account.email, params).await?;
    Ok(())
}

async fn password_step(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PasswordStep>,
) -> ApiResult<Response> {
    let (realm, flow) = flow_of(&st, &headers, &id).await?;
    same_origin(realm, &headers)?;
    // After Google named an account that needs its password before the link.
    let linking = flow.stage == "link_google";
    if flow.stage != "start" && !linking {
        return Err(
            ApiError::conflict("this sign-in is past the password").with_details(json!({ "code": "wrong_stage" }))
        );
    }
    let caller = Caller::of(&st, peer, &headers);
    let email = crate::store::normalize_email(&body.email);
    let account = st.store.account_by_email(&email).await?;
    if linking && account.as_ref().map(|a| a.id.as_str()) != flow.account_id.as_deref() {
        return Err(ApiError::conflict("sign in to the account Google named, or start again")
            .with_details(json!({ "code": "wrong_account" })));
    }

    // Counted before hashing: a locked address or account costs no work.
    if st.store.throttle_locked("login_address", &caller.ip_bucket()).await?.is_some() {
        return Err(too_many());
    }
    let device = read_cookie(&headers, &cookie_name(realm, super::DEVICE)).map(|d| st.boot.token_pepper.hash(&d));
    let known = match (&account, &device) {
        (Some(a), Some(d)) => st.store.device_known(d, &a.id).await?,
        _ => false,
    };
    // A browser this account signed in on before keeps its own counter: someone who knows the
    // address cannot lock its owner out.
    let (bucket, key) = match (&account, &device) {
        (Some(a), Some(d)) if known => ("login_device", format!("{}:{}", hex::encode(d), a.id)),
        _ => ("login_account", email.clone()),
    };
    if st.store.throttle_locked(bucket, &key).await?.is_some() {
        return Err(too_many());
    }
    // Each attempt takes its slot before the hashing: parallel guesses cannot all pass the
    // checks above before the first of them is counted. A success gives the slots back.
    if !st.store.throttle_take("login_address", &caller.ip_bucket(), WINDOW, ADDRESS_LIMIT, LOCK).await? {
        return Err(too_many());
    }
    if !st.store.throttle_take(bucket, &key, WINDOW, ACCOUNT_LIMIT, LOCK).await? {
        st.store.throttle_give_back("login_address", &caller.ip_bucket()).await?;
        return Err(too_many());
    }

    let stored = account.as_ref().and_then(|a| a.password_hash.clone()).unwrap_or_else(|| st.dummy_hash.clone());
    let checked = verify_password(&st, &body.password, &stored).await?;
    let audit = |action: &str, subject: &str, detail: serde_json::Value| AuditEntry {
        realm: Some(realm.name.clone()),
        client_id: Some(flow.client_id.clone()),
        ip: caller.ip.clone(),
        user_agent: caller.user_agent.clone(),
        ..AuditEntry::new(action).subject(subject).detail(detail)
    };
    let Some(account) = account.filter(|a| checked.ok && a.password_hash.is_some()) else {
        st.store.audit(&audit("login_failed", &email, json!({ "reason": "credentials" }))).await?;
        return Err(ApiError::unauthorized("the address or the password is wrong")
            .with_details(json!({ "code": "invalid_credentials" })));
    };
    st.store.throttle_give_back("login_address", &caller.ip_bucket()).await?;
    st.store.throttle_clear(bucket, &key).await?;
    if linking && account.is_active() {
        crate::federation::link_pending(&st, &flow, &account).await?;
    }
    let account = if linking { st.store.account(&account.id).await?.unwrap_or(account) } else { account };
    if !account.is_active() {
        st.store.audit(&audit("login_failed", &account.id, json!({ "reason": "disabled" }))).await?;
        return Err(ApiError::forbidden("this account is disabled").with_details(json!({ "code": "account_disabled" })));
    }
    if checked.rehash {
        let hash = hash_password(&st, &body.password).await?;
        st.store.account_rehash(&account.id, &hash).await?;
    }
    // A role holder's password shorter than the role holders' minimum must change now; noted
    // before any detour, so a sign-in resumed after confirming the address still asks for it.
    let account = match policy::forced_min_length(&st.store, realm, &account).await? {
        Some(min) if policy::length(&body.password) < min && !account.password_change_due => {
            st.store.account_set_change_due(&account.id, true).await?;
            Account { password_change_due: true, ..account }
        }
        _ => account,
    };
    after_password(&st, realm, &flow, &account, body.remember_me, &caller, &headers).await
}

#[derive(Deserialize)]
struct NewPassword {
    password: String,
}

/// A new password, when the old one is too short for the account's roles or a change is due.
async fn password_change(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<NewPassword>,
) -> ApiResult<Response> {
    let (realm, flow) = flow_of(&st, &headers, &id).await?;
    same_origin(realm, &headers)?;
    let account_id = flow.account_id.clone().filter(|_| flow.stage == "change_password").ok_or_else(|| {
        ApiError::conflict("no password change is due in this sign-in").with_details(json!({ "code": "wrong_stage" }))
    })?;
    let account = st.store.account(&account_id).await?.filter(Account::is_active).ok_or_else(|| {
        ApiError::forbidden("this account is disabled").with_details(json!({ "code": "account_disabled" }))
    })?;
    let min = policy::min_length(&st.store, realm, Some(&account)).await?;
    if let Err(p) = policy::check(&body.password, min) {
        return Err(
            ApiError::invalid("choose another password").with_details(json!({ "code": p.code(), "min_length": min }))
        );
    }
    let hash = hash_password(&st, &body.password).await?;
    st.store.account_set_password(&account.id, &hash).await?;
    st.store.account_tokens_delete(&account.id, "reset_password").await?;
    super::logout::end_all(&st, &account.id, "password_changed").await?;
    let params = BTreeMap::from([
        ("when".to_owned(), Utc::now().format("%Y-%m-%d %H:%M UTC").to_string()),
        ("link".to_owned(), format!("{}/forgot-password", realm.issuer)),
    ]);
    mail::enqueue(&st, Template::PasswordChanged, &realm.name, &account.email, params).await?;
    let caller = Caller::of(&st, peer, &headers);
    st.store
        .audit(&AuditEntry {
            realm: Some(realm.name.clone()),
            ip: caller.ip.clone(),
            ..AuditEntry::new("password_changed").by(&account.id, "user").subject(&account.id)
        })
        .await?;
    let request: Request = serde_json::from_value(flow.request.clone()).map_err(ApiError::internal)?;
    let account = st.store.account(&account.id).await?.ok_or_else(|| ApiError::internal("the account vanished"))?;
    let done =
        sign_in(&st, realm, &flow, &account, request.remember_me, &["pwd".to_owned()], &caller, &headers).await?;
    Ok(moved(&done.to, done.cookies))
}

/// Continues a sign-in that waited for the address to be confirmed (in this browser, within the
/// flow's 15 minutes).
async fn resume(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let (realm, flow) = flow_of(&st, &headers, &id).await?;
    same_origin(realm, &headers)?;
    if flow.stage != "verify_email" {
        return Err(ApiError::conflict("this sign-in does not wait for a confirmation")
            .with_details(json!({ "code": "wrong_stage" })));
    }
    let request: Request = serde_json::from_value(flow.request.clone()).map_err(ApiError::internal)?;
    // A sign-up with a taken address waits like any other, and never completes.
    let Some(account_id) = flow.account_id.clone() else {
        return Ok(Json(json!({ "next": "verify_email", "email": request.login_hint })).into_response());
    };
    let account = st.store.account(&account_id).await?.filter(Account::is_active).ok_or_else(|| {
        ApiError::forbidden("this account is disabled").with_details(json!({ "code": "account_disabled" }))
    })?;
    if !account.is_verified() {
        return Ok(Json(json!({ "next": "verify_email", "email": account.email })).into_response());
    }
    let caller = Caller::of(&st, peer, &headers);
    after_password(&st, realm, &flow, &account, request.remember_me, &caller, &headers).await
}
