//! Stage 5: signing in and the token endpoint.

mod common;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use common::{Browser, CONSOLE_REDIRECT, TestId, account, account_with, authorize_url, param, pkce};
use serde_json::json;

/// A code from a browser that is signed in already.
async fn take_code(id: &TestId, b: &mut Browser, challenge: &str) -> String {
    param(&b.get(&authorize_url(id, challenge, &[])).await.location.unwrap(), "code").unwrap()
}

/// A new sign-in flow (prompt=login), its id.
async fn start(id: &TestId, b: &mut Browser, challenge: &str) -> String {
    param(&b.get(&authorize_url(id, challenge, &[("prompt", "login")])).await.location.unwrap(), "flow").unwrap()
}

#[tokio::test]
async fn a_person_signs_in_and_gets_tokens() {
    let id = TestId::start().await;
    let ada = account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let (verifier, challenge) = pkce();
    let a = b.get(&authorize_url(&id, &challenge, &[])).await;
    assert_eq!(a.status, 303);
    assert!(a.location.as_deref().unwrap().starts_with(&format!("{}/login?flow=flw_", id.public)));
    assert!(b.cookies.contains_key("pid_flow"), "the flow is bound to the browser");
    let flow = param(a.location.as_deref().unwrap(), "flow").unwrap();

    let (status, info) = id.get_json(&format!("{}/ui-api/v1/flows/{flow}", id.public)).await;
    assert_eq!(status, 410, "without the flow cookie, nobody may read the flow: {info}");
    let info = b.get(&format!("{}/ui-api/v1/flows/{flow}", id.public)).await;
    assert_eq!((info.json["stage"].as_str(), info.json["brand"].as_str()), (Some("start"), Some("gateway")));

    let a = b
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
            json!({ "email": "ADA@example.com ", "password": common::PASSWORD }),
        )
        .await;
    assert_eq!(a.status, 200, "{a:?}");
    let to = a.json["to"].as_str().unwrap();
    assert!(to.starts_with(CONSOLE_REDIRECT));
    assert_eq!(param(to, "state").as_deref(), Some("st-1"));
    assert_eq!(param(to, "iss"), Some(format!("http://127.0.0.1:{}", id.running.public.port())), "RFC 9207");
    assert!(
        b.cookies.contains_key("pid_sid") && b.cookies.contains_key("pid_dev") && !b.cookies.contains_key("pid_flow")
    );

    let (status, tokens) = id.redeem(&param(to, "code").unwrap(), &verifier).await;
    assert_eq!(status, 200, "{tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert!(tokens["refresh_token"].as_str().unwrap().starts_with("pidr_"));

    let (typ, at) = id.verify(tokens["access_token"].as_str().unwrap()).await;
    assert_eq!(typ, "at+jwt", "RFC 9068");
    assert_eq!(
        (at["aud"].as_str(), at["client_id"].as_str(), at["pid_kind"].as_str()),
        (Some("pacod-gw"), Some("console"), Some("user"))
    );
    assert_eq!(at["sub"], ada.id);
    assert_eq!(at["email"], "ada@example.com");

    let (typ, it) = id.verify(tokens["id_token"].as_str().unwrap()).await;
    assert_eq!(typ, "JWT");
    assert_eq!(
        (it["aud"].as_str(), it["azp"].as_str(), it["nonce"].as_str()),
        (Some("console"), Some("console"), Some("n-1"))
    );
    assert_eq!(it["sid"], at["sid"]);
    assert_eq!(it["amr"], json!(["pwd"]));
    assert_eq!(it["email_verified"], true);
    let digest = ring::digest::digest(&ring::digest::SHA256, tokens["access_token"].as_str().unwrap().as_bytes());
    assert_eq!(it["at_hash"], B64.encode(&digest.as_ref()[..16]));
    id.running.stop().await;
}

#[tokio::test]
async fn single_sign_on_skips_the_password_unless_asked_not_to() {
    let id = TestId::start().await;
    account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    id.sign_in(&mut b, "ada@example.com").await;

    // Signed in: a second authorize answers with a code at once.
    let (_, challenge) = pkce();
    let a = b.get(&authorize_url(&id, &challenge, &[])).await;
    let to = a.location.unwrap();
    assert!(to.starts_with(CONSOLE_REDIRECT) && param(&to, "code").is_some(), "{to}");
    // prompt=login and max_age=0 ask for the password again.
    for extra in [("prompt", "login"), ("max_age", "0")] {
        let a = b.get(&authorize_url(&id, &challenge, &[extra])).await;
        assert!(a.location.unwrap().contains("/login?flow="), "{extra:?}");
    }
    // prompt=none works only with a session.
    let a = b.get(&authorize_url(&id, &challenge, &[("prompt", "none")])).await;
    assert!(param(&a.location.unwrap(), "code").is_some());
    let mut stranger = Browser::new(&id);
    let a = stranger.get(&authorize_url(&id, &challenge, &[("prompt", "none")])).await;
    let to = a.location.unwrap();
    assert_eq!(param(&to, "error").as_deref(), Some("login_required"));
    assert_eq!(param(&to, "state").as_deref(), Some("st-1"));
    assert!(param(&to, "iss").is_some());
    id.running.stop().await;
}

#[tokio::test]
async fn authorize_never_redirects_to_an_unregistered_address() {
    let id = TestId::start().await;
    let (_, challenge) = pkce();
    let mut b = Browser::new(&id);
    for (extra, why) in [
        (vec![("client_id", "unknown")], "unknown client"),
        (vec![("redirect_uri", "https://evil.example/cb")], "unregistered redirect"),
        (vec![("redirect_uri", "http://127.0.0.1:13000/auth/callback/x")], "a longer redirect"),
        (vec![("client_id", "gw-core")], "a service client"),
    ] {
        let a = b.get(&authorize_url(&id, &challenge, &extra)).await;
        assert_eq!((a.status, a.location.is_none()), (400, true), "{why}: {a:?}");
        assert!(a.text.contains("The sign-in cannot start"), "{why}");
    }
    // Later errors go back to the client, with state and iss.
    for (extra, error) in [
        (vec![("code_challenge", "")], "invalid_request"),
        (vec![("code_challenge_method", "plain")], "invalid_request"),
        (vec![("response_type", "token")], "unsupported_response_type"),
        (vec![("request", "eyJ...")], "request_not_supported"),
        (vec![("request_uri", "https://x.example/r")], "request_uri_not_supported"),
        (vec![("scope", "email")], "invalid_scope"),
        (vec![("prompt", "none login")], "invalid_request"),
        (vec![("response_mode", "fragment")], "invalid_request"),
    ] {
        let a = b.get(&authorize_url(&id, &challenge, &extra)).await;
        let to = a.location.unwrap_or_else(|| panic!("{error}: {:?}", a.text));
        assert!(to.starts_with(CONSOLE_REDIRECT), "{to}");
        assert_eq!(param(&to, "error").as_deref(), Some(error), "{extra:?}");
        assert_eq!(param(&to, "state").as_deref(), Some("st-1"));
    }
    // A parameter twice cannot be trusted.
    let twice = format!("{}&state=again", authorize_url(&id, &challenge, &[]));
    assert_eq!(b.get(&twice).await.status, 400);
    // POST works as GET (OIDC Core 3.1.2.1).
    let form: Vec<(String, String)> =
        url::Url::parse(&authorize_url(&id, &challenge, &[])).unwrap().query_pairs().into_owned().collect();
    let r = id.http.post(format!("{}/oidc/authorize", id.public)).form(&form).send().await.unwrap();
    assert_eq!(r.status(), 303);
    id.running.stop().await;
}

#[tokio::test]
async fn codes_are_single_use_and_bound_to_their_request() {
    let id = TestId::start().await;
    account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (status, first) = id.redeem(&code, &verifier).await;
    assert_eq!(status, 200);
    // The same code again: refused, and what it produced is revoked.
    let (status, again) = id.redeem(&code, &verifier).await;
    assert_eq!((status, again["error"].as_str()), (400, Some("invalid_grant")));
    let (status, _) =
        id.token(&[("grant_type", "refresh_token"), ("refresh_token", first["refresh_token"].as_str().unwrap())]).await;
    assert_eq!(status, 400, "the replayed code's refresh token is revoked");

    // A wrong verifier, a wrong redirect URI.
    let (_, challenge) = pkce();
    let code = take_code(&id, &mut b, &challenge).await;
    let (status, r) = id.redeem(&code, &"x".repeat(43)).await;
    assert_eq!((status, r["error"].as_str()), (400, Some("invalid_grant")));
    let code = take_code(&id, &mut b, &challenge).await;
    let (status, r) = id
        .token(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", "http://127.0.0.1:13000/other"),
            ("code_verifier", &verifier),
        ])
        .await;
    assert_eq!((status, r["error"].as_str()), (400, Some("invalid_grant")));
    id.running.stop().await;
}

#[tokio::test]
async fn client_authentication_and_request_format() {
    let id = TestId::start().await;
    let url = format!("{}/oidc/token", id.public);
    // A wrong secret by Basic: 401 with a challenge.
    let r = id
        .http
        .post(&url)
        .basic_auth("console", Some("wrong"))
        .form(&[("grant_type", "refresh_token"), ("refresh_token", "x")])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert!(r.headers()["www-authenticate"].to_str().unwrap().starts_with("Basic"));
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.json::<serde_json::Value>().await.unwrap()["error"], "invalid_client");
    // client_secret_post is accepted (the grant is what fails here).
    let r = id
        .http
        .post(&url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", "x"),
            ("client_id", "console"),
            ("client_secret", common::CLIENT_SECRET),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<serde_json::Value>().await.unwrap()["error"], "invalid_grant");
    // A public client with a secret is refused; without one it is known.
    let r = id
        .http
        .post(&url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", "x"),
            ("client_id", "bazaar-android"),
            ("client_secret", "s"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // JSON is not a token request.
    let r = id
        .http
        .post(&url)
        .basic_auth("console", Some(common::CLIENT_SECRET))
        .json(&json!({ "grant_type": "refresh_token" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<serde_json::Value>().await.unwrap()["error"], "invalid_request");
    // A grant the client may not use.
    let (status, r) = id.token(&[("grant_type", "client_credentials")]).await;
    assert_eq!((status, r["error"].as_str()), (400, Some("unauthorized_client")));
    id.running.stop().await;
}

#[tokio::test]
async fn refresh_tokens_rotate_repeat_once_and_end_everything_when_reused() {
    let id = TestId::start().await;
    let ada = account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (_, first) = id.redeem(&code, &verifier).await;
    let r1 = first["refresh_token"].as_str().unwrap().to_owned();

    // Two tabs refresh with the same token at once: both get the same successor.
    let form = [("grant_type", "refresh_token"), ("refresh_token", r1.as_str())];
    let ((s1, a), (s2, c)) = tokio::join!(id.token(&form), id.token(&form));
    assert_eq!((s1, s2), (200, 200), "{a} {c}");
    assert_eq!(a["refresh_token"], c["refresh_token"]);
    let r2 = a["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(r1, r2);
    assert!(a["id_token"].is_string());

    // After the grace window the old token is a reuse: everything of the account ends.
    id.sql("UPDATE grants SET successor_until = now() - interval '1 second'").await;
    id.store().oidc_prune().await.unwrap();
    let sealed: i64 = sqlx::query_scalar("SELECT count(*) FROM grants WHERE successor_enc IS NOT NULL")
        .fetch_one(id.store().pool())
        .await
        .unwrap();
    assert_eq!(sealed, 0, "the successor is not kept past its window");
    let (status, r) = id.token(&form).await;
    assert_eq!((status, r["error"].as_str()), (400, Some("invalid_grant")));
    let (status, _) = id.token(&[("grant_type", "refresh_token"), ("refresh_token", &r2)]).await;
    assert_eq!(status, 400, "the current token died with the rest");
    let (_, challenge) = pkce();
    let a = b.get(&authorize_url(&id, &challenge, &[])).await;
    assert!(a.location.unwrap().contains("/login?flow="), "the browser's session ended too");
    let sent = id.mail().await;
    assert!(sent.iter().any(|m| m.template == "session_revoked_reuse" && m.to == "ada@example.com"), "{sent:?}");
    let changes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM authz_changes WHERE kind = 'session.revoked' AND principal_id = $1")
            .bind(&ada.id)
            .fetch_one(id.store().pool())
            .await
            .unwrap();
    assert!(changes >= 1, "the feed hears of it");
    id.running.stop().await;
}

#[tokio::test]
async fn failed_sign_ins_lock_out_new_browsers_only() {
    let id = TestId::start().await;
    account(&id, "ada@example.com").await;
    // The owner's browser has signed in before.
    let mut own = Browser::new(&id);
    id.sign_in(&mut own, "ada@example.com").await;

    let mut attacker = Browser::new(&id);
    let (_, challenge) = pkce();
    let flow = start(&id, &mut attacker, &challenge).await;
    for n in 0..10 {
        let a = attacker
            .post(
                &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
                json!({ "email": "ada@example.com", "password": format!("guess {n}") }),
            )
            .await;
        assert_eq!((a.status, a.json["error"]["details"]["code"].as_str()), (401, Some("invalid_credentials")));
    }
    let a = attacker
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
            json!({ "email": "ada@example.com", "password": common::PASSWORD }),
        )
        .await;
    assert_eq!(a.status, 429, "locked, even with the right password: {a:?}");
    // The owner signs in from the browser the account knows.
    let flow = start(&id, &mut own, &challenge).await;
    let a = own
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
            json!({ "email": "ada@example.com", "password": common::PASSWORD }),
        )
        .await;
    assert_eq!(a.status, 200, "{a:?}");
    // An unknown address answers like a wrong password.
    let flow = start(&id, &mut own, &challenge).await;
    let a = own
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
            json!({ "email": "nobody@example.com", "password": "whatever it is" }),
        )
        .await;
    assert_eq!((a.status, a.json["error"]["details"]["code"].as_str()), (401, Some("invalid_credentials")));
    id.running.stop().await;
}

#[tokio::test]
async fn an_unconfirmed_address_waits_for_its_link() {
    let id = TestId::start().await;
    let bob = account_with(&id, "bob@example.com", common::PASSWORD, false).await;
    let mut b = Browser::new(&id);
    let (_, challenge) = pkce();
    let flow = param(&b.get(&authorize_url(&id, &challenge, &[])).await.location.unwrap(), "flow").unwrap();
    let a = b
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
            json!({ "email": "bob@example.com", "password": common::PASSWORD, "remember_me": true }),
        )
        .await;
    assert_eq!(a.json["next"], "verify_email", "{a:?}");
    let sent = id.mail().await;
    let mail = sent.iter().find(|m| m.template == "verify_email").expect("a confirmation mail");
    assert!(
        mail.mail.text.contains("/verify-email?token=") && mail.mail.subject.starts_with("Потвърдете"),
        "Bulgarian first"
    );
    // Not confirmed yet: still waiting.
    let a = b.post(&format!("{}/ui-api/v1/flows/{flow}/resume", id.public), json!({})).await;
    assert_eq!(a.json["next"], "verify_email");
    id.store().account_mark_verified(&bob.id, "verified").await.unwrap();
    let a = b.post(&format!("{}/ui-api/v1/flows/{flow}/resume", id.public), json!({})).await;
    assert_eq!(a.json["next"], "redirect", "{a:?}");
    id.running.stop().await;
}

#[tokio::test]
async fn a_role_holder_with_a_short_password_chooses_a_longer_one() {
    let id = TestId::start().await;
    let carol = account_with(&id, "carol@example.com", "tenletters", true).await;
    id.sql(&format!(
        "INSERT INTO role_bindings (id, principal_id, principal_kind, role, scope_type) VALUES ('rb_1', '{}', 'user', 'gw.platform_admin', 'platform')",
        carol.id
    ))
    .await;
    let mut b = Browser::new(&id);
    let (_, challenge) = pkce();
    let flow = param(&b.get(&authorize_url(&id, &challenge, &[])).await.location.unwrap(), "flow").unwrap();
    let a = b
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
            json!({ "email": "carol@example.com", "password": "tenletters" }),
        )
        .await;
    assert_eq!((a.json["next"].as_str(), a.json["min_length"].as_i64()), (Some("change_password"), Some(12)), "{a:?}");
    let url = format!("{}/ui-api/v1/flows/{flow}/password-change", id.public);
    let a = b.post(&url, json!({ "password": "password1234" })).await;
    assert_eq!((a.status, a.json["error"]["details"]["code"].as_str()), (422, Some("password_too_common")));
    let a = b.post(&url, json!({ "password": "short" })).await;
    assert_eq!(a.json["error"]["details"]["code"], "password_too_short");
    let a = b.post(&url, json!({ "password": "a much longer passphrase" })).await;
    assert_eq!(a.json["next"], "redirect", "{a:?}");
    assert!(id.mail().await.iter().any(|m| m.template == "password_changed"));
    id.running.stop().await;
}

/// The stage of a new sign-in flow for the browser's session.
async fn flow_stage(id: &TestId, b: &mut Browser) -> String {
    let (_, challenge) = pkce();
    let flow = param(&b.get(&authorize_url(id, &challenge, &[])).await.location.unwrap(), "flow").unwrap();
    let a = b.get(&format!("{}/ui-api/v1/flows/{flow}", id.public)).await;
    a.json["stage"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn an_old_session_cannot_set_a_due_password_without_the_current_one() {
    let id = TestId::start().await;
    let ada = account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    id.sign_in(&mut b, "ada@example.com").await;
    id.sql(&format!("UPDATE accounts SET password_change_due = true WHERE id = '{}'", ada.id)).await;
    // Signed in moments ago: straight to choosing a new password.
    assert_eq!(flow_stage(&id, &mut b).await, "change_password");
    // An hour later (or a stolen cookie): the password first.
    id.sql(&format!("UPDATE sso_sessions SET auth_time = now() - interval '1 hour' WHERE account_id = '{}'", ada.id))
        .await;
    assert_eq!(flow_stage(&id, &mut b).await, "start");
    id.running.stop().await;
}

#[tokio::test]
async fn the_ui_api_takes_writes_from_its_own_pages_only() {
    let id = TestId::start().await;
    account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let (_, challenge) = pkce();
    let flow = param(&b.get(&authorize_url(&id, &challenge, &[])).await.location.unwrap(), "flow").unwrap();
    b.origin = "https://evil.example".into();
    let a = b
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
            json!({ "email": "ada@example.com", "password": common::PASSWORD }),
        )
        .await;
    assert_eq!((a.status, a.json["error"]["details"]["code"].as_str()), (403, Some("cross_origin")));
    // Another browser, without the flow's cookie.
    let mut other = Browser::new(&id);
    let a = other
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
            json!({ "email": "ada@example.com", "password": common::PASSWORD }),
        )
        .await;
    assert_eq!(a.status, 410);
    id.running.stop().await;
}

#[tokio::test]
async fn services_get_client_credentials_and_exchange_tokens() {
    let id = TestId::start().await;
    let core_secret = "a-secret-for-the-gateway-core-0123456789";
    id.store()
        .client_set_secret("gw-core", "gateway", &id.running.state.boot.token_pepper.hash(core_secret))
        .await
        .unwrap();
    let as_core = |form: Vec<(&'static str, String)>| {
        let http = id.http.clone();
        let url = format!("{}/oidc/token", id.internal);
        async move {
            let r = http.post(url).basic_auth("gw-core", Some(core_secret)).form(&form).send().await.unwrap();
            (r.status().as_u16(), r.json::<serde_json::Value>().await.unwrap())
        }
    };
    let (status, cc) = as_core(vec![("grant_type", "client_credentials".into())]).await;
    assert_eq!(status, 200, "{cc}");
    let (_, claims) = id.verify(cc["access_token"].as_str().unwrap()).await;
    assert_eq!(
        (claims["aud"].as_str(), claims["pid_kind"].as_str(), claims["sub"].as_str()),
        (Some("pacod-id"), Some("service"), Some("gw-core"))
    );

    // A person's token, swapped for a short one for pacod-id's API.
    let ada = account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (_, tokens) = id.redeem(&code, &verifier).await;
    let access = tokens["access_token"].as_str().unwrap().to_owned();
    let exchange = |token: String, requested: Option<&'static str>| {
        let mut form = vec![
            ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange".to_owned()),
            ("subject_token", token),
            ("subject_token_type", "urn:ietf:params:oauth:token-type:access_token".to_owned()),
        ];
        if let Some(r) = requested {
            form.push(("requested_token_type", r.to_owned()));
        }
        as_core(form)
    };
    let (status, ex) = exchange(access.clone(), None).await;
    assert_eq!(status, 200, "{ex}");
    let (_, claims) = id.verify(ex["access_token"].as_str().unwrap()).await;
    assert_eq!((claims["aud"].as_str(), claims["sub"].as_str()), (Some("pacod-id"), Some(ada.id.as_str())));
    assert_eq!(claims["act"]["sub"], "gw-core");
    assert!(claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap() <= 60);

    // An id_token is no access token, though the same key signed it.
    let (status, r) = exchange(tokens["id_token"].as_str().unwrap().to_owned(), None).await;
    assert_eq!((status, r["error"].as_str()), (400, Some("invalid_grant")));

    // A delegated grant: refreshable by the service, ended with the person's session.
    let (status, d) = exchange(access, Some("urn:ietf:params:oauth:token-type:refresh_token")).await;
    assert_eq!(status, 200, "{d}");
    let delegated = d["refresh_token"].as_str().unwrap().to_owned();
    let (_, claims) = id.verify(d["access_token"].as_str().unwrap()).await;
    assert_eq!((claims["aud"].as_str(), claims["act"]["sub"].as_str()), (Some("pacod-gw"), Some("gw-core")));
    let (status, refreshed) = as_core(vec![("grant_type", "refresh_token".into()), ("refresh_token", delegated)]).await;
    assert_eq!(status, 200, "{refreshed}");
    pacod_id::oidc::sign_out_everywhere(id.store(), &ada.id, "test").await.unwrap();
    let (status, _) = as_core(vec![
        ("grant_type", "refresh_token".into()),
        ("refresh_token", refreshed["refresh_token"].as_str().unwrap().to_owned()),
    ])
    .await;
    assert_eq!(status, 400, "signing out stops a delegated grant");
    id.running.stop().await;
}

#[tokio::test]
async fn only_a_known_earlier_refresh_token_counts_as_reuse() {
    let id = TestId::start().await;
    account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (_, first) = id.redeem(&code, &verifier).await;
    let refresh = |t: String| {
        let id = &id;
        async move { id.token(&[("grant_type", "refresh_token"), ("refresh_token", &t)]).await }
    };
    let r1 = first["refresh_token"].as_str().unwrap().to_owned();
    let (_, a) = refresh(r1.clone()).await;
    let r2 = a["refresh_token"].as_str().unwrap().to_owned();
    let (_, a) = refresh(r2.clone()).await;
    let r3 = a["refresh_token"].as_str().unwrap().to_owned();

    // The grant's id with a secret it never had: refused, and nothing else happens.
    let (head, _) = r3.rsplit_once('_').unwrap();
    let forged = format!("{head}_{}", "x".repeat(43));
    let (status, r) = refresh(forged).await;
    assert_eq!((status, r["error"].as_str()), (400, Some("invalid_grant")));
    let (status, a) = refresh(r3).await;
    assert_eq!(status, 200, "the forged token signed nobody out: {a}");
    assert!(!id.mail().await.iter().any(|m| m.template == "session_revoked_reuse"));

    // A token two rotations back (spent, not the previous one) is reuse: everything ends.
    let r4 = a["refresh_token"].as_str().unwrap().to_owned();
    let (status, _) = refresh(r1).await;
    assert_eq!(status, 400);
    let (status, _) = refresh(r4).await;
    assert_eq!(status, 400, "the current token died with the rest");
    assert!(id.mail().await.iter().any(|m| m.template == "session_revoked_reuse"));
    id.running.stop().await;
}

#[tokio::test]
async fn another_clients_code_neither_redeems_nor_revokes() {
    let id = TestId::start().await;
    account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (status, first) = id.redeem(&code, &verifier).await;
    assert_eq!(status, 200);
    // The used code, presented by a public client of another realm.
    let r = id
        .http
        .post(format!("{}/oidc/token", id.public))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", "bazaar-android"),
            ("code", code.as_str()),
            ("redirect_uri", CONSOLE_REDIRECT),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let (status, _) =
        id.token(&[("grant_type", "refresh_token"), ("refresh_token", first["refresh_token"].as_str().unwrap())]).await;
    assert_eq!(status, 200, "the console's grant lives on");

    // Presented by the other client first, a fresh code still redeems for its own.
    let (code, verifier) = id.sign_in(&mut Browser::new(&id), "ada@example.com").await;
    let r = id
        .http
        .post(format!("{}/oidc/token", id.public))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", "bazaar-android"),
            ("code", code.as_str()),
            ("redirect_uri", CONSOLE_REDIRECT),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(id.redeem(&code, &verifier).await.0, 200, "the code was not spent by the other client");
    id.running.stop().await;
}

#[tokio::test]
async fn a_burst_of_parallel_guesses_gets_no_more_tries_than_the_limit() {
    let id = TestId::start().await;
    account(&id, "ada@example.com").await;
    let mut attacker = Browser::new(&id);
    let (_, challenge) = pkce();
    let flow = start(&id, &mut attacker, &challenge).await;
    let url = format!("{}/ui-api/v1/flows/{flow}/password", id.public);
    let guesses: Vec<_> = (0..40)
        .map(|n| {
            let r = id
                .http
                .post(&url)
                .header("cookie", attacker.cookie_header())
                .header("origin", &attacker.origin)
                .json(&json!({ "email": "ada@example.com", "password": format!("guess {n}") }));
            tokio::spawn(async move { r.send().await.unwrap().status().as_u16() })
        })
        .collect();
    let mut statuses = Vec::new();
    for g in guesses {
        statuses.push(g.await.unwrap());
    }
    let tried = statuses.iter().filter(|s| **s == 401).count();
    let refused = statuses.iter().filter(|s| **s == 429).count();
    assert_eq!((tried, refused), (10, 30), "{statuses:?}");
    id.running.stop().await;
}

#[tokio::test]
async fn a_session_of_someone_else_than_the_hint_names_is_not_used() {
    let id = TestId::start().await;
    account(&id, "ada@example.com").await;
    account(&id, "bob@example.com").await;
    let mut ada = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut ada, "ada@example.com").await;
    let ada_token = id.redeem(&code, &verifier).await.1["id_token"].as_str().unwrap().to_owned();
    let mut bob = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut bob, "bob@example.com").await;
    let bob_token = id.redeem(&code, &verifier).await.1["id_token"].as_str().unwrap().to_owned();

    let (_, challenge) = pkce();
    let silent = |hint: &str| authorize_url(&id, &challenge, &[("prompt", "none"), ("id_token_hint", hint)]);
    let to = ada.get(&silent(&ada_token)).await.location.unwrap();
    assert!(param(&to, "code").is_some(), "the hint names the session's account: {to}");
    let to = ada.get(&silent(&bob_token)).await.location.unwrap();
    assert_eq!(param(&to, "error").as_deref(), Some("login_required"), "{to}");
    let to = ada.get(&silent("not.a.token")).await.location.unwrap();
    assert_eq!(param(&to, "error").as_deref(), Some("invalid_request"), "{to}");
    id.running.stop().await;
}
