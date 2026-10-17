//! Sign-up, address confirmation and password reset, for the hosted UI.
//!
//! Answers never say whether an address has an account: sign-up with a taken address answers
//! like a new one (its owner gets an "you already have an account" mail), and the link requests
//! always answer 202.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::json;

use super::flows::{MAIL_LIMIT, hash_password, same_origin, send_verification};
use super::{Caller, Request, realm_by_host};
use crate::config::Realm;
use crate::crypto::{sha256, tokens};
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::mail::{self, Template};
use crate::policy;
use crate::store::{Account, AuditEntry, NewAccount, normalize_email};

const WINDOW: u32 = 15 * 60;
/// Mail requests per client address in an hour.
const ADDRESS_MAIL_LIMIT: i32 = 20;
/// Sign-ups per client address in an hour.
const SIGNUP_LIMIT: i32 = 10;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/ui-api/v1/flows/{id}/signup", post(signup))
        .route("/ui-api/v1/verification", post(resend_verification))
        .route("/ui-api/v1/verification/confirm", post(confirm_verification))
        .route("/ui-api/v1/password-reset", post(request_reset))
        .route("/ui-api/v1/password-reset/confirm", post(confirm_reset))
}

fn realm_of<'a>(st: &'a Shared, headers: &HeaderMap) -> ApiResult<&'a Realm> {
    let realm = realm_by_host(st, headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    same_origin(realm, headers)?;
    Ok(realm)
}

fn accepted() -> Response {
    (StatusCode::ACCEPTED, Json(json!({ "next": "check_email" }))).into_response()
}

/// Whether a client address may ask for more mail this hour (20).
async fn address_may_mail(st: &Shared, caller: &Caller) -> ApiResult<bool> {
    let hit = st.store.throttle_hit("mail_address", &caller.ip_bucket(), 3600, ADDRESS_MAIL_LIMIT + 1, 3600).await?;
    Ok(hit.count <= ADDRESS_MAIL_LIMIT)
}

pub(crate) fn plausible_email(e: &str) -> bool {
    let e = e.trim();
    e.len() <= 254
        && e.split_once('@').is_some_and(|(l, d)| {
            !l.is_empty() && d.contains('.') && !d.starts_with('.') && !e.contains(char::is_whitespace)
        })
}

#[derive(Deserialize)]
struct Signup {
    email: String,
    password: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    locale: String,
    #[serde(default)]
    accept_terms: bool,
}

async fn signup(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<Signup>,
) -> ApiResult<Response> {
    let (realm, flow) = super::flows::flow_of_pub(&st, &headers, &id).await?;
    same_origin(realm, &headers)?;
    if flow.stage != "start" {
        return Err(
            ApiError::conflict("this sign-in is past its first step").with_details(json!({ "code": "wrong_stage" }))
        );
    }
    if !super::self_signup(&st, realm, &flow)? {
        return Err(ApiError::forbidden("accounts are made by invitation here")
            .with_details(json!({ "code": "signup_closed" })));
    }
    if !plausible_email(&body.email) {
        return Err(ApiError::invalid("that is not an email address").with_details(json!({ "code": "bad_email" })));
    }
    if let Err(p) = policy::check(&body.password, realm.password_min_length) {
        return Err(ApiError::invalid("choose another password")
            .with_details(json!({ "code": p.code(), "min_length": realm.password_min_length })));
    }
    if realm.terms_url.is_some() && !body.accept_terms {
        return Err(
            ApiError::invalid("the terms must be accepted").with_details(json!({ "code": "terms_not_accepted" }))
        );
    }
    let caller = Caller::of(&st, peer, &headers);
    // Counted before hashing, whether or not the address is taken.
    let hit = st.store.throttle_hit("signup_address", &caller.ip_bucket(), 3600, SIGNUP_LIMIT + 1, 3600).await?;
    if hit.count > SIGNUP_LIMIT {
        return Err(ApiError::too_many("too many sign-ups from this network; try again later")
            .with_details(json!({ "code": "too_many_attempts" })));
    }
    let email = normalize_email(&body.email);
    let locale = if realm.locales.contains(&body.locale) { body.locale.clone() } else { realm.default_locale.clone() };
    let hash = hash_password(&st, &body.password).await?;
    let created = st
        .store
        .account_create(&NewAccount {
            id: tokens::new_id("usr"),
            email: email.clone(),
            display_name: body.display_name.trim().chars().take(200).collect(),
            locale,
            password_hash: Some(hash),
            verified_by: None,
            signup_realm: realm.name.clone(),
        })
        .await;
    match created {
        Ok(account) => {
            if realm.terms_url.is_some() {
                st.store.consent(&account.id, &realm.name, "terms", &realm.terms_version).await?;
            }
            st.store
                .audit(&AuditEntry {
                    realm: Some(realm.name.clone()),
                    ip: caller.ip.clone(),
                    ..AuditEntry::new("account_created")
                        .by(&account.id, "user")
                        .subject(&account.id)
                        .detail(json!({ "how": "signup" }))
                })
                .await?;
            // Without mail no confirmation is possible, and none is asked for. Nor
            // can the answer hide a taken address then: a realm open to everyone needs mail.
            if st.mail.is_none() {
                let request: Request = serde_json::from_value(flow.request.clone()).map_err(ApiError::internal)?;
                let done = super::sign_in(
                    &st,
                    realm,
                    &flow,
                    &account,
                    request.remember_me,
                    &["pwd".to_owned()],
                    &caller,
                    &headers,
                )
                .await?;
                let mut r = Json(json!({ "next": "redirect", "to": done.to })).into_response();
                for c in done.cookies {
                    r.headers_mut().append(axum::http::header::SET_COOKIE, c);
                }
                return Ok(r);
            }
            send_verification(&st, realm, &account).await?;
            st.store
                .flow_set_request(&flow.id, &waiting_for(&flow, &email)?, "verify_email", Some(&account.id))
                .await?;
        }
        Err(crate::store::StoreError::Conflict(_)) => {
            // The same answer as for a new address, and the flow in the same state (bound to no
            // account, so that nothing can complete it); the owner hears of it.
            st.store.flow_set_request(&flow.id, &waiting_for(&flow, &email)?, "verify_email", None).await?;
            if address_may_mail(&st, &caller).await? {
                let hit = st.store.throttle_hit("mail_account_exists", &email, WINDOW, MAIL_LIMIT + 1, WINDOW).await?;
                if hit.count <= MAIL_LIMIT {
                    let params = BTreeMap::from([
                        ("email".to_owned(), email.clone()),
                        ("link".to_owned(), format!("{}/login", realm.issuer)),
                    ]);
                    mail::enqueue(&st, Template::AccountExists, &realm.name, &email, params).await?;
                }
            }
        }
        Err(e) => return Err(e.into()),
    }
    Ok(Json(json!({ "next": "verify_email", "email": email })).into_response())
}

/// The flow's request with the address a sign-up waits on, the same for a new and a taken one.
fn waiting_for(flow: &crate::store::Flow, email: &str) -> ApiResult<serde_json::Value> {
    let mut request: Request = serde_json::from_value(flow.request.clone()).map_err(ApiError::internal)?;
    request.login_hint = Some(email.to_owned());
    serde_json::to_value(&request).map_err(ApiError::internal)
}

#[derive(Deserialize)]
struct EmailOnly {
    email: String,
}

async fn resend_verification(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<EmailOnly>,
) -> ApiResult<Response> {
    let realm = realm_of(&st, &headers)?;
    let caller = Caller::of(&st, peer, &headers);
    if address_may_mail(&st, &caller).await?
        && let Some(a) = st.store.account_by_email(&body.email).await?.filter(|a| !a.is_verified() && a.is_active())
    {
        send_verification(&st, realm, &a).await?;
    }
    Ok(accepted())
}

#[derive(Deserialize)]
struct TokenOnly {
    token: String,
}

fn link_invalid() -> ApiError {
    ApiError::new(StatusCode::GONE, "link_invalid", "the link is used, expired or replaced by a newer one")
}

/// Confirms an address with the link's token: the link works once and only for the address it
/// was sent to.
async fn confirm_verification(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<TokenOnly>,
) -> ApiResult<Response> {
    let realm = realm_of(&st, &headers)?;
    let hash = sha256(body.token.as_bytes());
    // A link for a new address (sent from the account page) is used on the same page.
    let Some(link) = st.store.account_token_peek(&hash, "verify_email").await? else {
        return match crate::me::confirm_change(&st, realm, &hash).await? {
            Some(email) => Ok(Json(json!({ "email": email, "changed": true })).into_response()),
            None => Err(link_invalid()),
        };
    };
    let account = st
        .store
        .account(&link.account_id)
        .await?
        .filter(|a| a.email == link.email && link.realm == realm.name)
        .ok_or_else(link_invalid)?;
    st.store.account_token_take(&hash, "verify_email").await?.ok_or_else(link_invalid)?;
    st.store.account_mark_verified(&account.id, "verified").await?;
    let caller = Caller::of(&st, peer, &headers);
    st.store
        .audit(&AuditEntry {
            realm: Some(realm.name.clone()),
            ip: caller.ip,
            ..AuditEntry::new("email_verified").by(&account.id, "user").subject(&account.id)
        })
        .await?;
    Ok(Json(json!({ "email": account.email })).into_response())
}

async fn request_reset(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<EmailOnly>,
) -> ApiResult<Response> {
    let realm = realm_of(&st, &headers)?;
    let caller = Caller::of(&st, peer, &headers);
    if !address_may_mail(&st, &caller).await? {
        return Ok(accepted());
    }
    let Some(account) = st.store.account_by_email(&body.email).await?.filter(Account::is_active) else {
        return Ok(accepted());
    };
    let hit = st.store.throttle_hit("mail_reset_password", &account.email, WINDOW, MAIL_LIMIT + 1, WINDOW).await?;
    if hit.count > MAIL_LIMIT {
        return Ok(accepted());
    }
    send_reset(&st, realm, &account).await?;
    Ok(accepted())
}

/// Mails a reset link for `realm`; the caller has checked throttles and rights.
pub async fn send_reset(st: &Shared, realm: &Realm, account: &Account) -> ApiResult<()> {
    let token = tokens::opaque();
    let expires = Utc::now() + Duration::minutes(i64::from(realm.reset_link_minutes));
    st.store
        .account_token_create(
            &sha256(token.as_bytes()),
            "reset_password",
            &account.id,
            &account.email,
            &realm.name,
            expires,
        )
        .await?;
    let params = BTreeMap::from([
        ("email".to_owned(), account.email.clone()),
        ("minutes".to_owned(), realm.reset_link_minutes.to_string()),
        ("link".to_owned(), format!("{}/reset-password?token={token}", realm.issuer)),
    ]);
    mail::enqueue(st, Template::ResetPassword, &realm.name, &account.email, params).await?;
    Ok(())
}

#[derive(Deserialize)]
struct Reset {
    token: String,
    password: String,
}

/// A new password with a reset link. The link is checked before it is used up, so a refused
/// password does not cost it. Proves the address, and ends every session of the account.
async fn confirm_reset(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Reset>,
) -> ApiResult<Response> {
    let realm = realm_of(&st, &headers)?;
    let hash = sha256(body.token.as_bytes());
    let link = st.store.account_token_peek(&hash, "reset_password").await?.ok_or_else(link_invalid)?;
    let account = st
        .store
        .account(&link.account_id)
        .await?
        .filter(|a| a.email == link.email && a.is_active() && link.realm == realm.name)
        .ok_or_else(link_invalid)?;
    let min = policy::min_length(&st.store, realm, Some(&account)).await?;
    if let Err(p) = policy::check(&body.password, min) {
        return Err(
            ApiError::invalid("choose another password").with_details(json!({ "code": p.code(), "min_length": min }))
        );
    }
    st.store.account_token_take(&hash, "reset_password").await?.ok_or_else(link_invalid)?;
    let new_hash = hash_password(&st, &body.password).await?;
    st.store.account_set_password(&account.id, &new_hash).await?;
    st.store.account_mark_verified(&account.id, "verified").await?;
    // Whoever guessed at the old password no longer holds the account's sign-in lock.
    st.store.throttle_clear("login_account", &account.email).await?;
    super::logout::end_all(&st, &account.id, "password_reset").await?;
    let params = BTreeMap::from([
        ("when".to_owned(), Utc::now().format("%Y-%m-%d %H:%M UTC").to_string()),
        ("link".to_owned(), format!("{}/forgot-password", realm.issuer)),
    ]);
    mail::enqueue(&st, Template::PasswordChanged, &realm.name, &account.email, params).await?;
    let caller = Caller::of(&st, peer, &headers);
    st.store
        .audit(&AuditEntry {
            realm: Some(realm.name.clone()),
            ip: caller.ip,
            ..AuditEntry::new("password_reset").by(&account.id, "user").subject(&account.id)
        })
        .await?;
    Ok(Json(json!({ "next": "sign_in", "email": account.email })).into_response())
}
