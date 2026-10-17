//! Stage 5: userinfo, revocation, introspection, logout and back-channel logout.

mod common;

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Browser, CLIENT_SECRET, TestId, account, authorize_url, param, pkce};
use serde_json::{Value, json};

const CORE_SECRET: &str = "a-secret-for-the-gateway-core-0123456789";
const BFF_SECRET: &str = "a-secret-for-the-second-bff-0123456789";
const BFF_REDIRECT: &str = "http://127.0.0.1:13001/auth/callback";

async fn core_may_call(id: &TestId) {
    id.store()
        .client_set_secret("gw-core", "gateway", &id.running.state.boot.token_pepper.hash(CORE_SECRET))
        .await
        .unwrap();
}

/// A form post to the internal listener with HTTP Basic.
async fn internal(id: &TestId, path: &str, client: &str, secret: &str, form: &[(&str, &str)]) -> (u16, Value) {
    let r = id
        .http
        .post(format!("{}{path}", id.internal))
        .basic_auth(client, Some(secret))
        .form(form)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
}

async fn introspect(id: &TestId, token: &str) -> Value {
    let (status, v) = internal(id, "/internal/v1/introspect", "gw-core", CORE_SECRET, &[("token", token)]).await;
    assert_eq!(status, 200, "{v}");
    v
}

async fn userinfo(id: &TestId, token: &str) -> (u16, String, Value) {
    let r = id.http.get(format!("{}/oidc/userinfo", id.public)).bearer_auth(token).send().await.unwrap();
    let challenge = r.headers().get("www-authenticate").map(|h| h.to_str().unwrap().to_owned()).unwrap_or_default();
    (r.status().as_u16(), challenge, r.json().await.unwrap_or(Value::Null))
}

async fn sid_of(id: &TestId, tokens: &Value) -> String {
    let (_, claims) = id.verify(tokens["id_token"].as_str().unwrap()).await;
    claims["sid"].as_str().unwrap().to_owned()
}

async fn live(id: &TestId, sid: &str) -> bool {
    id.store().session(sid).await.unwrap().is_some_and(|s| s.is_live())
}

#[tokio::test]
async fn userinfo_revocation_and_introspection() {
    let pepper = "a-gateway-pepper-for-the-tests-0123456789";
    let db = common::fresh_database().await;
    let id = TestId::start_with(
        &db,
        |_| String::new(),
        |boot| {
            boot.legacy_peppers.insert("gw".into(), pacod_id::crypto::Pepper::new(pepper.as_bytes().to_vec()).unwrap());
        },
    )
    .await;
    core_may_call(&id).await;
    let ada = account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (_, tokens) = id.redeem(&code, &verifier).await;
    let access = tokens["access_token"].as_str().unwrap();

    let (status, _, info) = userinfo(&id, access).await;
    assert_eq!(status, 200, "{info}");
    assert_eq!(
        (info["sub"].as_str(), info["email"].as_str(), info["email_verified"].as_bool(), info["name"].as_str()),
        (Some(ada.id.as_str()), Some("ada@example.com"), Some(true), Some("Ada Lovelace"))
    );
    let (status, challenge, _) = userinfo(&id, tokens["id_token"].as_str().unwrap()).await;
    assert_eq!((status, challenge.contains("invalid_token")), (401, true), "an id_token is no access token");
    let r = id.http.get(format!("{}/oidc/userinfo", id.public)).send().await.unwrap();
    assert_eq!(r.status(), 401);

    // Introspection: products' services only.
    let v = introspect(&id, access).await;
    assert_eq!(
        (v["active"].as_bool(), v["sub"].as_str(), v["pid_kind"].as_str()),
        (Some(true), Some(ada.id.as_str()), Some("user"))
    );
    let (_, v) = internal(&id, "/internal/v1/introspect", "console", CLIENT_SECRET, &[("token", access)]).await;
    assert_eq!(v["error"], "unauthorized_client", "{v}");
    let (status, _) =
        internal(&id, "/internal/v1/introspect", "gw-core", "a-wrong-secret-0123456789012345", &[("token", access)])
            .await;
    assert_eq!(status, 401);
    // A client-credentials token works as well as the secret.
    let (_, cc) = internal(&id, "/oidc/token", "gw-core", CORE_SECRET, &[("grant_type", "client_credentials")]).await;
    let r = id
        .http
        .post(format!("{}/internal/v1/introspect", id.internal))
        .bearer_auth(cc["access_token"].as_str().unwrap())
        .form(&[("token", access)])
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["active"], true);
    // Not on the public listener.
    let r =
        id.http.post(format!("{}/internal/v1/introspect", id.public)).form(&[("token", access)]).send().await.unwrap();
    assert_eq!(r.status(), 404);

    // Service tokens: pacod-id's own, and one imported from the gateway with its pepper.
    id.store().service_account_create("sva_1", "ci", None, "test").await.unwrap();
    let own = pacod_id::crypto::tokens::token("pidt", "stk_1");
    let boot = &id.running.state.boot;
    id.store()
        .service_token_create("stk_1", "sva_1", &boot.token_pepper.hash(&own), None, "pidt_1…", None)
        .await
        .unwrap();
    let imported = "pgwa_9f8e7d_an-old-gateway-admin-token-secret";
    let gw_pepper = pacod_id::crypto::Pepper::new(pepper.as_bytes().to_vec()).unwrap();
    id.store()
        .service_token_create("stk_2", "sva_1", &gw_pepper.hash(imported), Some("gw"), "pgwa_9f…", None)
        .await
        .unwrap();
    for t in [own.as_str(), imported] {
        let v = introspect(&id, t).await;
        assert_eq!(
            (v["active"].as_bool(), v["sub"].as_str(), v["pid_kind"].as_str()),
            (Some(true), Some("sva_1"), Some("service")),
            "{t}"
        );
    }
    for t in ["pgwa_9f8e7d_a-guess", "pidt_1_a-guess", "nonsense", tokens["id_token"].as_str().unwrap()] {
        assert_eq!(introspect(&id, t).await, json!({ "active": false }), "{t}");
    }

    // Revocation: another client's token stays, the owner's goes. Always 200.
    let refresh = tokens["refresh_token"].as_str().unwrap();
    let (status, _) = internal(&id, "/oidc/revoke", "gw-core", CORE_SECRET, &[("token", refresh)]).await;
    assert_eq!(status, 200);
    let (status, rotated) = id.token(&[("grant_type", "refresh_token"), ("refresh_token", refresh)]).await;
    assert_eq!(status, 200, "{rotated}");
    let refresh = rotated["refresh_token"].as_str().unwrap();
    let r = id
        .http
        .post(format!("{}/oidc/revoke", id.public))
        .basic_auth("console", Some(CLIENT_SECRET))
        .form(&[("token", refresh), ("token_type_hint", "refresh_token")])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let (status, _) = id.token(&[("grant_type", "refresh_token"), ("refresh_token", refresh)]).await;
    assert_eq!(status, 400, "a revoked token is refused");
    let r = id
        .http
        .post(format!("{}/oidc/revoke", id.public))
        .basic_auth("console", Some(CLIENT_SECRET))
        .form(&[("token", "pidr_x_y")])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "an unknown token answers the same");

    // A signed-out session's access tokens stop at once.
    let sid = sid_of(&id, &tokens).await;
    let r = id
        .http
        .post(format!("{}/internal/v1/sessions/{sid}/revoke", id.internal))
        .basic_auth("gw-core", Some(CORE_SECRET))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert_eq!(introspect(&id, access).await, json!({ "active": false }));
    assert_eq!(userinfo(&id, access).await.0, 401);
    id.running.stop().await;
}

#[tokio::test]
async fn rp_initiated_logout() {
    let id = TestId::start().await;
    account(&id, "ada@example.com").await;
    let issuer = id.public.clone();
    let logout = |q: &[(&str, &str)]| {
        let mut u = url::Url::parse(&format!("{issuer}/oidc/logout")).unwrap();
        u.query_pairs_mut().extend_pairs(q);
        u.to_string()
    };
    let home = "http://127.0.0.1:13000/";

    // Never back to an address the client did not register; never with someone else's token.
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (_, tokens) = id.redeem(&code, &verifier).await;
    let hint = tokens["id_token"].as_str().unwrap();
    let sid = sid_of(&id, &tokens).await;
    for q in [
        vec![("client_id", "console"), ("post_logout_redirect_uri", "https://evil.example/")],
        vec![("post_logout_redirect_uri", home)],
        vec![("id_token_hint", hint), ("client_id", "bazaar-android")],
        vec![("id_token_hint", "eyJhbGciOiJub25lIn0.e30."), ("post_logout_redirect_uri", home)],
    ] {
        let a = b.get(&logout(&q)).await;
        assert_eq!((a.status, a.location.as_deref()), (400, None), "{q:?}");
    }
    assert!(live(&id, &sid).await);

    // With the client's id_token: ended at once, back to the client with its state.
    let a = b.get(&logout(&[("id_token_hint", hint), ("post_logout_redirect_uri", home), ("state", "s-1")])).await;
    assert_eq!(a.location.as_deref(), Some("http://127.0.0.1:13000/?state=s-1"), "{a:?}");
    assert!(!b.cookies.contains_key("pid_sid"), "the session cookie is cleared");
    assert!(!live(&id, &sid).await);
    let (status, _) = id
        .token(&[("grant_type", "refresh_token"), ("refresh_token", tokens["refresh_token"].as_str().unwrap())])
        .await;
    assert_eq!(status, 400, "the session's grants ended with it");

    // Without proof: the person confirms on the UI page, which ends the session.
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (_, tokens) = id.redeem(&code, &verifier).await;
    let sid = sid_of(&id, &tokens).await;
    let a = b.get(&logout(&[("client_id", "console"), ("post_logout_redirect_uri", home), ("state", "s-2")])).await;
    let to = a.location.unwrap();
    assert!(to.starts_with(&format!("{issuer}/logout?")), "{to}");
    assert_eq!(param(&to, "post_logout_redirect_uri").as_deref(), Some(home));
    assert!(live(&id, &sid).await, "nothing ends before the person confirms");
    let ui = format!("{issuer}/ui-api/v1/logout");
    let a = b.post(&ui, json!({ "client_id": "console", "post_logout_redirect_uri": "https://evil.example/" })).await;
    assert_eq!(a.status, 400);
    b.origin = "https://evil.example".into();
    assert_eq!(b.post(&ui, json!({})).await.status, 403, "only the UI's own pages");
    b.origin = issuer.clone();
    let a = b.post(&ui, json!({ "client_id": "console", "post_logout_redirect_uri": home, "state": "s-2" })).await;
    assert_eq!(
        (a.json["next"].as_str(), a.json["to"].as_str()),
        (Some("redirect"), Some("http://127.0.0.1:13000/?state=s-2"))
    );
    assert!(!live(&id, &sid).await);

    // From another browser, the hint names the session to end.
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (_, tokens) = id.redeem(&code, &verifier).await;
    let sid = sid_of(&id, &tokens).await;
    let mut other = Browser::new(&id);
    let a = other.get(&logout(&[("id_token_hint", tokens["id_token"].as_str().unwrap())])).await;
    assert_eq!(a.status, 200);
    assert!(a.text.contains("signed out"));
    assert!(!live(&id, &sid).await);
    // Nothing to end: still a friendly page.
    assert_eq!(Browser::new(&id).get(&logout(&[])).await.status, 200);
    let logged = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_log WHERE action = 'logout'")
        .fetch_one(id.store().pool())
        .await
        .unwrap();
    assert_eq!(logged, 3);
    id.running.stop().await;
}

/// A client's back-channel endpoint: records logout tokens, answers with `status`.
struct Receiver {
    url: String,
    tokens: Arc<Mutex<Vec<String>>>,
    status: Arc<AtomicU16>,
}

async fn receiver() -> Receiver {
    use axum::extract::{Form, State};
    let tokens = Arc::new(Mutex::new(Vec::new()));
    let status = Arc::new(AtomicU16::new(200));
    type Shared = (Arc<Mutex<Vec<String>>>, Arc<AtomicU16>);
    let app = axum::Router::new()
        .route(
            "/backchannel",
            axum::routing::post(
                |State((tokens, status)): State<Shared>,
                 headers: axum::http::HeaderMap,
                 Form(f): Form<std::collections::HashMap<String, String>>| async move {
                    let origin = headers.get("origin").and_then(|o| o.to_str().ok()).unwrap_or("-").to_owned();
                    tokens
                        .lock()
                        .unwrap()
                        .push(format!("{origin} {}", f.get("logout_token").cloned().unwrap_or_default()));
                    axum::http::StatusCode::from_u16(status.load(Ordering::SeqCst)).unwrap()
                },
            ),
        )
        .with_state((tokens.clone(), status.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/backchannel", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Receiver { url, tokens, status }
}

async fn eventually(what: &str, mut check: impl AsyncFnMut() -> bool) {
    for _ in 0..80 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("not within 8 seconds: {what}");
}

#[tokio::test]
async fn back_channel_logout_and_internal_revocation() {
    let rp = receiver().await;
    let db = common::fresh_database().await;
    let rp_url = rp.url.clone();
    let id = TestId::start_with(
        &db,
        move |_| {
            format!(
                r#"
[[client]]
id = "bff"
realm = "gateway"
kind = "confidential"
secret_env = "BFF_SECRET"
redirect_uris = ["{BFF_REDIRECT}"]
backchannel_logout_uri = "{rp_url}"
audience = "pacod-gw"
"#
            )
        },
        |boot| {
            boot.client_secrets.insert("bff".into(), BFF_SECRET.into());
        },
    )
    .await;
    core_may_call(&id).await;
    let ada = account(&id, "ada@example.com").await;

    // Signed in to the console, then to the BFF by single sign-on: one session, two clients.
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (_, console_tokens) = id.redeem(&code, &verifier).await;
    let (verifier, challenge) = pkce();
    let a = b.get(&authorize_url(&id, &challenge, &[("client_id", "bff"), ("redirect_uri", BFF_REDIRECT)])).await;
    let code = param(&a.location.unwrap(), "code").unwrap();
    let r = id
        .http
        .post(format!("{}/oidc/token", id.public))
        .basic_auth("bff", Some(BFF_SECRET))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", BFF_REDIRECT),
            ("code_verifier", &verifier),
        ])
        .send()
        .await
        .unwrap();
    let bff_tokens: Value = r.json().await.unwrap();
    let sid = sid_of(&id, &bff_tokens).await;
    assert_eq!(sid, sid_of(&id, &console_tokens).await);

    // Another session of Ada's, in which the BFF had no grant: not the BFF's to end.
    let mut c = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut c, "ada@example.com").await;
    let (_, other_tokens) = id.redeem(&code, &verifier).await;
    let other_sid = sid_of(&id, &other_tokens).await;
    let revoke = |sid: String, client: &'static str, secret: &'static str| {
        let req =
            id.http.post(format!("{}/internal/v1/sessions/{sid}/revoke", id.internal)).basic_auth(client, Some(secret));
        async move { req.send().await.unwrap().status().as_u16() }
    };
    assert_eq!(revoke(other_sid.clone(), "bff", BFF_SECRET).await, 403);
    assert_eq!(revoke(other_sid.clone(), "bff", "not-the-secret-of-the-bff-012345678").await, 401);
    let r = id.http.post(format!("{}/internal/v1/sessions/{other_sid}/revoke", id.internal)).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert!(live(&id, &other_sid).await);

    // The BFF ends the shared session; its endpoint fails once, then takes the retry.
    rp.status.store(500, Ordering::SeqCst);
    assert_eq!(revoke(sid.clone(), "bff", BFF_SECRET).await, 204);
    assert!(!live(&id, &sid).await);
    let (status, _) = id
        .token(&[("grant_type", "refresh_token"), ("refresh_token", console_tokens["refresh_token"].as_str().unwrap())])
        .await;
    assert_eq!(status, 400, "the console's grant ended with the session");
    let pool = id.store().pool().clone();
    let attempts = || {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (i32, String, bool)>(
                "SELECT attempts, last_error, done_at IS NOT NULL FROM backchannel_outbox",
            )
            .fetch_all(&pool)
            .await
            .unwrap()
        }
    };
    eventually("a failed attempt", async || attempts().await.first().is_some_and(|r| r.0 == 1)).await;
    let rows = attempts().await;
    assert_eq!(rows.len(), 1, "only the client with a back-channel endpoint: {rows:?}");
    assert_eq!(rows[0].1, "HTTP 500");
    rp.status.store(200, Ordering::SeqCst);
    sqlx::query("UPDATE backchannel_outbox SET next_attempt_at = now()").execute(&pool).await.unwrap();
    eventually("delivery", async || attempts().await[0].2).await;

    let sent = rp.tokens.lock().unwrap().clone();
    assert!(sent.iter().all(|s| s.starts_with(&format!("{} ", id.public))), "the issuer's origin goes along: {sent:?}");
    let sent: Vec<String> = sent.iter().map(|s| s.split_once(' ').unwrap().1.to_owned()).collect();
    assert_eq!(sent.len(), 2);
    assert_ne!(sent[0], sent[1], "each attempt is signed afresh");
    let (typ, claims) = id.verify(&sent[1]).await;
    assert_eq!(typ, "logout+jwt");
    assert_eq!(
        (claims["iss"].as_str(), claims["aud"].as_str(), claims["sid"].as_str(), claims["sub"].as_str()),
        (Some(id.public.as_str()), Some("bff"), Some(sid.as_str()), Some(ada.id.as_str()))
    );
    assert!(claims["events"].get("http://schemas.openid.net/event/backchannel-logout").is_some());
    assert!(claims.get("nonce").is_none() && claims["jti"].is_string());
    assert_eq!(claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(), 120);

    // A product's service may end any session; no client is told of one it had no grant in.
    assert_eq!(revoke(other_sid.clone(), "gw-core", CORE_SECRET).await, 204);
    assert!(!live(&id, &other_sid).await);
    assert_eq!(attempts().await.len(), 1);
    id.running.stop().await;
}

const WEBUI_SECRET: &str = "a-secret-for-open-webui-in-tests-0123456789";
const WEBUI_REDIRECT: &str = "http://127.0.0.1:13002/oauth/oidc/callback";

/// Signs in to Open WebUI in `b` (signed in already): where the browser went, and the tokens.
async fn webui(id: &TestId, b: &mut Browser) -> (String, Value) {
    let (verifier, challenge) = pkce();
    let url = authorize_url(id, &challenge, &[("client_id", "webui"), ("redirect_uri", WEBUI_REDIRECT)]);
    let to = b.get(&url).await.location.unwrap();
    let Some(code) = param(&to, "code") else { return (to, Value::Null) };
    let r = id
        .http
        .post(format!("{}/oidc/token", id.public))
        .basic_auth("webui", Some(WEBUI_SECRET))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", WEBUI_REDIRECT),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .unwrap();
    (to, r.json::<Value>().await.unwrap())
}

#[tokio::test]
async fn open_webui_gets_roles_and_only_proven_addresses() {
    let db = common::fresh_database().await;
    let id = TestId::start_with(
        &db,
        |_| {
            format!(
                r#"
[[client]]
id = "webui"
realm = "gateway"
kind = "confidential"
secret_env = "WEBUI_SECRET"
redirect_uris = ["{WEBUI_REDIRECT}"]
audience = "shop-webui"
roles_claim = {{ values = {{ admin = "shop.webui.admin" }}, default = "user" }}
"#
            )
        },
        |boot| {
            boot.client_secrets.insert("webui".into(), WEBUI_SECRET.into());
            // No mail: unconfirmed accounts sign in, so the client's own rule shows.
            boot.mail = None;
        },
    )
    .await;
    let st = &id.running.state;
    let text = std::fs::read_to_string(format!("{}/tests/fixtures/shop.yaml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let source = serde_json::to_value(pacod_id_authz::Manifest::from_yaml(&text).unwrap().source()).unwrap();
    let mut tx = st.store.begin().await.unwrap();
    pacod_id::store::authz::catalog_put(&mut tx, "shop", &source).await.unwrap();
    tx.commit().await.unwrap();
    st.catalog.reload(&st.store).await.unwrap();
    let ada = account(&id, "ada@example.com").await;

    let mut b = Browser::new(&id);
    id.sign_in(&mut b, "ada@example.com").await;
    let (_, tokens) = webui(&id, &mut b).await;
    let (_, claims) = id.verify(tokens["id_token"].as_str().unwrap()).await;
    assert_eq!(claims["roles"], json!(["user"]));
    // A superuser holds webui.admin: an Open WebUI admin from the next sign-in.
    let mut tx = st.store.begin().await.unwrap();
    pacod_id::store::authz::lock_head(&mut tx).await.unwrap();
    let add = [pacod_id::authz::bindings::NewBinding {
        principal: ada.id.clone(),
        role: "shop.superuser".into(),
        scope: pacod_id_authz::ScopeRef::platform(),
        source: "manual",
        created_by: "test".into(),
    }];
    pacod_id::authz::bindings::apply(&mut tx, &st.catalog.get(), pacod_id::authz::bindings::Protect::Skip, &[], &add)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (_, tokens) = webui(&id, &mut b).await;
    let (_, claims) = id.verify(tokens["id_token"].as_str().unwrap()).await;
    assert_eq!(claims["roles"], json!(["admin"]));
    let (status, _, info) = userinfo(&id, tokens["access_token"].as_str().unwrap()).await;
    assert_eq!((status, info["roles"].clone()), (200, json!(["admin"])));
    // Other clients get no roles claim.
    let (code, verifier) = id.sign_in(&mut Browser::new(&id), "ada@example.com").await;
    let (_, console) = id.redeem(&code, &verifier).await;
    let (_, claims) = id.verify(console["id_token"].as_str().unwrap()).await;
    assert!(claims.get("roles").is_none());

    // An unconfirmed address gets no code for Open WebUI, which merges accounts by address.
    common::account_with(&id, "bob@example.com", common::PASSWORD, false).await;
    let mut bb = Browser::new(&id);
    id.sign_in(&mut bb, "bob@example.com").await;
    let (to, tokens) = webui(&id, &mut bb).await;
    assert!(tokens.is_null());
    assert_eq!(
        (param(&to, "error").as_deref(), param(&to, "state").as_deref()),
        (Some("access_denied"), Some("st-1")),
        "{to}"
    );
    id.running.stop().await;
}
