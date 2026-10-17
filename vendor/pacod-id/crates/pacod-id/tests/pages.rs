//! Stage 8: serving the hosted pages, the account pages' API, and the admin pages by cookie.

mod common;

use base64::Engine;
use common::{Browser, TestId};
use pacod_id::authz::bindings::{self, NewBinding, Protect};
use pacod_id_authz::ScopeRef;
use serde_json::{Value, json};

const BOOT: &str = "\n\t\t\tstart_the_app();\n\t\t";

/// A small build like SvelteKit's: the page with its placeholders, one module.
fn build() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pid-ui-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(dir.join("_app/immutable")).unwrap();
    std::fs::write(
        dir.join("index.html"),
        format!(
            "<!doctype html><html lang=\"__PID_LANG__\" data-brand=\"__PID_BRAND__\"__PID_THEME__><head>\
             <link href=\"/_app/immutable/start.js\" rel=\"modulepreload\"></head><body><script>{BOOT}</script></body></html>"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("_app/immutable/start.js"), "export const x = 1;").unwrap();
    std::fs::write(dir.join("robots.txt"), "User-agent: *\nDisallow: /\n").unwrap();
    dir
}

async fn page(id: &TestId, path: &str, headers: &[(&str, &str)]) -> (u16, reqwest::header::HeaderMap, String) {
    let mut r = id.http.get(format!("{}{path}", id.public));
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    let r = r.send().await.unwrap();
    (r.status().as_u16(), r.headers().clone(), r.text().await.unwrap())
}

#[tokio::test]
async fn every_page_route_answers_the_page_with_the_realms_look() {
    let dir = build();
    let db = common::fresh_database().await;
    let id = TestId::start_with(&db, |_| String::new(), |b| b.ui_dir = dir.clone()).await;
    let host = format!("localhost:{}", id.running.public.port());

    let (status, h, body) = page(&id, "/account/sessions", &[]).await;
    assert_eq!(status, 200, "a deep link is the page too");
    assert!(body.starts_with("<!doctype html><html lang=\"en\" data-brand=\"gateway\"><head>"), "{body}");
    let csp = h["content-security-policy"].to_str().unwrap();
    let hash = base64::engine::general_purpose::STANDARD.encode(pacod_id::crypto::sha256(BOOT.as_bytes()));
    for part in [
        format!("script-src 'self' 'sha256-{hash}'"),
        "frame-ancestors 'none'".into(),
        "default-src 'none'".into(),
        "base-uri 'none'".into(),
    ] {
        assert!(csp.contains(&part), "{part} in {csp}");
    }
    assert!(!csp.contains("unsafe-eval") && !csp.contains("script-src 'self' 'unsafe-inline'"));
    assert_eq!(
        (h["cache-control"].to_str().unwrap(), h["referrer-policy"].to_str().unwrap()),
        ("no-store", "no-referrer")
    );

    // The language: the person's choice, else the browser's, else the realm's.
    let (_, _, body) = page(&id, "/login", &[("accept-language", "fr-FR,bg;q=0.8,en;q=0.5")]).await;
    assert!(body.contains("lang=\"bg\""), "{body}");
    let (_, _, body) = page(&id, "/login", &[("accept-language", "bg"), ("cookie", "pid_lang=de")]).await;
    assert!(body.contains("lang=\"de\""));
    let (_, _, body) = page(&id, "/login", &[("cookie", "pid_lang=xx; pid_theme=dark")]).await;
    assert!(body.contains("lang=\"en\" data-brand=\"gateway\" data-theme=\"dark\""), "{body}");
    let (_, _, body) = page(&id, "/login", &[("host", &host)]).await;
    assert!(body.contains("data-brand=\"bazaar\""), "Bazaar's own look on its own address");

    // Files of the build; missing files are no page.
    let (status, h, body) = page(&id, "/_app/immutable/start.js", &[]).await;
    assert_eq!((status, body.as_str()), (200, "export const x = 1;"));
    assert!(h["content-type"].to_str().unwrap().starts_with("text/javascript"));
    assert!(h["cache-control"].to_str().unwrap().contains("immutable"));
    assert_eq!(page(&id, "/robots.txt", &[]).await.0, 200);
    assert_eq!(page(&id, "/_app/immutable/gone.js", &[]).await.0, 404);
    assert_eq!(page(&id, "/favicon.ico", &[]).await.0, 404);
    // The API keeps its own answers; other hosts and methods get no page.
    let (status, _, body) = page(&id, "/ui-api/v1/nothing", &[]).await;
    assert_eq!(status, 404);
    assert!(serde_json::from_str::<Value>(&body).is_ok());
    assert_eq!(page(&id, "/admin/v1/accounts", &[]).await.0, 404, "the admin API is internal");
    assert_eq!(page(&id, "/login", &[("host", "elsewhere.example")]).await.0, 404);
    let r = id.http.post(format!("{}/login", id.public)).send().await.unwrap();
    assert_eq!(r.status(), 404);
    id.running.stop().await;
    let _ = std::fs::remove_dir_all(dir);

    // Without a build: 503 for pages, the API as before.
    let db = common::fresh_database().await;
    let empty = std::env::temp_dir().join(format!("pid-ui-none-{}", uuid::Uuid::new_v4().simple()));
    let id = TestId::start_with(&db, |_| String::new(), |b| b.ui_dir = empty).await;
    assert_eq!(page(&id, "/login", &[]).await.0, 503);
    assert_eq!(page(&id, "/.well-known/openid-configuration", &[]).await.0, 200);
    id.running.stop().await;
}

async fn bind(id: &TestId, principal: &str, role: &str, scope: ScopeRef) {
    let st = &id.running.state;
    let mut tx = st.store.begin().await.unwrap();
    pacod_id::store::authz::lock_head(&mut tx).await.unwrap();
    let add = [NewBinding {
        principal: principal.into(),
        role: role.into(),
        scope,
        source: "manual",
        created_by: "test".into(),
    }];
    bindings::apply(&mut tx, &st.catalog.get(), Protect::Skip, &[], &add).await.unwrap();
    tx.commit().await.unwrap();
}

fn link_token(text: &str) -> String {
    let at = text.find("token=").expect("a link") + "token=".len();
    text[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

#[tokio::test]
async fn the_account_pages() {
    let id = TestId::start().await;
    let ada = common::account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    id.sign_in(&mut b, "ada@example.com").await;
    let mut other = Browser::new(&id);
    id.sign_in(&mut other, "ada@example.com").await;
    id.mail().await;
    let api = |p: &str| format!("{}/ui-api/v1/{p}", id.public);

    let a = b.get(&api("realm")).await;
    assert_eq!((a.json["brand"].as_str(), a.json["google"].as_bool()), (Some("gateway"), Some(false)));
    let a = b.get(&api("account")).await;
    assert_eq!(
        (a.json["account"]["email"].as_str(), a.json["session"]["recent"].as_bool()),
        (Some("ada@example.com"), Some(true)),
        "{a:?}"
    );
    assert_eq!(a.json["admin"], json!({ "accounts": false, "service_accounts": false, "audit": false }));
    assert_eq!(Browser::new(&id).get(&api("account")).await.status, 401);

    // Profile; writes only from the realm's own pages.
    let a = b.patch(&api("account"), json!({ "display_name": " Ada L. ", "locale": "bg" })).await;
    assert_eq!((a.status, a.json["display_name"].as_str()), (200, Some("Ada L.")));
    assert_eq!(
        b.patch(&api("account"), json!({ "locale": "fr" })).await.json["error"]["details"]["code"],
        "bad_locale"
    );
    b.origin = "https://evil.example".into();
    assert_eq!(b.patch(&api("account"), json!({ "display_name": "x" })).await.status, 403);
    b.origin = id.public.clone();

    // The address: after a recent sign-in, by a link to the new address.
    id.sql(&format!("UPDATE sso_sessions SET auth_time = now() - interval '1 hour' WHERE account_id = '{}'", ada.id))
        .await;
    let a = b.post(&api("account/email"), json!({ "email": "ada.new@example.com" })).await;
    assert_eq!(a.json["error"]["details"]["code"], "reauth_required");
    assert_eq!(b.post(&api("account/reauth"), json!({ "password": "wrong" })).await.status, 401);
    assert_eq!(b.post(&api("account/reauth"), json!({ "password": common::PASSWORD })).await.status, 204);
    let a = b.post(&api("account/email"), json!({ "email": "Ada.New@Example.com" })).await;
    assert_eq!((a.status, a.json["email"].as_str()), (202, Some("ada.new@example.com")), "{a:?}");
    let sent = id.mail().await;
    assert_eq!((sent[0].template.as_str(), sent[0].to.as_str()), ("verify_email", "ada.new@example.com"));
    let a =
        Browser::new(&id).post(&api("verification/confirm"), json!({ "token": link_token(&sent[0].mail.text) })).await;
    assert_eq!((a.status, a.json["changed"].as_bool()), (200, Some(true)), "{a:?}");
    let now = id.store().account(&ada.id).await.unwrap().unwrap();
    assert_eq!((now.email.as_str(), now.is_verified()), ("ada.new@example.com", true));
    assert!(
        id.mail().await.iter().any(|m| m.template == "email_changed" && m.to == "ada@example.com"),
        "the old address hears of it"
    );

    // Sessions: both listed; the password change ends the other one and keeps this one.
    let a = b.get(&api("account/sessions")).await;
    let list = a.json["sessions"].as_array().unwrap();
    assert_eq!((list.len(), list.iter().filter(|s| s["current"] == true).count()), (2, 1));
    assert_eq!(
        b.post(&api("account/password"), json!({ "current": "wrong", "password": "a brand new passphrase" }))
            .await
            .status,
        401
    );
    let a = b
        .post(&api("account/password"), json!({ "current": common::PASSWORD, "password": "a brand new passphrase" }))
        .await;
    assert_eq!((a.status, a.json["other_sessions_ended"].as_i64()), (200, Some(1)), "{a:?}");
    assert_eq!(b.get(&api("account")).await.status, 200);
    assert_eq!(other.get(&api("account")).await.status, 401);
    let mut third = Browser::new(&id);
    let (_, ch) = common::pkce();
    let f = common::param(&third.get(&common::authorize_url(&id, &ch, &[])).await.location.unwrap(), "flow").unwrap();
    third
        .post(
            &format!("{}/ui-api/v1/flows/{f}/password", id.public),
            json!({ "email": "ada.new@example.com", "password": "a brand new passphrase" }),
        )
        .await;
    let theirs = b.get(&api("account/sessions")).await.json["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["current"] == false)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(b.delete_json(&api(&format!("account/sessions/{theirs}"))).await.status, 204);
    assert_eq!(third.get(&api("account")).await.status, 401);
    assert_eq!(b.delete_json(&api("account/sessions/sid_nope")).await.status, 404);

    // Roles, named.
    id.service_secret("gw-core", "a-secret-for-the-gateway-core-0123456789").await;
    let gw = std::fs::read_to_string(format!("{}/tests/fixtures/gw.yaml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let gw = serde_json::to_value(pacod_id_authz::Manifest::from_yaml(&gw).unwrap().source()).unwrap();
    let put = |path: &'static str, body: Value| {
        id.internal_call(reqwest::Method::PUT, path, "gw-core", "a-secret-for-the-gateway-core-0123456789", Some(body))
    };
    put("/internal/v1/catalog/gw", json!({ "manifest": gw })).await;
    put("/internal/v1/organisations/org_1", json!({ "slug": "acme", "display_name": "Acme" })).await;
    put("/internal/v1/scopes/gw.workspace/ws_1", json!({ "parent": "organisation:org_1", "display_name": "Prod" }))
        .await;
    bind(&id, &ada.id, "gw.workspace_admin", ScopeRef::of("gw.workspace", "ws_1")).await;
    let a = b.get(&api("account/organisations")).await;
    let names: Vec<&str> =
        a.json["roles"][0]["path"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["", "Acme", "Prod"], "{a:?}");

    // Deleting the account: after a recent sign-in.
    id.sql(&format!("UPDATE sso_sessions SET auth_time = now() - interval '1 hour' WHERE account_id = '{}'", ada.id))
        .await;
    assert_eq!(b.delete_json(&api("account")).await.json["error"]["details"]["code"], "reauth_required");
    b.post(&api("account/reauth"), json!({ "password": "a brand new passphrase" })).await;
    let a = b.delete_json(&api("account")).await;
    assert_eq!(a.status, 200, "{a:?}");
    assert!(!b.cookies.contains_key("pid_sid"));
    assert!(id.store().account(&ada.id).await.unwrap().is_none());
    id.running.stop().await;
}

#[tokio::test]
async fn address_changes_hide_taken_addresses_and_end_with_a_new_password() {
    let id = TestId::start().await;
    let ada = common::account(&id, "ada@example.com").await;
    common::account(&id, "bob@example.com").await;
    let mut b = Browser::new(&id);
    id.sign_in(&mut b, "ada@example.com").await;
    id.mail().await;
    let api = |p: &str| format!("{}/ui-api/v1/{p}", id.public);

    // Another account's address: the same answer, and no link.
    let a = b.post(&api("account/email"), json!({ "email": "bob@example.com" })).await;
    assert_eq!(a.status, 202, "{a:?}");
    assert!(id.mail().await.is_empty(), "no link for a taken address");

    // A link is pending when the password changes: it no longer works.
    assert_eq!(b.post(&api("account/email"), json!({ "email": "ada.new@example.com" })).await.status, 202);
    let sent = id.mail().await;
    let token = link_token(&sent[0].mail.text);
    let a = b
        .post(&api("account/password"), json!({ "current": common::PASSWORD, "password": "a brand new passphrase" }))
        .await;
    assert_eq!(a.status, 200, "{a:?}");
    let a = Browser::new(&id).post(&api("verification/confirm"), json!({ "token": token })).await;
    assert_eq!(a.status, 410, "{a:?}");
    assert_eq!(id.store().account(&ada.id).await.unwrap().unwrap().email, "ada@example.com");
    id.running.stop().await;
}

#[tokio::test]
async fn the_admin_pages_act_by_cookie_only() {
    let id = TestId::start().await;
    id.service_secret("gw-core", "a-secret-for-the-gateway-core-0123456789").await;
    let gw = std::fs::read_to_string(format!("{}/tests/fixtures/gw.yaml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let gw = serde_json::to_value(pacod_id_authz::Manifest::from_yaml(&gw).unwrap().source()).unwrap();
    id.internal_call(
        reqwest::Method::PUT,
        "/internal/v1/catalog/gw",
        "gw-core",
        "a-secret-for-the-gateway-core-0123456789",
        Some(json!({ "manifest": gw })),
    )
    .await;
    let root = common::account(&id, "root@example.com").await;
    bind(&id, &root.id, "gw.platform_admin", ScopeRef::platform()).await;
    let mut b = Browser::new(&id);
    id.sign_in(&mut b, "root@example.com").await;
    let admin = |p: &str| format!("{}/ui-api/v1/admin/{p}", id.public);

    let a = b.get(&admin("accounts")).await;
    assert_eq!(a.status, 200, "{a:?}");
    let a = b.get(&format!("{}/ui-api/v1/account", id.public)).await;
    assert_eq!(a.json["admin"]["accounts"], true, "the realm policy");
    let a = b
        .post(&admin("accounts"), json!({ "email": "eve@example.com", "password": "a long passphrase for eve" }))
        .await;
    assert_eq!(a.status, 201, "{a:?}");
    b.origin = "https://evil.example".into();
    let a = b.post(&admin("accounts"), json!({ "email": "mallory@example.com" })).await;
    assert_eq!((a.status, a.json["error"]["details"]["code"].as_str()), (403, Some("cross_origin")));
    // A bearer token counts for nothing here.
    let (code, verifier) = id.sign_in(&mut Browser::new(&id), "root@example.com").await;
    let (_, tokens) = id.redeem(&code, &verifier).await;
    let r = id.http.get(admin("accounts")).bearer_auth(tokens["access_token"].as_str().unwrap()).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let (s, _) = id
        .internal_call(
            reqwest::Method::GET,
            "/admin/v1/audit",
            "gw-core",
            "a-secret-for-the-gateway-core-0123456789",
            None,
        )
        .await;
    assert_eq!(s, 401, "a service's own credentials are no actor either");
    id.running.stop().await;
}

#[tokio::test]
async fn the_realms_own_pages_sign_in_without_a_client() {
    let id = TestId::start().await;
    common::account(&id, "ada@example.com").await;
    let mut b = Browser::new(&id);
    let start = format!("{}/ui-api/v1/account/sign-in", id.public);
    for bad in
        ["https://evil.example/", "//evil.example/x", "/ui-api/v1/account", "account", "/\t/evil.example", "/account\n"]
    {
        let a = b.post(&start, json!({ "return_to": bad })).await;
        assert_eq!(a.json["error"]["details"]["code"], "bad_return_to", "{bad}");
    }
    let a = b.post(&start, json!({ "return_to": "/account/sessions?x=1" })).await;
    let to = a.json["to"].as_str().unwrap().to_owned();
    assert!(to.starts_with(&format!("{}/login?flow=", id.public)), "{to}");
    let flow = common::param(&to, "flow").unwrap();
    let f = b.get(&format!("{}/ui-api/v1/flows/{flow}", id.public)).await;
    assert_eq!((f.json["client"].as_str(), f.json["self_signup"].as_bool()), (Some(""), Some(false)), "{f:?}");
    let a = b
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
            json!({ "email": "ada@example.com", "password": common::PASSWORD }),
        )
        .await;
    assert_eq!(
        (a.json["next"].as_str(), a.json["to"].as_str()),
        (Some("redirect"), Some(format!("{}/account/sessions?x=1", id.public).as_str())),
        "{a:?}"
    );
    assert_eq!(b.get(&format!("{}/ui-api/v1/account", id.public)).await.status, 200, "signed in to pacod-id itself");
    // Sign-up keeps the realm's own rule.
    let a = b.post(&start, json!({ "create": true })).await;
    let flow = common::param(a.json["to"].as_str().unwrap(), "flow").unwrap();
    assert!(a.json["to"].as_str().unwrap().contains("/signup?flow="));
    let a = b
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/signup", id.public),
            json!({ "email": "eve@example.com", "password": common::PASSWORD }),
        )
        .await;
    assert_eq!(a.json["error"]["details"]["code"], "signup_closed");
    id.running.stop().await;
}
