//! Ending sessions: RP-initiated logout, the hosted UI's confirmation, a BFF's
//! server-to-server revocation, and back-channel logout to every client that had a grant in an
//! ended session.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration as StdDuration;

use axum::extract::{ConnectInfo, Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::{Json, Router, routing::get, routing::post};
use chrono::{Duration, Utc};
use pacod_id_jose::Alg;
use serde::Deserialize;
use serde_json::json;

use super::{Caller, SID, clear_cookie, cookie_name, read_cookie, realm_by_host, redirect, sign, with_query};
use crate::config::{Client, Realm};
use crate::crypto::{sha256, tokens};
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::http::caller::CallerClient;
use crate::store::{AuditEntry, Store, changes};

/// The event a back-channel logout token carries.
pub(crate) const BACKCHANNEL_EVENT: &str = "http://schemas.openid.net/event/backchannel-logout";

pub fn public_routes() -> Router<Shared> {
    Router::new()
        .route("/oidc/logout", get(rp_logout_get).post(rp_logout_post))
        .route("/ui-api/v1/logout", post(ui_logout))
}

pub fn internal_routes() -> Router<Shared> {
    Router::new().route("/internal/v1/sessions/{sid}/revoke", post(revoke_session))
}

// Ending sessions ----------------------------------------------------------------------------

/// Queues back-channel logouts for the clients that had grants in these sessions.
pub async fn notify_clients(st: &Shared, revoked: &[(String, String)]) -> anyhow::Result<()> {
    if revoked.is_empty() {
        return Ok(());
    }
    let sids: Vec<String> = revoked.iter().map(|(s, _)| s.clone()).collect();
    for (sid, client_id, account) in st.store.clients_of_sessions(&sids).await? {
        let Some(client) = st.config.clients.get(&client_id) else { continue };
        if client.backchannel_logout_uri.is_none() {
            continue;
        }
        let claims = json!({
            "iss": st.config.realms[&client.realm].issuer,
            "aud": client.id,
            "sid": sid,
            "sub": account,
            "events": { BACKCHANNEL_EVENT: {} },
        });
        st.store.backchannel_enqueue(&client.id, &claims).await?;
    }
    Ok(())
}

/// A logout token (OIDC Back-Channel Logout 2.4), valid for two minutes from now.
fn logout_token(st: &Shared, client: &Client, claims: &serde_json::Value) -> anyhow::Result<String> {
    let now = Utc::now();
    let mut claims = claims.clone();
    claims["iat"] = json!(now.timestamp());
    claims["exp"] = json!((now + Duration::minutes(2)).timestamp());
    claims["jti"] = json!(tokens::opaque());
    sign(st, client.id_token_alg, "logout+jwt", &claims).map_err(|e| anyhow::anyhow!(e.description))
}

/// Ends one session and its grants, and tells the clients.
pub async fn end_session(st: &Shared, sid: &str, reason: &str) -> anyhow::Result<bool> {
    let mut tx = st.store.begin().await?;
    let revoked = Store::session_revoke(&mut tx, sid, reason).await?;
    changes::sessions_revoked(&mut tx, &revoked, reason).await?;
    tx.commit().await?;
    notify_clients(st, &revoked).await?;
    Ok(!revoked.is_empty())
}

/// Ends every session and grant of an account, and tells the clients.
pub async fn end_all(st: &Shared, account: &str, reason: &str) -> anyhow::Result<()> {
    // A pending address change goes too: whoever asked for it may be who the owner is ending.
    st.store.account_tokens_delete(account, "change_email").await?;
    let sids = super::sign_out_everywhere(&st.store, account, reason).await?;
    let revoked: Vec<(String, String)> = sids.into_iter().map(|s| (s, account.to_owned())).collect();
    notify_clients(st, &revoked).await
}

/// Ends every other session and app grant of an account, keeping `keep`, and tells the clients.
pub async fn end_others(st: &Shared, account: &str, keep: &str, reason: &str) -> anyhow::Result<usize> {
    // Signing out the others is what someone does who suspects one of them: a pending address
    // change goes too, whoever asked for it.
    st.store.account_tokens_delete(account, "change_email").await?;
    let mut tx = st.store.begin().await?;
    let revoked = Store::sessions_revoke_others(&mut tx, account, keep, reason).await?;
    changes::sessions_revoked(&mut tx, &revoked, reason).await?;
    tx.commit().await?;
    notify_clients(st, &revoked).await?;
    Ok(revoked.len())
}

/// Delivers due back-channel logouts. Every instance runs this every few seconds; the outbox's
/// row locks keep a token from going twice.
pub async fn deliver(st: &Shared) -> anyhow::Result<usize> {
    let due = st.store.backchannel_due(20).await?;
    if due.is_empty() {
        return Ok(0);
    }
    let http = reqwest::Client::builder()
        .timeout(StdDuration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut done = 0;
    for d in due {
        let Some((client, uri)) =
            st.config.clients.get(&d.client_id).and_then(|c| Some((c, c.backchannel_logout_uri.clone()?)))
        else {
            st.store.backchannel_result(d.id, "").await?;
            continue;
        };
        // A token that cannot be signed fails this delivery only, not the rest of the batch.
        let token = match logout_token(st, client, &d.claims) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(client = %d.client_id, error = %e, "a back-channel logout could not be signed");
                st.store.backchannel_result(d.id, &format!("signing: {e}")).await?;
                continue;
            }
        };
        // The issuer's origin: frameworks that refuse cross-site form posts (SvelteKit) let the
        // client trust it by name rather than turn their protection off.
        let origin = url::Url::parse(&st.config.realms[&client.realm].issuer).map(|u| u.origin().ascii_serialization());
        let mut request = http.post(&uri).form(&[("logout_token", &token)]);
        if let Ok(o) = origin {
            request = request.header("origin", o);
        }
        let result = request.send().await;
        let error = match result {
            Ok(r) if r.status().is_success() => String::new(),
            Ok(r) => format!("HTTP {}", r.status().as_u16()),
            // With its causes: "error sending request" alone does not say a certificate failed.
            Err(e) => {
                let mut text = e.to_string();
                let mut cause = std::error::Error::source(&e);
                while let Some(c) = cause {
                    text.push_str(": ");
                    text.push_str(&c.to_string());
                    cause = c.source();
                }
                text
            }
        };
        if error.is_empty() {
            done += 1;
        } else {
            tracing::warn!(client = %d.client_id, %error, "a back-channel logout was not accepted");
        }
        st.store.backchannel_result(d.id, &error).await?;
    }
    Ok(done)
}

// RP-initiated logout ------------------------------------------------------------------------

fn page(status: StatusCode, title: &str, text: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"robots\" content=\"noindex\">\
         <title>{title}</title></head><body style=\"font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
         <h1>{title}</h1><p>{text}</p></body></html>"
    );
    let mut r = (status, Html(body)).into_response();
    super::no_store(r.headers_mut());
    r
}

fn signed_out(realm: &Realm) -> Response {
    let mut r = page(StatusCode::OK, "You are signed out", "You can close this page.");
    r.headers_mut().append(header::SET_COOKIE, clear_cookie(realm, SID));
    r
}

/// What an id_token_hint says: pacod-id signed it for this realm (expired is fine).
pub(crate) struct Hint {
    pub client: String,
    pub sid: Option<String>,
    pub sub: String,
}

pub(crate) fn read_hint(st: &Shared, realm: &Realm, token: &str) -> Option<Hint> {
    let v = st.keys.verifier().verify(token, &[Alg::RS256, Alg::ES256]).ok()?;
    if v.header.typ.as_deref() != Some("JWT") || v.claims["iss"] != realm.issuer.as_str() {
        return None;
    }
    Some(Hint {
        client: v.claims["aud"].as_str()?.to_owned(),
        sid: v.claims["sid"].as_str().map(str::to_owned),
        sub: v.claims["sub"].as_str()?.to_owned(),
    })
}

async fn rp_logout_get(
    State(st): State<Shared>,
    headers: HeaderMap,
    Query(p): Query<BTreeMap<String, String>>,
) -> Response {
    rp_logout(&st, &headers, p).await
}

async fn rp_logout_post(
    State(st): State<Shared>,
    headers: HeaderMap,
    Form(p): Form<BTreeMap<String, String>>,
) -> Response {
    rp_logout(&st, &headers, p).await
}

async fn rp_logout(st: &Shared, headers: &HeaderMap, p: BTreeMap<String, String>) -> Response {
    match run_logout(st, headers, p).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "logout failed");
            page(StatusCode::INTERNAL_SERVER_ERROR, "Signing out failed", "Something went wrong on our side.")
        }
    }
}

async fn run_logout(st: &Shared, headers: &HeaderMap, p: BTreeMap<String, String>) -> anyhow::Result<Response> {
    let Some(realm) = realm_by_host(st, headers) else {
        return Ok(page(StatusCode::NOT_FOUND, "Unknown address", "No sign-in service is configured at this address."));
    };
    let hint = match p.get("id_token_hint") {
        Some(t) => match read_hint(st, realm, t) {
            Some(h) => Some(h),
            None => {
                return Ok(page(
                    StatusCode::BAD_REQUEST,
                    "Signing out failed",
                    "The application sent a token this service did not issue.",
                ));
            }
        },
        None => None,
    };
    let client = match (hint.as_ref().map(|h| h.client.as_str()), p.get("client_id").map(String::as_str)) {
        (Some(a), Some(b)) if a != b => {
            return Ok(page(
                StatusCode::BAD_REQUEST,
                "Signing out failed",
                "The application's request does not fit together.",
            ));
        }
        (Some(a), _) | (None, Some(a)) => st.config.clients.get(a).filter(|c| c.realm == realm.name),
        (None, None) => None,
    };
    // Back to the application only to an address it registered (no open redirect).
    let target = match p.get("post_logout_redirect_uri") {
        Some(uri) => match client {
            Some(c) if c.post_logout_redirect_uris.contains(uri) => {
                let mut params = vec![];
                if let Some(s) = p.get("state") {
                    params.push(("state", s.as_str()));
                }
                Some(with_query(uri, &params))
            }
            _ => {
                return Ok(page(
                    StatusCode::BAD_REQUEST,
                    "Signing out failed",
                    "The application's return address is not registered.",
                ));
            }
        },
        None => None,
    };
    let session = match read_cookie(headers, &cookie_name(realm, SID)) {
        Some(c) => {
            st.store.session_by_cookie(&sha256(c.as_bytes())).await?.filter(|s| s.is_live() && s.realm == realm.name)
        }
        None => None,
    };
    let finish = |target: Option<String>| -> Response {
        match target {
            Some(t) => {
                let mut r = redirect(&t);
                r.headers_mut().append(header::SET_COOKIE, clear_cookie(realm, SID));
                r
            }
            None => signed_out(realm),
        }
    };
    match (&session, &hint) {
        // The application proves who it signs out: done at once.
        (Some(s), Some(h)) if h.sid.as_deref() == Some(s.id.as_str()) || h.sub == s.account_id => {
            end_session(st, &s.id, "logout").await?;
            audit_logout(st, realm, &h.client, &s.account_id, &s.id).await?;
            Ok(finish(target))
        }
        // A session, but no proof from the application: the person confirms on the UI page.
        (Some(_), _) => {
            let mut q: Vec<(&str, &str)> = vec![];
            for k in ["client_id", "post_logout_redirect_uri", "state", "ui_locales"] {
                if let Some(v) = p.get(k) {
                    q.push((k, v));
                }
            }
            if let (Some(c), false) = (client, p.contains_key("client_id")) {
                q.push(("client_id", c.id.as_str()));
            }
            Ok(redirect(&with_query(&format!("{}/logout", realm.issuer), &q)))
        }
        // No session in this browser: end the one the hint names, if it lives.
        (None, Some(h)) => {
            if let Some(sid) = &h.sid
                && st.store.session(sid).await?.is_some_and(|s| s.is_live() && s.account_id == h.sub)
            {
                end_session(st, sid, "logout").await?;
                audit_logout(st, realm, &h.client, &h.sub, sid).await?;
            }
            Ok(finish(target))
        }
        (None, None) => Ok(finish(target)),
    }
}

async fn audit_logout(st: &Shared, realm: &Realm, client: &str, account: &str, sid: &str) -> anyhow::Result<()> {
    st.store
        .audit(&AuditEntry {
            realm: Some(realm.name.clone()),
            client_id: Some(client.to_owned()),
            ..AuditEntry::new("logout").by(account, "user").subject(sid)
        })
        .await?;
    Ok(())
}

#[derive(Deserialize)]
struct UiLogout {
    client_id: Option<String>,
    post_logout_redirect_uri: Option<String>,
    state: Option<String>,
}

/// The confirmation on the hosted UI's sign-out page.
async fn ui_logout(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<UiLogout>,
) -> ApiResult<Response> {
    let realm = realm_by_host(&st, &headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    super::flows::same_origin(realm, &headers)?;
    let client = body.client_id.as_deref().and_then(|c| st.config.clients.get(c)).filter(|c| c.realm == realm.name);
    let to = match (&body.post_logout_redirect_uri, client) {
        (Some(uri), Some(c)) if c.post_logout_redirect_uris.contains(uri) => {
            let mut params = vec![];
            if let Some(s) = &body.state {
                params.push(("state", s.as_str()));
            }
            Some(with_query(uri, &params))
        }
        (Some(_), _) => return Err(ApiError::bad_request("the return address is not registered for the application")),
        (None, _) => None,
    };
    if let Some(c) = read_cookie(&headers, &cookie_name(realm, SID))
        && let Some(s) =
            st.store.session_by_cookie(&sha256(c.as_bytes())).await?.filter(|s| s.is_live() && s.realm == realm.name)
    {
        end_session(&st, &s.id, "logout").await?;
        let caller = Caller::of(&st, peer, &headers);
        st.store
            .audit(&AuditEntry {
                realm: Some(realm.name.clone()),
                ip: caller.ip,
                ..AuditEntry::new("logout").by(&s.account_id, "user").subject(&s.id)
            })
            .await?;
    }
    let body = match to {
        Some(t) => json!({ "next": "redirect", "to": t }),
        None => json!({ "next": "signed_out" }),
    };
    let mut r = Json(body).into_response();
    r.headers_mut().append(header::SET_COOKIE, clear_cookie(realm, SID));
    super::no_store(r.headers_mut());
    Ok(r)
}

/// A BFF or a product's service ends a session server to server: a product's service any session,
/// another client (a confidential one, a service without a product) one it had a grant in.
async fn revoke_session(
    State(st): State<Shared>,
    caller: CallerClient,
    Path(sid): Path<String>,
) -> ApiResult<StatusCode> {
    let products_service = caller.is_service() && caller.product.is_some();
    if !products_service && !st.store.client_had_session(&caller.id, &sid).await? {
        return Err(ApiError::forbidden("the client had no grant in this session"));
    }
    end_session(&st, &sid, "logout").await?;
    st.store
        .audit(&AuditEntry {
            client_id: Some(caller.id.clone()),
            ..AuditEntry::new("session_revoked").by(&caller.id, "client").subject(&sid)
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
