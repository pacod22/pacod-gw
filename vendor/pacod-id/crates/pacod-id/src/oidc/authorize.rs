//! The authorization endpoint. GET and POST, code flow with PKCE (S256) only.
//!
//! Checks come in an order that matters: an unknown client or a redirect URI that is not
//! registered exactly gets an error page, never a redirect (a redirect would go wherever the
//! request said). Every later error goes back to the client with `error`, `state` and `iss`.

use std::collections::BTreeMap;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use chrono::{Duration, Utc};

use super::{
    FLOW, Request, SID, cookie_name, issue_code, read_cookie, realm_by_host, redirect, set_cookie, valid_challenge,
    with_query,
};
use crate::config::{ClientKind, Grant, Realm};
use crate::crypto::{sha256, tokens};
use crate::http::Shared;
use crate::store::Session;

/// How long a sign-in may take, from authorize to the code.
pub const FLOW_LIFETIME: Duration = Duration::minutes(15);

fn error_page(status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"robots\" content=\"noindex\">\
         <title>Sign-in error</title></head><body style=\"font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
         <h1>The sign-in cannot start</h1><p>{}</p><p>Go back to the application and try again.</p></body></html>",
        message.replace('<', "&lt;")
    );
    let mut r = (status, Html(body)).into_response();
    super::no_store(r.headers_mut());
    r
}

/// The error redirect of RFC 6749 4.1.2.1 with the issuer of RFC 9207.
fn error_redirect(realm: &Realm, uri: &str, state: Option<&String>, error: &str, description: &str) -> Response {
    let mut params = vec![("error", error), ("error_description", description), ("iss", realm.issuer.as_str())];
    if let Some(s) = state {
        params.push(("state", s));
    }
    redirect(&with_query(uri, &params))
}

pub async fn get(
    State(st): State<Shared>,
    headers: HeaderMap,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    authorize(&st, &headers, params).await
}

pub async fn post(State(st): State<Shared>, headers: HeaderMap, Form(params): Form<Vec<(String, String)>>) -> Response {
    authorize(&st, &headers, params).await
}

async fn authorize(st: &Shared, headers: &HeaderMap, raw: Vec<(String, String)>) -> Response {
    match run(st, headers, raw).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "authorize failed");
            error_page(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on our side.")
        }
    }
}

async fn run(st: &Shared, headers: &HeaderMap, raw: Vec<(String, String)>) -> anyhow::Result<Response> {
    // A parameter given twice is an error (RFC 6749, 3.1); without a trustworthy redirect URI
    // yet, it can only be shown.
    let mut p: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in raw {
        if p.insert(k.clone(), v).is_some() {
            return Ok(error_page(StatusCode::BAD_REQUEST, &format!("The parameter {k} appears twice.")));
        }
    }
    let Some(realm) = realm_by_host(st, headers) else {
        return Ok(error_page(StatusCode::NOT_FOUND, "No sign-in service is configured at this address."));
    };
    let Some(client) = p.get("client_id").and_then(|id| st.config.clients.get(id)) else {
        return Ok(error_page(StatusCode::BAD_REQUEST, "The application is not known here."));
    };
    if client.realm != realm.name
        || client.kind == ClientKind::Service
        || !client.grants().contains(&Grant::AuthorizationCode)
    {
        return Ok(error_page(StatusCode::BAD_REQUEST, "The application does not sign people in here."));
    }
    let Some(redirect_uri) = p.get("redirect_uri").filter(|u| client.redirect_uris.contains(u)).cloned() else {
        return Ok(error_page(StatusCode::BAD_REQUEST, "The application's return address is not registered."));
    };
    let state = p.get("state");
    let fail = |error: &str, description: &str| Ok(error_redirect(realm, &redirect_uri, state, error, description));

    if p.contains_key("request") {
        return fail("request_not_supported", "request objects are not supported");
    }
    if p.contains_key("request_uri") {
        return fail("request_uri_not_supported", "request_uri is not supported");
    }
    if p.get("response_type").map(String::as_str) != Some("code") {
        return fail("unsupported_response_type", "only the authorization code flow (response_type=code) is offered");
    }
    if p.get("response_mode").is_some_and(|m| m != "query") {
        return fail("invalid_request", "only response_mode=query is supported");
    }
    let scope = p.get("scope").cloned().unwrap_or_default();
    if !scope.split(' ').any(|s| s == "openid") {
        return fail("invalid_scope", "the openid scope is required");
    }
    let Some(challenge) = p.get("code_challenge") else {
        return fail("invalid_request", "PKCE is required: send code_challenge with code_challenge_method=S256");
    };
    if p.get("code_challenge_method").map(String::as_str) != Some("S256") || !valid_challenge(challenge) {
        return fail("invalid_request", "PKCE needs code_challenge_method=S256 and a 43-character challenge");
    }
    let prompt: Vec<String> = p
        .get("prompt")
        .map(|s| s.split(' ').filter(|x| !x.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    if prompt.iter().any(|x| !["none", "login", "consent", "select_account", "create"].contains(&x.as_str())) {
        return fail("invalid_request", "unknown prompt value");
    }
    if prompt.contains(&"none".to_owned()) && prompt.len() > 1 {
        return fail("invalid_request", "prompt=none cannot be combined with other values");
    }
    // id_token_hint names who the client expects (OIDC Core 3.1.2.1): a session of anyone else
    // is not used. A hint pacod-id did not sign for this realm is an error.
    let hint_sub = match p.get("id_token_hint") {
        None => None,
        Some(h) => match super::logout::read_hint(st, realm, h) {
            Some(hint) => Some(hint.sub),
            None => return fail("invalid_request", "id_token_hint is not an id_token of this issuer"),
        },
    };
    let max_age = match p.get("max_age").map(|m| m.parse::<i64>()) {
        None => None,
        Some(Ok(m)) if m >= 0 => Some(m),
        Some(_) => return fail("invalid_request", "max_age is a number of seconds"),
    };
    let request = Request {
        redirect_uri: redirect_uri.clone(),
        scope: scope.split(' ').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "),
        state: state.cloned(),
        nonce: p.get("nonce").cloned(),
        code_challenge: challenge.clone(),
        prompt: prompt.clone(),
        max_age,
        login_hint: p.get("login_hint").cloned(),
        ui_locales: p.get("ui_locales").cloned(),
        remember_me: false,
    };

    // A usable single sign-on session: this realm, live, its account active, recent enough.
    let session: Option<Session> = match read_cookie(headers, &cookie_name(realm, SID)) {
        Some(c) => {
            st.store.session_by_cookie(&sha256(c.as_bytes())).await?.filter(|s| s.is_live() && s.realm == realm.name)
        }
        None => None,
    };
    let account = match &session {
        Some(s) => st.store.account(&s.account_id).await?.filter(|a| a.is_active()),
        None => None,
    };
    let (session, account) = match (session, account) {
        (Some(_), Some(a)) if hint_sub.as_deref().is_some_and(|h| h != a.id) => (None, None),
        other => other,
    };
    // max_age=0 asks for a new sign-in, whatever the clock says (OIDC Core 3.1.2.1).
    let fresh_enough = |s: &Session| max_age.is_none_or(|m| m > 0 && (Utc::now() - s.auth_time).num_seconds() <= m);
    let wants_login = prompt.iter().any(|x| x == "login" || x == "create");
    if let (Some(s), Some(a)) = (&session, &account)
        && fresh_enough(s)
        && !wants_login
    {
        if a.password_change_due {
            if prompt.iter().any(|x| x == "none") {
                return fail("interaction_required", "the account must choose a new password first");
            }
        } else {
            st.store.session_touch(&s.id).await?;
            let to = issue_code(st, realm, client, &request, a, &s.id, s.auth_time)
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?;
            return Ok(redirect(&to));
        }
    }
    if prompt.iter().any(|x| x == "none") {
        return fail("login_required", "nobody is signed in here");
    }

    // A sign-in flow, bound to this browser by a cookie.
    let flow_id = tokens::new_id("flw");
    let browser = tokens::opaque();
    let (stage, bound) = match (&session, &account) {
        // Signed in moments ago, but the password must change first: the flow starts there. An
        // older session proves too little to set a password without the current one (a stolen
        // cookie): the person signs in again, and the password step leads to the change.
        (Some(s), Some(a))
            if a.password_change_due
                && fresh_enough(s)
                && !wants_login
                && s.auth_time > Utc::now() - crate::me::RECENT =>
        {
            ("change_password", Some(a.id.as_str()))
        }
        _ => ("start", None),
    };
    st.store
        .flow_create(
            &flow_id,
            &realm.name,
            &client.id,
            &serde_json::to_value(&request)?,
            &sha256(browser.as_bytes()),
            stage,
            bound,
            Utc::now() + FLOW_LIFETIME,
        )
        .await?;
    let page = if prompt.iter().any(|x| x == "create") { "signup" } else { "login" };
    let mut r = redirect(&format!("{}/{page}?flow={flow_id}", realm.issuer));
    r.headers_mut().append(header::SET_COOKIE, set_cookie(realm, FLOW, &browser, Some(FLOW_LIFETIME.num_seconds())));
    Ok(r)
}
