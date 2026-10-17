//! Stage 5: sign-up, confirming an address, and resetting a password.

mod common;

use common::{Browser, PASSWORD, TestId, account, account_with, authorize_url, param, pkce};
use serde_json::{Value, json};

const BAZAAR_REDIRECT: &str = "https://bazaar.example/native/oidc/callback";

/// The token in a mail's link.
fn link_token(text: &str) -> String {
    let at = text.find("token=").expect("a link with a token") + "token=".len();
    text[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

/// A sign-in flow of the bazaar app in `b`; its id and the PKCE verifier.
async fn bazaar_flow(id: &TestId, b: &mut Browser) -> (String, String) {
    let (verifier, challenge) = pkce();
    let url = authorize_url(id, &challenge, &[("client_id", "bazaar-android"), ("redirect_uri", BAZAAR_REDIRECT)]);
    let a = b.get(&url).await;
    let to = a.location.clone().unwrap_or_else(|| panic!("{a:?}"));
    assert!(to.starts_with(&b.origin), "the flow stays on the realm's address: {to}");
    (param(&to, "flow").unwrap(), verifier)
}

async fn accounts_with(id: &TestId, email: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM accounts WHERE email = $1")
        .bind(email)
        .fetch_one(id.store().pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn signing_up_confirming_and_signing_in() {
    let id = TestId::start().await;
    let mut b = Browser::bazaar(&id);
    let (flow, verifier) = bazaar_flow(&id, &mut b).await;
    let signup = format!("{}/ui-api/v1/flows/{flow}/signup", b.origin);
    let a = b.post(&signup, json!({ "email": "nia@example.com", "password": "short" })).await;
    assert_eq!(
        (a.status, a.json["error"]["details"]["code"].as_str(), a.json["error"]["details"]["min_length"].as_i64()),
        (422, Some("password_too_short"), Some(8)),
        "bazaar's minimum"
    );
    let a = b.post(&signup, json!({ "email": "not-an-address", "password": "violet skies" })).await;
    assert_eq!(a.json["error"]["details"]["code"], "bad_email");
    let a = b
        .post(
            &signup,
            json!({ "email": " Nia@Example.com ", "password": "violet skies", "display_name": "Nia", "locale": "bg" }),
        )
        .await;
    assert_eq!(
        (a.status, a.json["next"].as_str(), a.json["email"].as_str()),
        (200, Some("verify_email"), Some("nia@example.com")),
        "{a:?}"
    );
    let nia = id.store().account_by_email("nia@example.com").await.unwrap().unwrap();
    assert!(!nia.is_verified());
    assert_eq!((nia.locale.as_str(), nia.signup_realm.as_str()), ("bg", "bazaar"));
    let sent = id.mail().await;
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0].template.as_str(), sent[0].to.as_str()), ("verify_email", "nia@example.com"));
    assert!(sent[0].mail.text.contains(&format!("{}/verify-email?token=", b.origin)), "bazaar's own address");
    let token = link_token(&sent[0].mail.text);

    // The flow is spent on its first step; a second sign-up with the address answers alike.
    let a = b.post(&signup, json!({ "email": "nia@example.com", "password": "violet skies" })).await;
    assert_eq!(a.json["error"]["details"]["code"], "wrong_stage");
    let mut other = Browser::bazaar(&id);
    let (other_flow, _) = bazaar_flow(&id, &mut other).await;
    let a = other
        .post(
            &format!("{}/ui-api/v1/flows/{other_flow}/signup", other.origin),
            json!({ "email": "nia@example.com", "password": "another phrase" }),
        )
        .await;
    assert_eq!((a.status, a.json["next"].as_str()), (200, Some("verify_email")), "the same answer for a taken address");
    assert_eq!(accounts_with(&id, "nia@example.com").await, 1);
    let sent = id.mail().await;
    assert_eq!(sent.iter().map(|m| m.template.as_str()).collect::<Vec<_>>(), ["account_exists"]);
    // Nor does the flow tell: the same stage and address as a new one, and waiting never ends.
    let mine = b.get(&format!("{}/ui-api/v1/flows/{flow}", b.origin)).await;
    let theirs = other.get(&format!("{}/ui-api/v1/flows/{other_flow}", other.origin)).await;
    for f in [&mine, &theirs] {
        assert_eq!(
            (f.json["stage"].as_str(), f.json["login_hint"].as_str()),
            (Some("verify_email"), Some("nia@example.com"))
        );
    }
    let a = other.post(&format!("{}/ui-api/v1/flows/{other_flow}/resume", other.origin), json!({})).await;
    assert_eq!(
        (a.status, a.json["next"].as_str(), a.json["email"].as_str()),
        (200, Some("verify_email"), Some("nia@example.com"))
    );

    // Waiting for the link; the link works on its own realm's address only, and once.
    let resume = format!("{}/ui-api/v1/flows/{flow}/resume", b.origin);
    assert_eq!(b.post(&resume, json!({})).await.json["next"], "verify_email");
    let mut gateway = Browser::new(&id);
    let a = gateway.post(&format!("{}/ui-api/v1/verification/confirm", id.public), json!({ "token": token })).await;
    assert_eq!(a.status, 410);
    let confirm = format!("{}/ui-api/v1/verification/confirm", b.origin);
    let a = b.post(&confirm, json!({ "token": token })).await;
    assert_eq!((a.status, a.json["email"].as_str()), (200, Some("nia@example.com")), "{a:?}");
    assert_eq!(b.post(&confirm, json!({ "token": token })).await.status, 410);
    assert!(id.store().account(&nia.id).await.unwrap().unwrap().is_verified());

    let a = b.post(&resume, json!({})).await;
    assert_eq!(a.json["next"], "redirect", "{a:?}");
    let waiting = other.post(&format!("{}/ui-api/v1/flows/{other_flow}/resume", other.origin), json!({})).await;
    assert_eq!(waiting.json["next"], "verify_email", "the other flow is bound to nobody: {waiting:?}");
    let code = param(a.json["to"].as_str().unwrap(), "code").unwrap();
    let r = id
        .http
        .post(format!("{}/oidc/token", id.public))
        .header("host", b.host.clone().unwrap())
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", "bazaar-android"),
            ("code", &code),
            ("redirect_uri", BAZAAR_REDIRECT),
            ("code_verifier", &verifier),
        ])
        .send()
        .await
        .unwrap();
    let tokens: Value = r.json().await.unwrap();
    let (_, claims) = id.verify(tokens["id_token"].as_str().unwrap()).await;
    assert_eq!((claims["sub"].as_str(), claims["email_verified"].as_bool()), (Some(nia.id.as_str()), Some(true)));
    id.running.stop().await;
}

#[tokio::test]
async fn sign_up_is_closed_where_accounts_come_by_invitation() {
    let id = TestId::start().await;
    let mut b = Browser::new(&id);
    let (_, challenge) = pkce();
    let flow = param(&b.get(&authorize_url(&id, &challenge, &[])).await.location.unwrap(), "flow").unwrap();
    let a = b
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/signup", id.public),
            json!({ "email": "eve@example.com", "password": PASSWORD }),
        )
        .await;
    assert_eq!((a.status, a.json["error"]["details"]["code"].as_str()), (403, Some("signup_closed")));
    assert_eq!(accounts_with(&id, "eve@example.com").await, 0);
    id.running.stop().await;
}

#[tokio::test]
async fn without_mail_a_sign_up_signs_in_at_once() {
    let db = common::fresh_database().await;
    let id = TestId::start_with(&db, |_| String::new(), |boot| boot.mail = None).await;
    let mut b = Browser::bazaar(&id);
    let (flow, _) = bazaar_flow(&id, &mut b).await;
    let a = b
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/signup", b.origin),
            json!({ "email": "nia@example.com", "password": "violet skies" }),
        )
        .await;
    assert_eq!(a.json["next"], "redirect", "{a:?}");
    assert!(param(a.json["to"].as_str().unwrap(), "code").is_some());
    assert!(b.cookies.contains_key("pid_sid"));
    let nia = id.store().account_by_email("nia@example.com").await.unwrap().unwrap();
    assert!(!nia.is_verified(), "nothing proved the address");
    id.running.stop().await;
}

#[tokio::test]
async fn resending_the_confirmation() {
    let id = TestId::start().await;
    account_with(&id, "bob@example.com", PASSWORD, false).await;
    account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let url = format!("{}/ui-api/v1/verification", id.public);
    for email in ["bob@example.com", "ada@example.com", "nobody@example.com"] {
        let a = b.post(&url, json!({ "email": email })).await;
        assert_eq!((a.status, a.json["next"].as_str()), (202, Some("check_email")), "{email}");
    }
    let sent = id.mail().await;
    assert_eq!(
        sent.iter().map(|m| (m.template.as_str(), m.to.as_str())).collect::<Vec<_>>(),
        [("verify_email", "bob@example.com")]
    );
    // Three mails per address in 15 minutes.
    for _ in 0..4 {
        b.post(&url, json!({ "email": "bob@example.com" })).await;
    }
    assert_eq!(id.mail().await.len(), 2);
    b.origin = "https://evil.example".into();
    assert_eq!(b.post(&url, json!({ "email": "bob@example.com" })).await.status, 403);
    id.running.stop().await;
}

#[tokio::test]
async fn resetting_a_password() {
    let id = TestId::start().await;
    let ada = account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    let (_, tokens) = id.redeem(&code, &verifier).await;
    // Someone guessed at Ada's password from a new browser until the address locked.
    id.sql("INSERT INTO throttle (bucket, key, window_start, count, locked_until) VALUES ('login_account', 'ada@example.com', now(), 10, now() + interval '15 minutes')").await;

    let request = format!("{}/ui-api/v1/password-reset", id.public);
    for email in ["ADA@example.com", "nobody@example.com"] {
        let a = b.post(&request, json!({ "email": email })).await;
        assert_eq!(a.status, 202, "{email}");
    }
    let sent = id.mail().await;
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0].template.as_str(), sent[0].to.as_str()), ("reset_password", "ada@example.com"));
    assert!(sent[0].mail.text.contains(&format!("{}/reset-password?token=", id.public)));
    let token = link_token(&sent[0].mail.text);

    // A refused password does not use the link up.
    let confirm = format!("{}/ui-api/v1/password-reset/confirm", id.public);
    let a = b.post(&confirm, json!({ "token": token, "password": "password1234" })).await;
    assert_eq!((a.status, a.json["error"]["details"]["code"].as_str()), (422, Some("password_too_common")));
    let a = b.post(&confirm, json!({ "token": "not-a-token", "password": "a new long passphrase" })).await;
    assert_eq!(a.status, 410);
    let mut bazaar = Browser::bazaar(&id);
    let a = bazaar
        .post(
            &format!("{}/ui-api/v1/password-reset/confirm", bazaar.origin),
            json!({ "token": token, "password": "a new long passphrase" }),
        )
        .await;
    assert_eq!(a.status, 410, "a gateway link is not taken on bazaar's address");
    let a = b.post(&confirm, json!({ "token": token, "password": "a new long passphrase" })).await;
    assert_eq!((a.status, a.json["next"].as_str()), (200, Some("sign_in")), "{a:?}");
    assert_eq!(b.post(&confirm, json!({ "token": token, "password": "yet another passphrase" })).await.status, 410);

    // Every session ended; the lock is gone; the new password works, the old one does not.
    let (status, _) = id
        .token(&[("grant_type", "refresh_token"), ("refresh_token", tokens["refresh_token"].as_str().unwrap())])
        .await;
    assert_eq!(status, 400);
    assert!(id.store().sessions_of(&ada.id).await.unwrap().is_empty());
    assert!(id.store().throttle_locked("login_account", "ada@example.com").await.unwrap().is_none());
    assert_eq!(id.mail().await.iter().map(|m| m.template.as_str()).collect::<Vec<_>>(), ["password_changed"]);
    let mut c = Browser::new(&id);
    let (_, challenge) = pkce();
    let flow = param(&c.get(&authorize_url(&id, &challenge, &[])).await.location.unwrap(), "flow").unwrap();
    let password = format!("{}/ui-api/v1/flows/{flow}/password", id.public);
    let a = c.post(&password, json!({ "email": "ada@example.com", "password": PASSWORD })).await;
    assert_eq!(a.status, 401);
    let a = c.post(&password, json!({ "email": "ada@example.com", "password": "a new long passphrase" })).await;
    assert_eq!(a.json["next"], "redirect", "{a:?}");

    // Three reset mails per address in 15 minutes.
    for _ in 0..4 {
        b.post(&request, json!({ "email": "ada@example.com" })).await;
    }
    let resets = id.mail().await.into_iter().filter(|m| m.template == "reset_password").count();
    assert_eq!(resets, 2, "one was sent already in this window");
    id.running.stop().await;
}
