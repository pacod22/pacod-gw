//! The OpenID Connect provider: authorize, the sign-in flow behind it, the
//! token endpoint, and what they share.

pub mod account;
pub mod authorize;
pub mod exchange;
pub mod flows;
pub mod logout;
pub mod token;
pub mod userinfo;

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

use axum::Json;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD as B64};
use chrono::{DateTime, Duration, Utc};
use pacod_id_jose::Alg;
use serde_json::{Value, json};
use url::Url;

use crate::config::{Client, ClientKind, Realm};
use crate::crypto::{ct_eq, sha256, tokens};
use crate::error::ApiError;
use crate::http::Shared;
use crate::mail::{self, Template};
use crate::store::{Account, AuditEntry, Flow, NewGrant, NewSession, Store, changes};

/// The lifetime of an authorization code.
pub const CODE_LIFETIME: Duration = Duration::seconds(60);
/// How long a repeated refresh gets the same successor.
pub const REFRESH_GRACE: Duration = Duration::seconds(30);
/// The lifetime of an id_token.
pub const ID_TOKEN_LIFETIME: Duration = Duration::minutes(5);

// OAuth errors ---------------------------------------------------------------------------------

/// An error of the token, revocation and userinfo endpoints (RFC 6749, 5.2).
#[derive(Debug)]
pub struct OAuthError {
    pub status: StatusCode,
    pub error: &'static str,
    pub description: String,
    /// The client authenticated with Basic: answer 401 with a challenge.
    pub challenge: Option<&'static str>,
}

impl OAuthError {
    pub fn new(error: &'static str, description: impl Into<String>) -> Self {
        OAuthError { status: StatusCode::BAD_REQUEST, error, description: description.into(), challenge: None }
    }

    pub fn invalid_request(d: impl Into<String>) -> Self {
        Self::new("invalid_request", d)
    }

    pub fn invalid_grant(d: impl Into<String>) -> Self {
        Self::new("invalid_grant", d)
    }

    pub fn invalid_client(basic: bool) -> Self {
        OAuthError {
            status: StatusCode::UNAUTHORIZED,
            error: "invalid_client",
            description: "client authentication failed".into(),
            challenge: basic.then_some(r#"Basic realm="pacod-id""#),
        }
    }

    pub fn server(e: impl std::fmt::Display) -> Self {
        tracing::error!(error = %e, "internal error at an OAuth endpoint");
        OAuthError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: "server_error",
            description: "an internal error occurred".into(),
            challenge: None,
        }
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut r =
            (self.status, Json(json!({ "error": self.error, "error_description": self.description }))).into_response();
        no_store(r.headers_mut());
        if let Some(c) = self.challenge {
            r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static(c));
        }
        r
    }
}

impl From<crate::store::StoreError> for OAuthError {
    fn from(e: crate::store::StoreError) -> Self {
        OAuthError::server(e)
    }
}

impl From<sqlx::Error> for OAuthError {
    fn from(e: sqlx::Error) -> Self {
        OAuthError::server(e)
    }
}

impl From<anyhow::Error> for OAuthError {
    fn from(e: anyhow::Error) -> Self {
        OAuthError::server(e)
    }
}

pub fn no_store(h: &mut HeaderMap) {
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

// Cookies -----------------------------------------------------------------------------------

/// The names of pacod-id's cookies on a realm's host: with the __Host- prefix when the realm is
/// served over https (it requires Secure), plain in development over http.
pub fn cookie_name(realm: &Realm, base: &str) -> String {
    if realm.issuer.starts_with("https://") { format!("__Host-{base}") } else { base.to_owned() }
}

pub const SID: &str = "pid_sid";
pub const FLOW: &str = "pid_flow";
pub const DEVICE: &str = "pid_dev";

pub fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

/// A Set-Cookie value: HttpOnly, SameSite=Lax, Secure over https; `max_age` makes it last
/// beyond the browser session.
pub fn set_cookie(realm: &Realm, base: &str, value: &str, max_age: Option<i64>) -> HeaderValue {
    let secure = if realm.issuer.starts_with("https://") { "; Secure" } else { "" };
    let age = max_age.map(|s| format!("; Max-Age={s}")).unwrap_or_default();
    HeaderValue::from_str(&format!("{}={value}; Path=/; HttpOnly; SameSite=Lax{secure}{age}", cookie_name(realm, base)))
        .expect("cookie values are base64url")
}

pub fn clear_cookie(realm: &Realm, base: &str) -> HeaderValue {
    set_cookie(realm, base, "", Some(0))
}

// Clients -----------------------------------------------------------------------------------

/// Decodes one part of HTTP Basic client credentials: form-urlencoded (RFC 6749, 2.3.1).
pub(crate) fn form_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("v={}", s.replace('&', "%26").replace('=', "%3D")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// Authenticates the client of a token, revocation or introspection request: HTTP Basic or
/// client_secret_post for clients with a secret, the client_id alone for public ones.
pub async fn authenticate_client<'a>(
    st: &'a Shared,
    headers: &HeaderMap,
    form: &BTreeMap<String, String>,
) -> Result<&'a Client, OAuthError> {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic ").or_else(|| v.strip_prefix("basic ")))
        .map(|b| STANDARD.decode(b.trim()).ok().and_then(|raw| String::from_utf8(raw).ok()));
    let (id, secret, by_basic) = match (basic, form.get("client_secret")) {
        (Some(_), Some(_)) => return Err(OAuthError::invalid_request("use one way of client authentication")),
        (Some(None), _) => return Err(OAuthError::invalid_client(true)),
        (Some(Some(pair)), None) => {
            let (id, secret) = pair.split_once(':').ok_or(OAuthError::invalid_client(true))?;
            let id = form_decode(id);
            if form.get("client_id").is_some_and(|f| *f != id) {
                return Err(OAuthError::invalid_request("client_id does not match the credentials"));
            }
            (id, Some(form_decode(secret)), true)
        }
        (None, secret) => {
            let id = form.get("client_id").cloned().ok_or_else(|| OAuthError::invalid_client(false))?;
            (id, secret.cloned(), false)
        }
    };
    let client = st.config.clients.get(&id).ok_or(OAuthError::invalid_client(by_basic))?;
    match (client.kind, secret) {
        (ClientKind::Public, None) => Ok(client),
        (ClientKind::Public, Some(_)) | (_, None) => Err(OAuthError::invalid_client(by_basic)),
        (_, Some(secret)) => {
            let stored = st.store.client_secrets().await?;
            let hash = st.boot.token_pepper.hash(&secret);
            let ok = stored.iter().any(|(c, h)| *c == client.id && ct_eq(h, &hash));
            if ok { Ok(client) } else { Err(OAuthError::invalid_client(by_basic)) }
        }
    }
}

// PKCE --------------------------------------------------------------------------------------

/// A code_verifier: 43 to 128 unreserved characters (RFC 7636, 4.1).
pub fn valid_verifier(v: &str) -> bool {
    (43..=128).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

/// A code_challenge for S256: the base64url of a SHA-256, 43 characters.
pub fn valid_challenge(c: &str) -> bool {
    c.len() == 43 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    valid_verifier(verifier) && ct_eq(B64.encode(sha256(verifier.as_bytes())).as_bytes(), challenge.as_bytes())
}

// Redirects ---------------------------------------------------------------------------------

/// `uri` with parameters added to its query.
pub fn with_query(uri: &str, params: &[(&str, &str)]) -> String {
    let mut u = Url::parse(uri).expect("registered redirect URIs are URLs");
    // No parameters: the address as registered, without an empty "?".
    if params.is_empty() {
        return u.into();
    }
    {
        let mut q = u.query_pairs_mut();
        for (k, v) in params {
            q.append_pair(k, v);
        }
    }
    u.into()
}

pub fn redirect(to: &str) -> Response {
    let mut r = (StatusCode::SEE_OTHER, [(header::LOCATION, to)]).into_response();
    no_store(r.headers_mut());
    r
}

// Requests ----------------------------------------------------------------------------------

/// What a browser request tells about its sender, for sessions and the audit log.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    pub ip: String,
    pub user_agent: String,
}

impl Caller {
    pub fn of(st: &Shared, peer: SocketAddr, headers: &HeaderMap) -> Self {
        let ip: IpAddr = st.boot.trusted_proxies.client_ip(peer, headers);
        Caller {
            ip: ip.to_string(),
            user_agent: headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect(),
        }
    }

    /// The address bucket for throttling: the address, or its /64 for IPv6.
    pub fn ip_bucket(&self) -> String {
        match self.ip.parse::<IpAddr>() {
            Ok(IpAddr::V6(v6)) => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
            _ => self.ip.clone(),
        }
    }

    /// "Firefox on macOS": enough to recognise a device in a list.
    pub fn device_label(&self) -> String {
        let ua = self.user_agent.as_str();
        let browser = [
            ("Edg/", "Edge"),
            ("OPR/", "Opera"),
            ("Firefox/", "Firefox"),
            ("Chrome/", "Chrome"),
            ("Safari/", "Safari"),
        ]
        .iter()
        .find(|(k, _)| ua.contains(k))
        .map(|(_, n)| *n);
        let os = [
            ("Android", "Android"),
            ("iPhone", "iOS"),
            ("iPad", "iPadOS"),
            ("Mac OS X", "macOS"),
            ("Windows", "Windows"),
            ("Linux", "Linux"),
        ]
        .iter()
        .find(|(k, _)| ua.contains(k))
        .map(|(_, n)| *n);
        match (browser, os) {
            (Some(b), Some(o)) => format!("{b} on {o}"),
            (Some(b), None) => b.to_owned(),
            (None, Some(o)) => o.to_owned(),
            (None, None) if ua.is_empty() => "an unknown device".to_owned(),
            (None, None) => ua.chars().take(60).collect(),
        }
    }
}

/// The realm of a browser request, by its Host.
pub fn realm_by_host<'a>(st: &'a Shared, headers: &HeaderMap) -> Option<&'a Realm> {
    headers.get(header::HOST).and_then(|h| h.to_str().ok()).and_then(|h| st.config.realm_for_host(h))
}

// Tokens ------------------------------------------------------------------------------------

/// What tokens are made for: an account through a client, in a session.
pub struct Subject<'a> {
    pub account: &'a Account,
    pub sid: &'a str,
    pub auth_time: DateTime<Utc>,
    pub amr: &'a [String],
    pub scope: &'a str,
}

fn has_scope(scope: &str, s: &str) -> bool {
    scope.split(' ').any(|x| x == s)
}

/// Signs an access token for the client's product (RFC 9068: typ at+jwt, aud the product).
pub fn access_token(
    st: &Shared,
    realm: &Realm,
    client: &Client,
    sub: &Subject<'_>,
    act: Option<&str>,
    audience: Option<&str>,
    lifetime: Duration,
) -> Result<String, OAuthError> {
    let now = Utc::now();
    let mut claims = json!({
        "iss": realm.issuer,
        "sub": sub.account.id,
        "aud": audience.or(client.audience.as_deref()).unwrap_or_default(),
        "client_id": client.id,
        "iat": now.timestamp(),
        "exp": (now + lifetime).timestamp(),
        "jti": tokens::opaque(),
        "sid": sub.sid,
        "auth_time": sub.auth_time.timestamp(),
        "amr": sub.amr,
        "scope": sub.scope,
        "pid_kind": "user",
    });
    if has_scope(sub.scope, "email") {
        claims["email"] = json!(sub.account.email);
    }
    // An empty claim is left out (OIDC Core 5.3.2), not sent as "".
    if has_scope(sub.scope, "profile") && !sub.account.display_name.is_empty() {
        claims["name"] = json!(sub.account.display_name);
    }
    if let Some(act) = act {
        claims["act"] = json!({ "sub": act });
    }
    sign(st, client.access_token_alg, "at+jwt", &claims)
}

pub fn sign(st: &Shared, alg: Alg, typ: &str, claims: &Value) -> Result<String, OAuthError> {
    let key = st.keys.signer(alg).ok_or_else(|| OAuthError::server(format!("no active {} key", alg.as_str())))?;
    key.sign(typ, claims).map_err(OAuthError::server)
}

/// The id_token: aud and azp are the client, with nonce, at_hash, auth_time and sid.
pub fn id_token(
    st: &Shared,
    realm: &Realm,
    client: &Client,
    sub: &Subject<'_>,
    nonce: Option<&str>,
    access_token: &str,
    roles: Option<&[String]>,
) -> Result<String, OAuthError> {
    let now = Utc::now();
    let digest = sha256(access_token.as_bytes());
    let mut claims = json!({
        "iss": realm.issuer,
        "sub": sub.account.id,
        "aud": client.id,
        "azp": client.id,
        "iat": now.timestamp(),
        "exp": (now + ID_TOKEN_LIFETIME).timestamp(),
        "auth_time": sub.auth_time.timestamp(),
        "sid": sub.sid,
        "amr": sub.amr,
        "at_hash": B64.encode(&digest[..16]),
    });
    if let Some(n) = nonce {
        claims["nonce"] = json!(n);
    }
    if has_scope(sub.scope, "email") {
        claims["email"] = json!(sub.account.email);
        claims["email_verified"] = json!(sub.account.is_verified());
    }
    if has_scope(sub.scope, "profile") {
        if !sub.account.display_name.is_empty() {
            claims["name"] = json!(sub.account.display_name);
        }
        if !sub.account.locale.is_empty() {
            claims["locale"] = json!(sub.account.locale);
        }
    }
    if let Some(r) = roles {
        claims["roles"] = json!(r);
    }
    sign(st, client.id_token_alg, "JWT", &claims)
}

/// The `roles` claim of a client that has one (such as Open WebUI): each value whose
/// permission the account holds somewhere, else the default.
pub async fn roles_of(
    st: &Shared,
    client: &Client,
    account: &str,
) -> Result<Option<Vec<String>>, crate::store::StoreError> {
    let Some(claim) = &client.roles_claim else { return Ok(None) };
    let mut conn = st.store.pool().acquire().await?;
    let subject = pacod_id_authz::Subject::user(crate::authz::bindings::subject_bindings(&mut conn, account).await?);
    let catalog = st.catalog.get();
    let mut roles: Vec<String> =
        claim.values.iter().filter(|(_, p)| catalog.allowed_somewhere(&subject, p)).map(|(v, _)| v.clone()).collect();
    if roles.is_empty() {
        roles.extend(claim.default.clone());
    }
    Ok(Some(roles))
}

/// A new refresh token and its grant.
pub async fn new_grant(
    st: &Shared,
    client: &Client,
    sub: &Subject<'_>,
    session: Option<(&str, DateTime<Utc>)>,
    act: Option<&str>,
    code_hash: Option<&[u8]>,
) -> Result<String, OAuthError> {
    let id = tokens::new_id("grt");
    let token = tokens::token(tokens::REFRESH_PREFIX, &id);
    let now = Utc::now();
    let (idle, expires) = match (session, client.refresh_idle_days) {
        // A browser's grant ends with its session.
        (Some((_, until)), _) => (None, until),
        // A native app's lasts from its last use, up to an absolute end.
        (None, idle) => (
            Some(now + Duration::days(i64::from(idle.unwrap_or(7)))),
            now + Duration::days(i64::from(client.refresh_absolute_days.unwrap_or(365))),
        ),
    };
    st.store
        .grant_create(&NewGrant {
            id: &id,
            sso_session_id: session.map(|(s, _)| s),
            sid: sub.sid,
            client_id: &client.id,
            account_id: &sub.account.id,
            scope: sub.scope,
            refresh_hash: &sha256(token.as_bytes()),
            act_sub: act,
            auth_time: sub.auth_time,
            amr: sub.amr,
            idle_expires_at: idle,
            expires_at: expires,
            code_hash,
        })
        .await?;
    Ok(token)
}

/// The token endpoint's answer.
pub fn token_response(body: Value) -> Response {
    let mut r = Json(body).into_response();
    no_store(r.headers_mut());
    r
}

// Signing in --------------------------------------------------------------------------------

/// The authorization request a flow carries (stored as JSON in auth_flows).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub redirect_uri: String,
    pub scope: String,
    pub state: Option<String>,
    pub nonce: Option<String>,
    pub code_challenge: String,
    #[serde(default)]
    pub prompt: Vec<String>,
    pub max_age: Option<i64>,
    pub login_hint: Option<String>,
    pub ui_locales: Option<String>,
    /// "Keep me signed in", once the password step chose it.
    #[serde(default)]
    pub remember_me: bool,
}

/// Issues a code for `request` and returns the redirect to the client.
pub async fn issue_code(
    st: &Shared,
    realm: &Realm,
    client: &Client,
    request: &Request,
    account: &Account,
    session: &str,
    auth_time: DateTime<Utc>,
) -> Result<String, ApiError> {
    // A client that maps roles from addresses (Open WebUI merges accounts by address) gets
    // only proven ones.
    if client.roles_claim.is_some() && !account.is_verified() {
        let mut params = vec![
            ("error", "access_denied"),
            ("error_description", "the account's email address is not confirmed"),
            ("iss", realm.issuer.as_str()),
        ];
        if let Some(s) = &request.state {
            params.push(("state", s));
        }
        return Ok(with_query(&request.redirect_uri, &params));
    }
    let code = tokens::opaque();
    st.store
        .code_create(
            &sha256(code.as_bytes()),
            &account.id,
            Some(session),
            &client.id,
            &request.redirect_uri,
            &request.code_challenge,
            request.nonce.as_deref(),
            &request.scope,
            auth_time,
            Utc::now() + CODE_LIFETIME,
        )
        .await?;
    let mut params = vec![("code", code.as_str()), ("iss", realm.issuer.as_str())];
    if let Some(s) = &request.state {
        params.push(("state", s));
    }
    Ok(with_query(&request.redirect_uri, &params))
}

/// The outcome of a finished sign-in: where the browser goes, and the cookies it gets.
pub struct SignedIn {
    pub to: String,
    pub cookies: Vec<HeaderValue>,
}

/// Finishes a sign-in: a new SSO session (never an upgraded one), the browser remembered, the
/// code issued, the flow gone.
/// Whether a flow may make a new account: its client's choice, or the realm's for a flow of the
/// realm's own pages.
pub fn self_signup(st: &Shared, realm: &Realm, flow: &Flow) -> Result<bool, ApiError> {
    if flow.client_id.is_empty() {
        return Ok(realm.self_signup);
    }
    let client = st.config.clients.get(&flow.client_id).ok_or_else(|| ApiError::bad_request("the client is gone"))?;
    Ok(client.self_signup.unwrap_or(realm.self_signup))
}

/// Signs `account` in at the end of `flow`: a new SSO session, then a code for the client, or
/// for a flow of the realm's own pages (no client) the page it started from.
#[allow(clippy::too_many_arguments)]
pub async fn sign_in(
    st: &Shared,
    realm: &Realm,
    flow: &Flow,
    account: &Account,
    remember_me: bool,
    amr: &[String],
    caller: &Caller,
    headers: &HeaderMap,
) -> Result<SignedIn, ApiError> {
    let client = match flow.client_id.as_str() {
        "" => None,
        id => Some(st.config.clients.get(id).ok_or_else(|| ApiError::bad_request("the client is gone"))?),
    };
    let request: Request = serde_json::from_value(flow.request.clone()).map_err(ApiError::internal)?;
    let sid = tokens::new_id("sid");
    let cookie = tokens::opaque();
    let lifetime = if remember_me {
        Duration::days(i64::from(realm.remember_me_days))
    } else {
        Duration::hours(i64::from(realm.session_hours))
    };
    st.store
        .session_create(&NewSession {
            id: &sid,
            account_id: &account.id,
            realm: &realm.name,
            cookie_hash: &sha256(cookie.as_bytes()),
            remember_me,
            amr,
            ip: &caller.ip,
            user_agent: &caller.user_agent,
            device_label: &caller.device_label(),
            expires_at: Utc::now() + lifetime,
        })
        .await?;
    let mut cookies =
        vec![set_cookie(realm, SID, &cookie, remember_me.then(|| lifetime.num_seconds())), clear_cookie(realm, FLOW)];
    let device =
        read_cookie(headers, &cookie_name(realm, DEVICE)).filter(|d| d.len() == 43).unwrap_or_else(tokens::opaque);
    let device_hash = st.boot.token_pepper.hash(&device);
    let known = st.store.device_known(&device_hash, &account.id).await?;
    let had_others = st.store.device_remember(&device_hash, &account.id).await?;
    cookies.push(set_cookie(realm, DEVICE, &device, Some(365 * 24 * 3600)));
    if !known && had_others {
        let params = BTreeMap::from([
            ("device".to_owned(), caller.device_label()),
            ("when".to_owned(), Utc::now().format("%Y-%m-%d %H:%M UTC").to_string()),
        ]);
        mail::enqueue(st, Template::NewSignIn, &realm.name, &account.email, params).await?;
    }
    st.store.account_signed_in(&account.id, &realm.name).await?;
    let to = match client {
        Some(c) => issue_code(st, realm, c, &request, account, &sid, Utc::now()).await?,
        None => format!("{}{}", realm.issuer, request.redirect_uri),
    };
    st.store.flow_delete(&flow.id).await?;
    st.store
        .audit(&AuditEntry {
            client_id: client.map(|c| c.id.clone()),
            realm: Some(realm.name.clone()),
            ip: caller.ip.clone(),
            user_agent: caller.user_agent.clone(),
            ..AuditEntry::new("login_succeeded")
                .by(&account.id, "user")
                .subject(&account.id)
                .detail(json!({ "sid": sid, "amr": amr }))
        })
        .await?;
    Ok(SignedIn { to, cookies })
}

/// Signs an account out of every session and grant (a password change or reset, a reused
/// refresh token, an account disabled). Returns the sessions ended.
pub async fn sign_out_everywhere(
    store: &Store,
    account: &str,
    reason: &str,
) -> Result<Vec<String>, crate::store::StoreError> {
    let mut tx = store.begin().await?;
    let revoked = Store::sessions_revoke_all(&mut tx, account, reason).await?;
    changes::sessions_revoked(&mut tx, &revoked, reason).await?;
    tx.commit().await?;
    Ok(revoked.into_iter().map(|(sid, _)| sid).collect())
}

/// This browser's live SSO session in `realm`, with its account while it is active.
pub async fn browser_session(
    st: &Shared,
    realm: &Realm,
    headers: &HeaderMap,
) -> Result<Option<(crate::store::Session, Account)>, crate::store::StoreError> {
    let Some(cookie) = read_cookie(headers, &cookie_name(realm, SID)) else { return Ok(None) };
    let Some(session) = st
        .store
        .session_by_cookie(&crate::crypto::sha256(cookie.as_bytes()))
        .await?
        .filter(|s| s.is_live() && s.realm == realm.name)
    else {
        return Ok(None);
    };
    let account = st.store.account(&session.account_id).await?.filter(Account::is_active);
    Ok(account.map(|a| (session, a)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce() {
        // RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(valid_challenge(challenge));
        assert!(pkce_matches(verifier, challenge));
        assert!(!pkce_matches("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXl", challenge));
        assert!(!pkce_matches("short", challenge));
        assert!(!valid_verifier(&"a".repeat(129)));
        assert!(!valid_verifier(&format!("{}!", "a".repeat(43))));
    }

    #[test]
    fn basic_credentials_are_form_decoded() {
        assert_eq!(form_decode("a%3Ab+c%25"), "a:b c%");
        assert_eq!(form_decode("plain"), "plain");
    }

    #[test]
    fn device_labels() {
        let c = |ua: &str| Caller { ip: String::new(), user_agent: ua.into() }.device_label();
        assert_eq!(
            c("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit Chrome/129.0 Safari/537"),
            "Chrome on macOS"
        );
        assert_eq!(c("Mozilla/5.0 (X11; Linux x86_64; rv:131.0) Gecko/20100101 Firefox/131.0"), "Firefox on Linux");
        assert_eq!(c(""), "an unknown device");
    }

    #[test]
    fn redirects_keep_existing_query_parameters() {
        assert_eq!(with_query("https://app.example/cb?x=1", &[("code", "a b")]), "https://app.example/cb?x=1&code=a+b");
    }
}
