//! "Keep me logged in", email verification and password reset, end to end: a real
//! PostgreSQL, and a mail sink in place of the SMTP server.

mod common;

use std::time::Duration;

use chrono::{DateTime, Utc};
use common::{ADMIN_EMAIL, ADMIN_PASSWORD, MailSink, TestGw, link_token};
use serde_json::{Value, json};

async fn with_mail(sink: &MailSink) -> TestGw {
    let db = common::fresh_database().await;
    let cfg = sink.config();
    TestGw::start_on(&db, None, move |boot| boot.email = Some(cfg)).await
}

/// A public call without a token.
async fn public(gw: &TestGw, path: &str, body: Value) -> (u16, Value) {
    let r = gw.http.post(format!("{}/api/v1{path}", gw.admin)).json(&body).send().await.expect("request");
    let status = r.status().as_u16();
    let text = r.text().await.expect("body");
    (status, if text.is_empty() { Value::Null } else { serde_json::from_str(&text).expect("json") })
}

async fn login(gw: &TestGw, email: &str, password: &str, remember_me: bool) -> (u16, Value) {
    public(gw, "/session", json!({ "email": email, "password": password, "remember_me": remember_me })).await
}

fn expires_in_hours(session: &Value) -> i64 {
    let at: DateTime<Utc> = session["expires_at"].as_str().expect("expires_at").parse().expect("time");
    (at - Utc::now()).num_minutes().div_euclid(60) + 1
}

#[tokio::test]
async fn keep_me_logged_in_lasts_days_instead_of_hours() {
    let gw = TestGw::start(None).await;
    let (st, short) = login(&gw, ADMIN_EMAIL, ADMIN_PASSWORD, false).await;
    assert_eq!(st, 200);
    assert_eq!(expires_in_hours(&short), 12, "session_hours by default");
    let (_, long) = login(&gw, ADMIN_EMAIL, ADMIN_PASSWORD, true).await;
    assert_eq!(expires_in_hours(&long), 30 * 24, "remember_me_days by default");

    // Both come from the settings.
    let (_, mut settings) = gw.api("GET", "/settings", None).await;
    settings["remember_me_days"] = json!(7);
    settings["session_hours"] = json!(2);
    assert_eq!(gw.api("PUT", "/settings", Some(settings.clone())).await.0, 200);
    assert_eq!(expires_in_hours(&login(&gw, ADMIN_EMAIL, ADMIN_PASSWORD, true).await.1), 7 * 24);
    assert_eq!(expires_in_hours(&login(&gw, ADMIN_EMAIL, ADMIN_PASSWORD, false).await.1), 2);
    settings["remember_me_days"] = json!(0);
    assert_eq!(gw.api("PUT", "/settings", Some(settings)).await.0, 422);
}

#[tokio::test]
async fn a_new_user_confirms_the_address_before_logging_in() {
    let sink = MailSink::start().await;
    let gw = with_mail(&sink).await;
    let pw = "a long enough password";
    let user = gw
        .create("/users", json!({ "email": "Ada@Example.test", "display_name": "Ada <Admin>", "password": pw }))
        .await;
    assert_eq!(user.body["email_verified"], false);

    // The verification email goes out on its own.
    let mails = sink.wait(1).await;
    assert!(mails[0].contains("To: ada@example.test"), "{}", mails[0]);
    assert!(mails[0].contains("Ada &lt;Admin&gt;"), "the name is escaped in the HTML part");
    let first = link_token(&mails[0], "/verify-email");

    // The right password is not enough yet; a wrong one still gets the usual answer.
    let (st, body) = login(&gw, "ada@example.test", pw, false).await;
    assert_eq!((st, body["error"]["type"].as_str()), (403, Some("email_not_verified")));
    assert_eq!(login(&gw, "ada@example.test", "not the password", false).await.0, 401);

    // A new link replaces the first one.
    assert_eq!(public(&gw, "/session/verification", json!({ "email": "ada@example.test" })).await.0, 202);
    let second = link_token(&sink.wait(2).await[1], "/verify-email");
    let (st, body) = public(&gw, "/session/verification/confirm", json!({ "token": first })).await;
    assert_eq!((st, body["error"]["type"].as_str()), (400, Some("invalid_token")));
    assert_eq!(public(&gw, "/session/verification/confirm", json!({ "token": second })).await.0, 204);
    assert_eq!(public(&gw, "/session/verification/confirm", json!({ "token": second })).await.0, 400, "used once");

    assert_eq!(login(&gw, "ada@example.test", pw, false).await.0, 200);
    let (_, view) = gw.api("GET", &format!("/users/{}", user.id), None).await;
    assert_eq!(view["email_verified"], true);

    // A confirmed user is not sent another link.
    assert_eq!(public(&gw, "/session/verification", json!({ "email": "ada@example.test" })).await.0, 202);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(sink.mails().len(), 2);
}

#[tokio::test]
async fn a_password_reset_replaces_the_password_and_ends_the_sessions() {
    let sink = MailSink::start().await;
    let gw = with_mail(&sink).await;
    let old_session = gw.login(ADMIN_EMAIL, ADMIN_PASSWORD).await;

    // Unknown and known addresses get the same answer; only the known one gets an email.
    assert_eq!(public(&gw, "/session/password-reset", json!({ "email": "nobody@example.test" })).await.0, 202);
    assert_eq!(public(&gw, "/session/password-reset", json!({ "email": ADMIN_EMAIL.to_uppercase() })).await.0, 202);
    let mails = sink.wait(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(sink.mails().len(), 1);
    assert!(mails[0].contains(&format!("To: {ADMIN_EMAIL}")));
    assert!(mails[0].contains("valid for 1 hour"));
    let token = link_token(&mails[0], "/reset-password");

    // A short password is refused without using up the link.
    let (st, _) = public(&gw, "/session/password-reset/confirm", json!({ "token": token, "password": "short" })).await;
    assert_eq!(st, 422);
    let new_pw = "a brand new password";
    let reset = json!({ "token": token, "password": new_pw });
    assert_eq!(public(&gw, "/session/password-reset/confirm", reset.clone()).await.0, 204);
    assert_eq!(public(&gw, "/session/password-reset/confirm", reset).await.0, 400, "used once");

    assert_eq!(gw.api_as(&old_session, "GET", "/session", None).await.0, 401, "old sessions end");
    assert_eq!(login(&gw, ADMIN_EMAIL, ADMIN_PASSWORD, false).await.0, 401);
    assert_eq!(login(&gw, ADMIN_EMAIL, new_pw, false).await.0, 200);

    // The audit log names what happened, never the token.
    let (_, log) = gw.api_as(&gw.login(ADMIN_EMAIL, new_pw).await, "GET", "/audit-log?limit=50", None).await;
    let text = log.to_string();
    assert!(text.contains("reset_password"), "{text}");
    assert!(!text.contains(&token));
}

#[tokio::test]
async fn requests_beyond_the_limit_are_dropped_silently() {
    let sink = MailSink::start().await;
    let gw = with_mail(&sink).await;
    for _ in 0..5 {
        assert_eq!(public(&gw, "/session/password-reset", json!({ "email": ADMIN_EMAIL })).await.0, 202);
    }
    sink.wait(3).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(sink.mails().len(), 3);
}

#[tokio::test]
async fn a_changed_address_must_be_confirmed_again_but_not_ones_own() {
    let sink = MailSink::start().await;
    let gw = with_mail(&sink).await;
    let pw = "a long enough password";
    let user = gw.create("/users", json!({ "email": "bob@example.test", "password": pw })).await;
    let old_link = link_token(&sink.wait(1).await[0], "/verify-email");
    assert_eq!(gw.api("POST", &format!("/users/{}/email-confirmation", user.id), None).await.0, 204);
    assert_eq!(login(&gw, "bob@example.test", pw, false).await.0, 200);

    // An admin moves Bob to another address: Bob confirms it before the next login, and the
    // link to the old address no longer works.
    let (st, view) =
        gw.api("PUT", &format!("/users/{}", user.id), Some(json!({ "email": "bob@new.example.test" }))).await;
    assert_eq!((st, &view["email_verified"]), (200, &json!(false)));
    let mails = sink.wait(2).await;
    assert!(mails[1].contains("To: bob@new.example.test"));
    assert_eq!(login(&gw, "bob@new.example.test", pw, false).await.0, 403);
    assert_eq!(public(&gw, "/session/verification/confirm", json!({ "token": old_link })).await.0, 400);

    // The admin changing their own address stays confirmed.
    let (_, me) = gw.api("GET", "/session", None).await;
    let (st, view) = gw
        .api("PUT", &format!("/users/{}", me["id"].as_str().unwrap()), Some(json!({ "email": "root@example.test" })))
        .await;
    assert_eq!((st, &view["email_verified"]), (200, &json!(true)));
}

#[tokio::test]
async fn admins_send_the_emails_from_the_user_list() {
    let sink = MailSink::start().await;
    let gw = with_mail(&sink).await;
    let user = gw.create("/users", json!({ "email": "cy@example.test", "password": "a long enough password" })).await;
    sink.wait(1).await;

    assert_eq!(gw.api("POST", &format!("/users/{}/verification", user.id), None).await.0, 204);
    assert_eq!(gw.api("POST", &format!("/users/{}/password-reset", user.id), None).await.0, 204);
    let mails = sink.wait(3).await;
    assert!(mails[1].contains("/verify-email?token="));
    assert!(mails[2].contains("/reset-password?token="));

    assert_eq!(gw.api("POST", &format!("/users/{}/email-confirmation", user.id), None).await.0, 204);
    assert_eq!(gw.api("POST", &format!("/users/{}/verification", user.id), None).await.0, 409, "already confirmed");

    // Only platform admins.
    let token = gw.login("cy@example.test", "a long enough password").await;
    assert_eq!(gw.api_as(&token, "POST", &format!("/users/{}/password-reset", user.id), None).await.0, 403);
}

#[tokio::test]
async fn without_email_accounts_need_no_confirmation() {
    let gw = TestGw::start(None).await;
    let user = gw.create("/users", json!({ "email": "dee@example.test", "password": "a long enough password" })).await;
    assert_eq!(user.body["email_verified"], true);
    assert_eq!(login(&gw, "dee@example.test", "a long enough password", false).await.0, 200);

    let (st, body) = public(&gw, "/session/password-reset", json!({ "email": "dee@example.test" })).await;
    assert_eq!((st, body["error"]["type"].as_str()), (503, Some("email_unavailable")));
    assert_eq!(gw.api("POST", &format!("/users/{}/password-reset", user.id), None).await.0, 503);
}
