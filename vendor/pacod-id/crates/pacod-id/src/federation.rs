//! Google sign-in: one Google client per realm; the authorization code flow with
//! PKCE, `state` and `nonce`; the id_token checked against Google's keys. Accounts are keyed by
//! Google's `sub`. Linking to an account by address needs Google's `email_verified` and either
//! the account's own confirmed address or its password now. Where accounts come by invitation
//! (the gateway realm), Google makes one only for an address with a pending invitation. An
//! account keeps at least one way to sign in.
//!
//! Any OpenID provider works the same way: tests and the development stack use a mock.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use chrono::Utc;
use pacod_id_jose::{Alg, JwkSet, Verifier};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use tokio::sync::Mutex;

use crate::config::{Realm, Upstream};
use crate::crypto::{sha256, tokens};
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::oidc::flows::{flow_of_pub, same_origin, send_verification};
use crate::oidc::{Caller, Request, browser_session, realm_by_host, sign_in};
use crate::store::{Account, AuditEntry, Flow, NewAccount, StoreError};

pub const PROVIDER: &str = "google";
/// Discovery is read again after this; keys at most this often when an unknown one shows up.
const DISCOVERY_FOR: StdDuration = StdDuration::from_secs(3600);
const KEYS_AT_MOST_EVERY: StdDuration = StdDuration::from_secs(60);
const LEEWAY: i64 = 60;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/ui-api/v1/flows/{id}/google", post(start))
        .route("/federation/google/callback", get(callback))
        .route("/ui-api/v1/account/connections", get(connections))
        .route("/ui-api/v1/account/connections/google", delete(unlink))
}

/// What pacod-id knows of a realm's provider.
struct Known {
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    discovered: Instant,
    keys: Arc<Verifier>,
    keys_read: Option<Instant>,
}

/// The providers' discovery documents and keys, by realm.
#[derive(Default)]
pub struct Upstreams {
    known: Mutex<BTreeMap<String, Known>>,
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(StdDuration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("an HTTP client")
}

/// `url` with its origin replaced by `server`'s, for a provider reached at another address than
/// the one browsers use (a container).
fn rebase(url: &str, server: Option<&str>) -> String {
    let Some(server) = server else { return url.to_owned() };
    match (url::Url::parse(url), url::Url::parse(server)) {
        (Ok(mut u), Ok(s)) => {
            let _ = u.set_scheme(s.scheme());
            let _ = u.set_host(s.host_str());
            let _ = u.set_port(s.port());
            u.into()
        }
        _ => url.to_owned(),
    }
}

impl Upstreams {
    /// The provider's endpoints, from discovery (cached for an hour).
    async fn endpoints(&self, realm: &Realm, up: &Upstream) -> anyhow::Result<(String, String)> {
        let mut known = self.known.lock().await;
        if let Some(k) = known.get(&realm.name)
            && k.discovered.elapsed() < DISCOVERY_FOR
        {
            return Ok((k.authorization_endpoint.clone(), k.token_endpoint.clone()));
        }
        let base = up.server_url.as_deref().unwrap_or(&up.issuer).trim_end_matches('/');
        let doc: Value = http()
            .get(format!("{base}/.well-known/openid-configuration"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        anyhow::ensure!(doc["issuer"] == up.issuer.as_str(), "the provider's discovery names another issuer");
        let field = |k: &str| doc[k].as_str().map(str::to_owned).ok_or_else(|| anyhow::anyhow!("discovery lacks {k}"));
        let k = Known {
            authorization_endpoint: field("authorization_endpoint")?,
            token_endpoint: rebase(&field("token_endpoint")?, up.server_url.as_deref()),
            jwks_uri: rebase(&field("jwks_uri")?, up.server_url.as_deref()),
            discovered: Instant::now(),
            keys: Arc::new(Verifier::default()),
            keys_read: None,
        };
        let out = (k.authorization_endpoint.clone(), k.token_endpoint.clone());
        known.insert(realm.name.clone(), k);
        Ok(out)
    }

    /// The provider's keys, read again when `kid` is unknown (at most once a minute).
    async fn keys(&self, realm: &Realm, kid: &str) -> anyhow::Result<Arc<Verifier>> {
        let mut known = self.known.lock().await;
        let k = known.get_mut(&realm.name).ok_or_else(|| anyhow::anyhow!("the provider was not discovered"))?;
        if !k.keys.has_kid(kid) && k.keys_read.is_none_or(|t| t.elapsed() > KEYS_AT_MOST_EVERY) {
            let set: JwkSet = http().get(&k.jwks_uri).send().await?.error_for_status()?.json().await?;
            k.keys = Arc::new(Verifier::from_jwks(&set));
            k.keys_read = Some(Instant::now());
        }
        Ok(k.keys.clone())
    }
}

/// The realm's provider, when it has one.
fn upstream(realm: &Realm) -> ApiResult<&Upstream> {
    realm.google.as_ref().ok_or_else(|| {
        ApiError::not_found("Google sign-in is not offered here").with_details(json!({ "code": "google_off" }))
    })
}

fn callback_uri(realm: &Realm) -> String {
    format!("{}/federation/google/callback", realm.issuer)
}

/// What the flow keeps of a Google sign-in in progress.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
struct Pending {
    state: String,
    nonce: String,
    verifier: String,
}

/// Starts Google sign-in for a flow: the browser goes to the provider.
async fn start(State(st): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult<Response> {
    let (realm, flow) = flow_of_pub(&st, &headers, &id).await?;
    same_origin(realm, &headers)?;
    let up = upstream(realm)?;
    if !matches!(flow.stage.as_str(), "start" | "google" | "link_google") {
        return Err(
            ApiError::conflict("this sign-in is past its first step").with_details(json!({ "code": "wrong_stage" }))
        );
    }
    let (authorize, _) = st.upstreams.endpoints(realm, up).await.map_err(|e| {
        tracing::warn!(error = %e, "the Google provider cannot be reached");
        ApiError::unavailable("Google sign-in is not available just now")
            .with_details(json!({ "code": "google_unavailable" }))
    })?;
    let pending = Pending {
        state: format!("{}.{}", flow.id, tokens::opaque()),
        nonce: tokens::opaque(),
        verifier: tokens::opaque(),
    };
    let challenge = B64.encode(sha256(pending.verifier.as_bytes()));
    let mut request = flow.request.clone();
    request["google"] = serde_json::to_value(&pending).map_err(ApiError::internal)?;
    st.store.flow_set_request(&flow.id, &request, "google", None).await?;
    let req: Request = serde_json::from_value(flow.request.clone()).map_err(ApiError::internal)?;
    let mut u = url::Url::parse(&authorize).map_err(ApiError::internal)?;
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &up.client_id)
            .append_pair("redirect_uri", &callback_uri(realm))
            .append_pair("scope", "openid email profile")
            .append_pair("state", &pending.state)
            .append_pair("nonce", &pending.nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("prompt", "select_account");
        if let Some(hint) = &req.login_hint {
            q.append_pair("login_hint", hint);
        }
    }
    Ok(Json(json!({ "next": "redirect", "to": String::from(u) })).into_response())
}

/// What the provider's id_token says, once checked.
#[derive(Debug, Clone)]
struct Said {
    sub: String,
    email: String,
    email_verified: bool,
    name: String,
}

async fn check_id_token(st: &Shared, realm: &Realm, up: &Upstream, token: &str, nonce: &str) -> anyhow::Result<Said> {
    let header = pacod_id_jose::decode_header(token)?;
    let keys = st.upstreams.keys(realm, &header.kid).await?;
    let v = keys.verify(token, &[Alg::RS256])?;
    let c = &v.claims;
    let now = Utc::now().timestamp();
    // Google writes its issuer with and without the scheme.
    let iss_ok = c["iss"] == up.issuer.as_str()
        || (up.issuer == "https://accounts.google.com" && c["iss"] == "accounts.google.com");
    // Several audiences: the token must have been issued to this client (azp, OIDC Core 3.1.3.7).
    let aud_ok = c["aud"] == up.client_id.as_str()
        || (c["aud"].as_array().is_some_and(|a| a.iter().any(|x| x == up.client_id.as_str()))
            && c["azp"] == up.client_id.as_str());
    anyhow::ensure!(iss_ok, "the id_token is from another issuer");
    anyhow::ensure!(aud_ok, "the id_token is for another client");
    anyhow::ensure!(c["exp"].as_i64().is_some_and(|e| e + LEEWAY > now), "the id_token has expired");
    anyhow::ensure!(c["iat"].as_i64().is_some_and(|i| i - LEEWAY < now), "the id_token is from the future");
    anyhow::ensure!(c["nonce"] == nonce, "the id_token's nonce does not match");
    let sub = c["sub"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| anyhow::anyhow!("no sub"))?.to_owned();
    let email = crate::store::normalize_email(c["email"].as_str().ok_or_else(|| anyhow::anyhow!("no email"))?);
    let email_verified = c["email_verified"] == true || c["email_verified"] == "true";
    Ok(Said { sub, email, email_verified, name: c["name"].as_str().unwrap_or_default().to_owned() })
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Back to the sign-in page of the flow, with what went wrong.
fn back(realm: &Realm, flow: &str, error: &str) -> Response {
    let mut u = url::Url::parse(&format!("{}/login", realm.issuer)).expect("a checked issuer");
    u.query_pairs_mut().append_pair("flow", flow).append_pair("error", error);
    Redirect::to(u.as_str()).into_response()
}

fn page(status: StatusCode, text: &str) -> Response {
    let mut r = (
        status,
        axum::response::Html(format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Sign-in</title></head>\
             <body style=\"font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem\"><h1>Sign-in</h1><p>{text}</p></body></html>"
        )),
    )
        .into_response();
    crate::oidc::no_store(r.headers_mut());
    r
}

/// The provider sends the browser back here.
async fn callback(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    match run_callback(&st, peer, &headers, q).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e.message, "a Google sign-in failed");
            page(
                StatusCode::BAD_REQUEST,
                "The sign-in could not be completed. Go back to the application and start again.",
            )
        }
    }
}

async fn run_callback(st: &Shared, peer: SocketAddr, headers: &HeaderMap, q: CallbackQuery) -> ApiResult<Response> {
    let state = q.state.ok_or_else(|| ApiError::bad_request("no state"))?;
    let (flow_id, _) = state.split_once('.').ok_or_else(|| ApiError::bad_request("a malformed state"))?;
    let (realm, flow) = flow_of_pub(st, headers, flow_id).await?;
    let up = upstream(realm)?;
    let pending: Pending = serde_json::from_value(flow.request["google"].clone())
        .map_err(|_| ApiError::bad_request("no Google sign-in in this flow"))?;
    if flow.stage != "google" || !crate::crypto::ct_eq(pending.state.as_bytes(), state.as_bytes()) {
        return Err(ApiError::bad_request("the state does not match"));
    }
    // One use of what the flow kept, whatever happens next.
    let mut request = flow.request.clone();
    request.as_object_mut().map(|o| o.remove("google"));
    st.store.flow_set_request(&flow.id, &request, "start", None).await?;
    if q.error.is_some() {
        return Ok(back(realm, &flow.id, "google_cancelled"));
    }
    let code = q.code.ok_or_else(|| ApiError::bad_request("no code"))?;
    let (_, token_endpoint) = st.upstreams.endpoints(realm, up).await.map_err(ApiError::internal)?;
    let secret =
        st.boot.google_secrets.get(&realm.name).ok_or_else(|| ApiError::internal("no Google client secret"))?;
    let answer: Value = http()
        .post(&token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", callback_uri(realm).as_str()),
            ("code_verifier", pending.verifier.as_str()),
            ("client_id", up.client_id.as_str()),
            ("client_secret", secret.as_str()),
        ])
        .send()
        .await
        .map_err(ApiError::internal)?
        .json()
        .await
        .map_err(ApiError::internal)?;
    let id_token = answer["id_token"]
        .as_str()
        .ok_or_else(|| ApiError::bad_request(format!("the provider answered {}", answer["error"])))?;
    let said = check_id_token(st, realm, up, id_token, &pending.nonce)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let flow = Flow { request, stage: "start".into(), ..flow };
    let caller = Caller::of(st, peer, headers);
    resolve(st, realm, &flow, said, &caller, headers).await
}

/// Who the provider's identity is here, and what happens next.
async fn resolve(
    st: &Shared,
    realm: &Realm,
    flow: &Flow,
    said: Said,
    caller: &Caller,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    let audit = |action: &str, subject: &str, detail: Value| AuditEntry {
        realm: Some(realm.name.clone()),
        client_id: Some(flow.client_id.clone()),
        ip: caller.ip.clone(),
        user_agent: caller.user_agent.clone(),
        ..AuditEntry::new(action).subject(subject).detail(detail)
    };
    let linked: Option<String> =
        sqlx::query_scalar("SELECT account_id FROM federated_identities WHERE provider = $1 AND provider_subject = $2")
            .bind(PROVIDER)
            .bind(&said.sub)
            .fetch_optional(st.store.pool())
            .await?;
    let account = match linked {
        Some(id) => st.store.account(&id).await?.ok_or(StoreError::NotFound)?,
        None => match st.store.account_by_email(&said.email).await? {
            // The address has an account: linked when both sides proved it, else after the
            // account's password.
            Some(a) if said.email_verified && a.is_verified() => {
                link(st, &a, &said).await?;
                st.store.audit(&audit("google_linked", &a.id, json!({ "how": "verified_address" }))).await?;
                a
            }
            Some(a) if said.email_verified => {
                let mut request = flow.request.clone();
                request["google_pending"] = json!({ "sub": said.sub, "email": said.email });
                st.store.flow_set_request(&flow.id, &request, "link_google", Some(&a.id)).await?;
                let mut u = url::Url::parse(&format!("{}/login", realm.issuer)).expect("a checked issuer");
                u.query_pairs_mut()
                    .append_pair("flow", &flow.id)
                    .append_pair("link", "google")
                    .append_pair("email", &a.email);
                return Ok(Redirect::to(u.as_str()).into_response());
            }
            Some(_) => return Ok(back(realm, &flow.id, "google_unverified")),
            None => {
                if !said.email_verified {
                    return Ok(back(realm, &flow.id, "google_unverified"));
                }
                let open = crate::oidc::self_signup(st, realm, flow)?;
                let invited: bool =
                    sqlx::query("SELECT 1 FROM invitations WHERE email = $1 AND realm = $2 AND expires_at > now()")
                        .bind(&said.email)
                        .bind(&realm.name)
                        .fetch_optional(st.store.pool())
                        .await?
                        .is_some();
                if !open && !invited {
                    st.store
                        .audit(&audit("login_failed", &said.email, json!({ "reason": "google_no_account" })))
                        .await?;
                    return Ok(back(realm, &flow.id, "google_no_account"));
                }
                let a = match st
                    .store
                    .account_create(&NewAccount {
                        id: tokens::new_id("usr"),
                        email: said.email.clone(),
                        display_name: said.name.chars().take(200).collect(),
                        locale: realm.default_locale.clone(),
                        password_hash: None,
                        verified_by: Some("federated".into()),
                        signup_realm: realm.name.clone(),
                    })
                    .await
                {
                    Err(StoreError::Conflict(_)) => return Ok(back(realm, &flow.id, "google_retry")),
                    r => r?,
                };
                link(st, &a, &said).await?;
                st.store
                    .audit(
                        &audit("account_created", &a.id, json!({ "how": "google", "invited": invited }))
                            .by(&a.id, "user"),
                    )
                    .await?;
                a
            }
        },
    };
    if !account.is_active() {
        st.store.audit(&audit("login_failed", &account.id, json!({ "reason": "disabled" }))).await?;
        return Ok(back(realm, &flow.id, "account_disabled"));
    }
    sqlx::query("UPDATE federated_identities SET last_used_at = now() WHERE provider = $1 AND provider_subject = $2")
        .bind(PROVIDER)
        .bind(&said.sub)
        .execute(st.store.pool())
        .await?;
    finish(st, realm, flow, &account, caller, headers).await
}

/// Signs the account in after the provider, or detours to confirm its address.
async fn finish(
    st: &Shared,
    realm: &Realm,
    flow: &Flow,
    account: &Account,
    caller: &Caller,
    headers: &HeaderMap,
) -> ApiResult<Response> {
    if realm.require_verified_email && st.mail.is_some() && !account.is_verified() {
        send_verification(st, realm, account).await?;
        st.store.flow_set_stage(&flow.id, "verify_email", Some(&account.id)).await?;
        let mut u = url::Url::parse(&format!("{}/login", realm.issuer)).expect("a checked issuer");
        u.query_pairs_mut().append_pair("flow", &flow.id).append_pair("next", "verify_email");
        return Ok(Redirect::to(u.as_str()).into_response());
    }
    // A role holder whose password is too short chooses a new one first, as after a password
    // sign-in: Google has just proven who this is.
    if account.password_change_due {
        st.store.flow_set_stage(&flow.id, "change_password", Some(&account.id)).await?;
        let mut u = url::Url::parse(&format!("{}/login", realm.issuer)).expect("a checked issuer");
        u.query_pairs_mut().append_pair("flow", &flow.id);
        return Ok(Redirect::to(u.as_str()).into_response());
    }
    let request: Request = serde_json::from_value(flow.request.clone()).map_err(ApiError::internal)?;
    let done = sign_in(st, realm, flow, account, request.remember_me, &["fed".to_owned()], caller, headers).await?;
    let mut r = Redirect::to(&done.to).into_response();
    for c in done.cookies {
        r.headers_mut().append(header::SET_COOKIE, c);
    }
    crate::oidc::no_store(r.headers_mut());
    Ok(r)
}

async fn link(st: &Shared, account: &Account, said: &Said) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO federated_identities (provider, provider_subject, account_id, email_at_link) VALUES ($1, $2, $3, $4)
         ON CONFLICT (provider, provider_subject) DO NOTHING",
    )
    .bind(PROVIDER)
    .bind(&said.sub)
    .bind(&account.id)
    .bind(&said.email)
    .execute(st.store.pool())
    .await?;
    Ok(())
}

/// Links the Google identity a flow waits on, once the account's password was given. The
/// address counts as proven: Google confirmed it, and its owner holds the account.
pub async fn link_pending(st: &Shared, flow: &Flow, account: &Account) -> ApiResult<()> {
    let p = &flow.request["google_pending"];
    let (Some(sub), Some(email)) = (p["sub"].as_str(), p["email"].as_str()) else { return Ok(()) };
    if email != account.email {
        return Ok(());
    }
    let said = Said { sub: sub.to_owned(), email: email.to_owned(), email_verified: true, name: String::new() };
    link(st, account, &said).await?;
    if !account.is_verified() {
        st.store.account_mark_verified(&account.id, "federated").await?;
    }
    st.store
        .audit(
            &AuditEntry {
                realm: Some(flow.realm.clone()),
                ..AuditEntry::new("google_linked").by(&account.id, "user").subject(&account.id)
            }
            .detail(json!({ "how": "password" })),
        )
        .await?;
    Ok(())
}

/// The signed-in person's ways to sign in.
async fn connections(State(st): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    let realm = realm_by_host(&st, &headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    let (_, account) = browser_session(&st, realm, &headers)
        .await?
        .ok_or_else(|| ApiError::unauthorized("sign in first").with_details(json!({ "code": "sign_in_required" })))?;
    let google = sqlx::query("SELECT email_at_link, created_at, last_used_at FROM federated_identities WHERE account_id = $1 AND provider = $2")
        .bind(&account.id)
        .bind(PROVIDER)
        .fetch_optional(st.store.pool())
        .await?
        .map(|r| -> Result<Value, sqlx::Error> {
            Ok(json!({
                "email": r.try_get::<String, _>("email_at_link")?,
                "linked_at": r.try_get::<chrono::DateTime<Utc>, _>("created_at")?,
                "last_used_at": r.try_get::<Option<chrono::DateTime<Utc>>, _>("last_used_at")?,
            }))
        })
        .transpose()?;
    Ok(Json(json!({ "password": account.password_hash.is_some(), "google": google, "google_offered": realm.google.is_some() })).into_response())
}

/// Unlinks Google, unless it is the account's only way to sign in.
async fn unlink(State(st): State<Shared>, headers: HeaderMap) -> ApiResult<StatusCode> {
    let realm = realm_by_host(&st, &headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    same_origin(realm, &headers)?;
    let (_, account) = browser_session(&st, realm, &headers)
        .await?
        .ok_or_else(|| ApiError::unauthorized("sign in first").with_details(json!({ "code": "sign_in_required" })))?;
    if account.password_hash.is_none() {
        return Err(ApiError::conflict("set a password first: Google is this account's only way to sign in")
            .with_details(json!({ "code": "last_sign_in_method" })));
    }
    let gone = sqlx::query("DELETE FROM federated_identities WHERE account_id = $1 AND provider = $2")
        .bind(&account.id)
        .bind(PROVIDER)
        .execute(st.store.pool())
        .await?;
    if gone.rows_affected() > 0 {
        st.store
            .audit(&AuditEntry {
                realm: Some(realm.name.clone()),
                ..AuditEntry::new("google_unlinked").by(&account.id, "user").subject(&account.id)
            })
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
