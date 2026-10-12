//! pacod-id in process, for the core's tests in OIDC mode (identity concept, phase 2): the
//! gateway's realm and clients, a database from `fresh_database()` (so the sweep covers it), mail
//! in memory. Only the OIDC tests include this (`#[path = "common/id.rs"] mod id;`), so the other
//! test binaries do not link pacod-id.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::TcpListener;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use pacod_id::{Bootstrap, Config, Listeners, Running};
use serde_json::{Value, json};

pub const CONSOLE_SECRET: &str = "a-console-secret-for-the-gateway-tests-0123";
pub const CORE_SECRET: &str = "a-core-secret-for-the-gateway-tests-012345";
pub const CONSOLE_REDIRECT: &str = "http://127.0.0.1:13000/auth/callback";
pub const PASSWORD: &str = "a long and unusual passphrase";

/// The gateway's realm and clients, as the development stack configures them (stage 13).
pub fn config(port: u16) -> String {
    format!(
        r#"
[[realm]]
name = "gateway"
issuer = "http://127.0.0.1:{port}"
brand = "gateway"
accounts_managed_by = ["gw.platform_admin"]

[[client]]
id = "pacod-gw-console"
realm = "gateway"
kind = "confidential"
secret_env = "PGW_UI_CLIENT_SECRET"
redirect_uris = ["{CONSOLE_REDIRECT}"]
post_logout_redirect_uris = ["http://127.0.0.1:13000/"]
audience = "pacod-gw"

[[client]]
id = "pacod-gw-core"
realm = "gateway"
kind = "service"
product = "gw"
secret_env = "PGW_ID_CLIENT_SECRET"
audience = "pacod-gw"
"#
    )
}

pub struct Id {
    pub running: Running,
    pub sent: std::sync::Arc<std::sync::Mutex<Vec<pacod_id::mail::Sent>>>,
    /// The issuer, which is also the public listener.
    pub public: String,
    pub internal: String,
    pub http: reqwest::Client,
}

impl Id {
    pub async fn start() -> Self {
        Self::start_on(&crate::common::fresh_database().await).await
    }

    pub async fn start_on(db: &str) -> Self {
        let bind = || {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.set_nonblocking(true).unwrap();
            l
        };
        let listeners = Listeners { public: bind(), internal: bind(), metrics: bind() };
        let port = listeners.public.local_addr().unwrap().port();
        let config = Config::from_toml(&config(port)).expect("the gateway's pacod-id configuration");
        let mut boot = Bootstrap::for_tests(db);
        boot.client_secrets.insert("pacod-gw-console".into(), CONSOLE_SECRET.into());
        boot.client_secrets.insert("pacod-gw-core".into(), CORE_SECRET.into());
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        boot.mail = Some(pacod_id::mail::Transport::Memory(sent.clone()));
        let running = pacod_id::start(boot, config, listeners).await.expect("pacod-id starts");
        Id {
            sent,
            public: format!("http://{}", running.public),
            internal: format!("http://{}", running.internal),
            running,
            http: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap(),
        }
    }

    pub fn store(&self) -> &pacod_id::store::Store {
        &self.running.state.store
    }

    /// The core's view of pacod-id: the issuer for tokens, the internal listener for calls.
    pub fn client_config(&self) -> pacod_id_client::IdConfig {
        pacod_id_client::IdConfig {
            issuers: vec![self.public.clone()],
            internal_url: self.internal.clone(),
            client_id: "pacod-gw-core".into(),
            client_secret: CORE_SECRET.into(),
            product: "gw".into(),
            audience: "pacod-gw".into(),
        }
    }

    /// A person with a password (`PASSWORD`), their address confirmed. Returns the account id.
    pub async fn account(&self, email: &str) -> String {
        let hash = pacod_id::crypto::password::hash(PASSWORD, Default::default()).unwrap();
        self.store()
            .account_create(&pacod_id::store::NewAccount {
                id: pacod_id::crypto::tokens::new_id("usr"),
                email: email.into(),
                display_name: email.split('@').next().unwrap_or_default().into(),
                password_hash: Some(hash),
                verified_by: Some("admin".to_owned()),
                signup_realm: "gateway".into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
    }

    /// Signs `email` in through the console's client as the hosted pages would, and redeems the
    /// code: the token answer (access, refresh and id tokens).
    pub async fn sign_in(&self, email: &str) -> Value {
        let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let challenge = B64.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref());
        let mut authorize = url::Url::parse(&format!("{}/oidc/authorize", self.public)).unwrap();
        authorize
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", "pacod-gw-console")
            .append_pair("redirect_uri", CONSOLE_REDIRECT)
            .append_pair("scope", "openid email profile")
            .append_pair("state", "st")
            .append_pair("nonce", "n")
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        let r = self.http.get(authorize).send().await.unwrap();
        let cookies: Vec<String> = r
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok()?.split(';').next().map(str::to_owned))
            .collect();
        let to = r.headers()["location"].to_str().unwrap().to_owned();
        let flow = param(&to, "flow").unwrap_or_else(|| panic!("no flow: {to}"));
        let r = self
            .http
            .post(format!("{}/ui-api/v1/flows/{flow}/password", self.public))
            .header("cookie", cookies.join("; "))
            .header("origin", &self.public)
            .json(&json!({ "email": email, "password": PASSWORD }))
            .send()
            .await
            .unwrap();
        let step: Value = r.json().await.unwrap();
        let code = param(step["to"].as_str().unwrap_or_else(|| panic!("{step}")), "code").unwrap();
        let form = BTreeMap::from([
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", CONSOLE_REDIRECT),
            ("code_verifier", verifier.as_str()),
        ]);
        self.http
            .post(format!("{}/oidc/token", self.internal))
            .basic_auth("pacod-gw-console", Some(CONSOLE_SECRET))
            .form(&form)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

pub fn param(url: &str, name: &str) -> Option<String> {
    url::Url::parse(url).ok()?.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
}
