//! Signing in through pacod-id without a browser (`--auth oidc`), for gw-smoke and gw-load: the
//! code flow of the development client `gw-smoke` (public, PKCE), walked the way the hosted
//! pages walk it. /oidc/authorize starts a sign-in flow, the flow's password step answers with
//! the redirect that carries the code, and /oidc/token redeems it. The access token is refreshed
//! a minute before it expires, so that a long run keeps working.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use serde_json::{Value, json};

/// How the tools sign in to the management API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Auth {
    /// The core's own login (PGW_AUTH=local).
    Local,
    /// pacod-id, as the development client `gw-smoke` (PGW_AUTH=oidc).
    Oidc,
}

impl Auth {
    /// The client to sign in with, for `--auth oidc`.
    pub fn client(self, issuer: &str) -> Option<IdClient> {
        (self == Auth::Oidc).then(|| IdClient {
            issuer: issuer.trim_end_matches('/').to_owned(),
            client_id: "gw-smoke".into(),
            redirect_uri: "http://127.0.0.1:13999/callback".into(),
        })
    }
}

/// Where and as which client to sign in.
#[derive(Debug, Clone)]
pub struct IdClient {
    /// The realm's issuer, as a browser reaches it: `http://127.0.0.1:13070` in development.
    pub issuer: String,
    pub client_id: String,
    /// One of the client's registered redirect URIs; nothing needs to listen there.
    pub redirect_uri: String,
}

/// A signed-in session: its tokens, refreshed when due.
pub struct Session {
    http: reqwest::Client,
    client: IdClient,
    access: String,
    refresh: Option<String>,
    until: Instant,
}

fn query(url: &str, base: &str, name: &str) -> Option<String> {
    let url = Url::parse(url).or_else(|_| Url::parse(base)?.join(url)).ok()?;
    url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
}

impl Session {
    pub async fn sign_in(client: IdClient, email: &str, password: &str) -> anyhow::Result<Session> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        let issuer = client.issuer.trim_end_matches('/').to_owned();
        let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()));
        let state = uuid::Uuid::new_v4().simple().to_string();
        let mut authorize = Url::parse(&format!("{issuer}/oidc/authorize"))?;
        authorize
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &client.client_id)
            .append_pair("redirect_uri", &client.redirect_uri)
            .append_pair("scope", "openid email profile offline_access")
            .append_pair("state", &state)
            .append_pair("nonce", &uuid::Uuid::new_v4().simple().to_string())
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        let r = http.get(authorize).send().await.with_context(|| format!("pacod-id at {issuer} does not answer"))?;
        let cookies: Vec<String> = r
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok()?.split(';').next().map(str::to_owned))
            .collect();
        let to = r.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
        let flow = query(&to, &issuer, "flow")
            .with_context(|| format!("pacod-id did not start a sign-in ({}, to {to:?})", r.status()))?;
        let r = http
            .post(format!("{issuer}/ui-api/v1/flows/{flow}/password"))
            .header("cookie", cookies.join("; "))
            // The pages' API answers its own origin only.
            .header("origin", &issuer)
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await?;
        let status = r.status();
        let step: Value = r.json().await.unwrap_or(Value::Null);
        if step["next"] != "redirect" {
            bail!("signing in as {email} at pacod-id failed ({status}): {}", step["error"]["message"]);
        }
        let to = step["to"].as_str().unwrap_or_default();
        if query(to, &issuer, "state").as_deref() != Some(state.as_str()) {
            bail!("pacod-id answered the sign-in with another state");
        }
        let code = query(to, &issuer, "code").context("pacod-id's redirect carries no code")?;
        let mut session = Session { http, client, access: String::new(), refresh: None, until: Instant::now() };
        session
            .token_request(&[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", &session.client.redirect_uri.clone()),
                ("code_verifier", &verifier),
            ])
            .await?;
        Ok(session)
    }

    async fn token_request(&mut self, form: &[(&str, &str)]) -> anyhow::Result<()> {
        let mut form = form.to_vec();
        form.push(("client_id", &self.client.client_id));
        let r = self
            .http
            .post(format!("{}/oidc/token", self.client.issuer.trim_end_matches('/')))
            .form(&form)
            .send()
            .await?;
        let status = r.status();
        let t: Value = r.json().await.unwrap_or(Value::Null);
        let Some(access) = t["access_token"].as_str() else {
            bail!("pacod-id's token endpoint answered {status}: {}", t["error"]);
        };
        self.access = access.to_owned();
        self.refresh = t["refresh_token"].as_str().map(str::to_owned).or(self.refresh.take());
        self.until = Instant::now() + Duration::from_secs(t["expires_in"].as_u64().unwrap_or(300));
        Ok(())
    }

    /// A valid access token: the current one, or a refreshed one a minute before it expires.
    pub async fn access_token(&mut self) -> anyhow::Result<String> {
        if self.until.saturating_duration_since(Instant::now()) < Duration::from_secs(60) {
            let refresh = self.refresh.clone().context("the session has no refresh token")?;
            self.token_request(&[("grant_type", "refresh_token"), ("refresh_token", &refresh)]).await?;
        }
        Ok(self.access.clone())
    }
}

impl Session {
    /// The session id (`sid`) the access token carries.
    pub fn sid(&self) -> Option<String> {
        let payload = self.access.split('.').nth(1)?;
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
        claims["sid"].as_str().map(str::to_owned)
    }
}

/// How long the core keeps taking an access token after pacod-id ended its session (identity
/// concept: 2 seconds or less). Each sample signs in, checks that the core takes the token,
/// ends the session as a sign-out does (`POST /internal/v1/sessions/{sid}/revoke`, here with a
/// service client's credentials), and asks the core every 20 ms until it refuses the token.
pub async fn measure_revocation(
    client: IdClient,
    email: &str,
    password: &str,
    admin_url: &str,
    internal_url: &str,
    service: (&str, &str),
    samples: usize,
) -> anyhow::Result<Vec<Duration>> {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        let mut session = Session::sign_in(client.clone(), email, password).await?;
        let token = session.access_token().await?;
        let sid = session.sid().context("the access token carries no sid")?;
        let me = |token: String| {
            let http = http.clone();
            async move {
                http.get(format!("{admin_url}/api/v1/session")).bearer_auth(token).send().await.map(|r| r.status())
            }
        };
        // The core may need a moment for a session it has not seen yet; then it takes it.
        let mut taken = false;
        for _ in 0..100 {
            if me(token.clone()).await?.is_success() {
                taken = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if !taken {
            bail!("the core does not take a fresh access token");
        }
        let started = Instant::now();
        let r = http
            .post(format!("{}/internal/v1/sessions/{sid}/revoke", internal_url.trim_end_matches('/')))
            .basic_auth(service.0, Some(service.1))
            .send()
            .await?;
        if !r.status().is_success() {
            bail!("pacod-id did not end the session: {}", r.status());
        }
        loop {
            if me(token.clone()).await? == reqwest::StatusCode::UNAUTHORIZED {
                out.push(started.elapsed());
                break;
            }
            if started.elapsed() > Duration::from_secs(10) {
                bail!("the core still takes the token 10 s after its session ended");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    Ok(out)
}
