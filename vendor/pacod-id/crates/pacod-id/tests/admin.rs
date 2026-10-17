//! Stage 6: the admin API, as the gateway's facades will call it, and the hosted invitation pages.

mod common;

use common::{Browser, TestId};
use pacod_id::authz::bindings::{self, NewBinding, Protect};
use pacod_id_authz::{Manifest, ScopeRef};
use reqwest::Method;
use serde_json::{Value, json};

const CORE: &str = "a-secret-for-the-gateway-core-0123456789";

fn manifest(name: &str) -> Value {
    let text = std::fs::read_to_string(format!("{}/tests/fixtures/{name}.yaml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    serde_json::to_value(Manifest::from_yaml(&text).unwrap().source()).unwrap()
}

/// pacod-id with gw's catalogue, organisations org_1 (workspaces ws_1, ws_2) and org_2.
async fn world() -> TestId {
    let id = TestId::start().await;
    id.service_secret("gw-core", CORE).await;
    let put = |path: &'static str, body: Value| id.internal_call(Method::PUT, path, "gw-core", CORE, Some(body));
    assert_eq!(put("/internal/v1/catalog/gw", json!({ "manifest": manifest("gw") })).await.0, 200);
    put("/internal/v1/organisations/org_1", json!({ "slug": "acme", "display_name": "Acme" })).await;
    put("/internal/v1/organisations/org_2", json!({ "slug": "other", "display_name": "Other" })).await;
    put("/internal/v1/scopes/gw.workspace/ws_1", json!({ "parent": "organisation:org_1", "display_name": "Prod" }))
        .await;
    put("/internal/v1/scopes/gw.workspace/ws_2", json!({ "parent": "organisation:org_1", "display_name": "Test" }))
        .await;
    id
}

/// An account with `role` at `scope` (none when `role` is empty); its id.
async fn person(id: &TestId, email: &str, role: &str, scope: ScopeRef) -> String {
    let a = common::account(id, email).await;
    if !role.is_empty() {
        let st = &id.running.state;
        let mut tx = st.store.begin().await.unwrap();
        pacod_id::store::authz::lock_head(&mut tx).await.unwrap();
        let add = [NewBinding {
            principal: a.id.clone(),
            role: role.into(),
            scope,
            source: "manual",
            created_by: "test".into(),
        }];
        bindings::apply(&mut tx, &st.catalog.get(), Protect::Skip, &[], &add).await.unwrap();
        tx.commit().await.unwrap();
    }
    a.id
}

/// What the gateway's core does for a facade: swaps its caller's token for one for pacod-id.
async fn exchange(id: &TestId, subject: &str) -> String {
    let r = id
        .http
        .post(format!("{}/oidc/token", id.internal))
        .basic_auth("gw-core", Some(CORE))
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
            ("subject_token", subject),
            ("subject_token_type", "urn:ietf:params:oauth:token-type:access_token"),
        ])
        .send()
        .await
        .unwrap();
    let v: Value = r.json().await.unwrap();
    v["access_token"].as_str().unwrap_or_else(|| panic!("{v}")).to_owned()
}

/// A signed-in person's facade token, and their browser.
async fn as_person(id: &TestId, email: &str) -> (String, Browser) {
    let mut b = Browser::new(id);
    let (code, verifier) = id.sign_in(&mut b, email).await;
    let (_, t) = id.redeem(&code, &verifier).await;
    (exchange(id, t["access_token"].as_str().unwrap()).await, b)
}

async fn call(id: &TestId, token: &str, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut r = id.http.request(method, format!("{}{path}", id.internal)).bearer_auth(token);
    if let Some(b) = body {
        r = r.json(&b);
    }
    let r = r.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

fn code(v: &Value) -> Option<&str> {
    v["error"]["details"]["code"].as_str()
}

#[tokio::test]
async fn the_admin_api_takes_exchanged_tokens_only() {
    let id = world().await;
    person(&id, "ada@example.com", "gw.org_admin", ScopeRef::organisation("org_1")).await;
    let mut b = Browser::new(&id);
    let (c, v) = id.sign_in(&mut b, "ada@example.com").await;
    let (_, tokens) = id.redeem(&c, &v).await;
    let own = tokens["access_token"].as_str().unwrap();
    let r = id
        .http
        .post(format!("{}/oidc/token", id.internal))
        .basic_auth("gw-core", Some(CORE))
        .form(&[("grant_type", "client_credentials")])
        .send()
        .await
        .unwrap();
    let service_own = r.json::<Value>().await.unwrap()["access_token"].as_str().unwrap().to_owned();
    for token in ["", own, &service_own, tokens["id_token"].as_str().unwrap()] {
        let (s, v) = call(&id, token, Method::GET, "/admin/v1/bindings", None).await;
        assert_eq!((s, code(&v)), (401, Some("invalid_token")), "{token:.20}");
    }
    let facade = exchange(&id, own).await;
    let (s, v) = call(&id, &facade, Method::GET, "/admin/v1/bindings", None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["bindings"][0]["role"], "gw.org_admin");
    assert_eq!(v["bindings"][0]["path"], json!(["platform", "organisation:org_1"]));
    let r = id.http.get(format!("{}/admin/v1/bindings", id.public)).bearer_auth(&facade).send().await.unwrap();
    assert_eq!(r.status(), 404, "internal only");
    // Signed out: the facade token stops with the session.
    pacod_id::oidc::sign_out_everywhere(id.store(), &common_id(&id, "ada@example.com").await, "test").await.unwrap();
    let (s, _) = call(&id, &facade, Method::GET, "/admin/v1/bindings", None).await;
    assert_eq!(s, 401);
    id.running.stop().await;
}

async fn common_id(id: &TestId, email: &str) -> String {
    id.store().account_by_email(email).await.unwrap().unwrap().id
}

#[tokio::test]
async fn members_follow_the_gateways_rules() {
    let id = world().await;
    let root = person(&id, "root@example.com", "gw.platform_admin", ScopeRef::platform()).await;
    let ada = person(&id, "ada@example.com", "gw.org_admin", ScopeRef::organisation("org_1")).await;
    let bob = person(&id, "bob@example.com", "", ScopeRef::platform()).await;
    person(&id, "carol@example.com", "gw.workspace_admin", ScopeRef::of("gw.workspace", "ws_1")).await;
    let (ada_t, _) = as_person(&id, "ada@example.com").await;
    let (carol_t, _) = as_person(&id, "carol@example.com").await;
    let (root_t, _) = as_person(&id, "root@example.com").await;

    let put = |t: &str, path: &str, role: Value| {
        let (t, path) = (t.to_owned(), path.to_owned());
        let id = &id;
        async move { call(id, &t, Method::PUT, &path, Some(json!({ "role": role }))).await }
    };
    let (s, v) = put(&ada_t, &format!("/admin/v1/scopes/organisation/org_1/members/{bob}"), json!("gw.viewer")).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["seq"].as_i64().is_some(), "writes answer with the feed's seq");
    let (s, v) = put(&ada_t, &format!("/admin/v1/scopes/organisation/org_1/members/{bob}"), json!("gw.auditor")).await;
    assert_eq!(s, 200, "{v}");
    // Workspace admins do not manage members (the gateway's rule); nobody gives platform roles here.
    let (s, v) = put(&carol_t, &format!("/admin/v1/scopes/gw.workspace/ws_1/members/{bob}"), json!("gw.viewer")).await;
    assert_eq!((s, code(&v)), (403, Some("not_allowed")));
    // Nor does the "nothing to change" answer tell them who holds what.
    let (s, v) =
        put(&carol_t, &format!("/admin/v1/scopes/organisation/org_1/members/{bob}"), json!("gw.auditor")).await;
    assert_eq!((s, code(&v)), (403, Some("not_allowed")), "bob is an auditor there, but carol may not know");
    let (s, v) =
        put(&ada_t, &format!("/admin/v1/scopes/organisation/org_1/members/{bob}"), json!("gw.platform_admin")).await;
    assert_eq!((s, code(&v)), (422, Some("scope_type_not_allowed")));
    let (s, v) = put(&ada_t, &format!("/admin/v1/scopes/organisation/org_2/members/{bob}"), json!("gw.viewer")).await;
    assert_eq!((s, code(&v)), (403, Some("not_allowed")), "another organisation");
    let (s, v) =
        put(&ada_t, &format!("/admin/v1/scopes/gw.workspace/ws_1/members/{bob}"), json!("gw.workspace_admin")).await;
    assert_eq!(s, 200, "{v}");

    // The members of the organisation, and of a workspace with the organisation's roles inherited.
    let (s, v) = call(&id, &ada_t, Method::GET, "/admin/v1/scopes/organisation/org_1/members", None).await;
    assert_eq!(s, 200, "{v}");
    let roles_of = |v: &Value, who: &str| -> Vec<(String, bool)> {
        v["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["principal"] == who)
            .map(|m| {
                m["roles"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| (r["role"].as_str().unwrap().to_owned(), r["inherited"].as_bool().unwrap()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut bobs = roles_of(&v, &bob);
    bobs.sort();
    assert_eq!(
        bobs,
        [("gw.auditor".to_owned(), false), ("gw.workspace_admin".to_owned(), false)],
        "auditor replaced viewer"
    );
    assert!(!v["members"].as_array().unwrap().iter().any(|m| m["principal"] == root), "platform roles are not members");
    let (_, v) = call(&id, &carol_t, Method::GET, "/admin/v1/scopes/gw.workspace/ws_1/members", None).await;
    assert!(roles_of(&v, &ada).contains(&("gw.org_admin".to_owned(), true)), "{v}");
    let (s, _) = call(&id, &carol_t, Method::GET, "/admin/v1/scopes/organisation/org_1/members", None).await;
    assert_eq!(s, 403, "a workspace role does not see the organisation's members");

    // The last organisation admin stays, unless a platform admin lets them go.
    let (s, v) =
        call(&id, &ada_t, Method::DELETE, &format!("/admin/v1/scopes/organisation/org_1/members/{ada}"), None).await;
    assert_eq!((s, code(&v)), (409, Some("last_holder")), "{v}");
    let (_, bs) = call(&id, &ada_t, Method::GET, "/admin/v1/bindings", None).await;
    let ada_binding = bs["bindings"][0]["id"].as_str().unwrap().to_owned();
    let (s, v) = call(&id, &ada_t, Method::DELETE, &format!("/admin/v1/bindings/{ada_binding}"), None).await;
    assert_eq!((s, code(&v)), (409, Some("last_holder")), "the bindings endpoint keeps the rule too");

    // Leaving an organisation leaves its workspaces; `below=false` only the scope itself.
    let (s, _) = call(
        &id,
        &ada_t,
        Method::DELETE,
        &format!("/admin/v1/scopes/organisation/org_1/members/{bob}?below=false"),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let (_, v) = call(&id, &ada_t, Method::GET, "/admin/v1/scopes/organisation/org_1/members", None).await;
    assert_eq!(roles_of(&v, &bob), [("gw.workspace_admin".to_owned(), false)]);
    let (s, _) =
        call(&id, &ada_t, Method::DELETE, &format!("/admin/v1/scopes/organisation/org_1/members/{bob}"), None).await;
    assert_eq!(s, 200);
    let (s, v) =
        call(&id, &ada_t, Method::DELETE, &format!("/admin/v1/scopes/organisation/org_1/members/{bob}"), None).await;
    assert_eq!((s, code(&v)), (404, Some("not_a_member")));

    // Additive bindings; the same one twice is refused.
    let body = json!({ "principal": bob, "role": "gw.viewer", "scope": "gw.workspace:ws_2" });
    let (s, v) = call(&id, &ada_t, Method::POST, "/admin/v1/bindings", Some(body.clone())).await;
    assert_eq!((s, v["path"].clone()), (201, json!(["platform", "organisation:org_1", "gw.workspace:ws_2"])), "{v}");
    let (s, v) = call(&id, &ada_t, Method::POST, "/admin/v1/bindings", Some(body)).await;
    assert_eq!((s, code(&v)), (409, Some("binding_exists")));
    let (s, v) = call(
        &id,
        &ada_t,
        Method::POST,
        "/admin/v1/bindings",
        Some(json!({ "principal": bob, "role": "gw.viewer", "scope": "gw.workspace:ws_9" })),
    )
    .await;
    assert_eq!((s, code(&v)), (409, Some("scope_unknown")));
    let (s, v) = call(
        &id,
        &ada_t,
        Method::POST,
        "/admin/v1/bindings",
        Some(json!({ "principal": "usr_nobody", "role": "gw.viewer", "scope": "organisation:org_1" })),
    )
    .await;
    assert_eq!((s, code(&v)), (404, Some("principal_unknown")));

    // A platform admin lets the last organisation admin go.
    let (s, v) = call(&id, &root_t, Method::DELETE, &format!("/admin/v1/bindings/{ada_binding}"), None).await;
    assert_eq!(s, 200, "{v}");
    id.running.stop().await;
}

#[tokio::test]
async fn accounts_under_the_realm_policy() {
    let id = world().await;
    let root = person(&id, "root@example.com", "gw.platform_admin", ScopeRef::platform()).await;
    person(&id, "viewer@example.com", "gw.viewer", ScopeRef::organisation("org_1")).await;
    // A Bazaar sign-up: not the gateway's to manage.
    let nia = common::account_with(&id, "nia@example.com", common::PASSWORD, true).await;
    id.sql(&format!("UPDATE accounts SET signup_realm = 'bazaar' WHERE id = '{}'", nia.id)).await;
    let (root_t, _) = as_person(&id, "root@example.com").await;
    let (viewer_t, _) = as_person(&id, "viewer@example.com").await;

    let (s, _) = call(&id, &viewer_t, Method::GET, "/admin/v1/accounts", None).await;
    assert_eq!(s, 403);
    let (s, v) = call(&id, &root_t, Method::GET, "/admin/v1/accounts", None).await;
    assert_eq!(s, 200, "{v}");
    let emails: Vec<&str> = v["accounts"].as_array().unwrap().iter().map(|a| a["email"].as_str().unwrap()).collect();
    assert_eq!(emails, ["root@example.com", "viewer@example.com"]);
    for (method, body) in
        [(Method::GET, None), (Method::PATCH, Some(json!({ "status": "disabled" }))), (Method::DELETE, None)]
    {
        let (s, _) = call(&id, &root_t, method, &format!("/admin/v1/accounts/{}", nia.id), body).await;
        assert_eq!(s, 404, "a Bazaar account is out of sight");
    }
    // An identity admin is out of reach too: the policy reaches only whom the actor could make.
    let idadmin = person(&id, "idadmin@example.com", "id.admin", ScopeRef::platform()).await;
    for (method, body) in [
        (Method::PATCH, Some(json!({ "password": "the platform admin's choice" }))),
        (Method::PATCH, Some(json!({ "email": "mine@example.com" }))),
        (Method::DELETE, None),
    ] {
        let (s, v) = call(&id, &root_t, method, &format!("/admin/v1/accounts/{idadmin}"), body).await;
        assert_eq!((s, code(&v)), (403, Some("holds_more_rights")));
    }
    id.sql(&format!("DELETE FROM role_bindings WHERE principal_id = '{idadmin}'")).await;
    id.sql(&format!("DELETE FROM accounts WHERE id = '{idadmin}'")).await;

    // Create: the address is confirmed by mail here.
    let (s, v) = call(
        &id,
        &root_t,
        Method::POST,
        "/admin/v1/accounts",
        Some(json!({ "email": "Eve@Example.com", "display_name": "Eve", "password": "a long passphrase for eve" })),
    )
    .await;
    assert_eq!(
        (s, v["email"].as_str(), v["email_verified"].as_bool(), v["signup_realm"].as_str()),
        (201, Some("eve@example.com"), Some(false), Some("gateway")),
        "{v}"
    );
    let eve = v["id"].as_str().unwrap().to_owned();
    assert_eq!(id.mail().await.iter().map(|m| m.template.as_str()).collect::<Vec<_>>(), ["verify_email"]);
    let (s, v) =
        call(&id, &root_t, Method::POST, "/admin/v1/accounts", Some(json!({ "email": "eve@example.com" }))).await;
    assert_eq!((s, code(&v)), (409, Some("email_taken")));
    let (s, _) = call(&id, &root_t, Method::POST, &format!("/admin/v1/accounts/{eve}/email-confirmation"), None).await;
    assert_eq!(s, 204);
    let (s, v) = call(&id, &root_t, Method::POST, &format!("/admin/v1/accounts/{eve}/verification"), None).await;
    assert_eq!((s, code(&v)), (409, Some("already_verified")));
    let (s, _) = call(&id, &root_t, Method::POST, &format!("/admin/v1/accounts/{eve}/password-reset"), None).await;
    assert_eq!(s, 202);
    assert_eq!(id.mail().await.iter().map(|m| m.template.as_str()).collect::<Vec<_>>(), ["reset_password"]);

    // A refused field leaves the account as it was; a new name reaches the feed.
    let (s, v) = call(
        &id,
        &root_t,
        Method::PATCH,
        &format!("/admin/v1/accounts/{eve}"),
        Some(json!({ "status": "disabled", "locale": "xx" })),
    )
    .await;
    assert_eq!((s, code(&v)), (422, Some("bad_locale")));
    assert_eq!(id.store().account(&eve).await.unwrap().unwrap().status, "active", "nothing was saved");
    let (s, v) = call(
        &id,
        &root_t,
        Method::PATCH,
        &format!("/admin/v1/accounts/{eve}"),
        Some(json!({ "display_name": "Eve Smith" })),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let kind: String = sqlx::query_scalar("SELECT kind FROM authz_changes WHERE version = $1")
        .bind(v["seq"].as_i64().unwrap())
        .fetch_one(id.store().pool())
        .await
        .unwrap();
    assert_eq!(kind, "account.updated");

    // Disabling ends the sessions and tells the feed; not for the last platform admin.
    let mut b = Browser::new(&id);
    id.sign_in(&mut b, "viewer@example.com").await;
    let viewer = common_id(&id, "viewer@example.com").await;
    let (s, v) = call(
        &id,
        &root_t,
        Method::PATCH,
        &format!("/admin/v1/accounts/{viewer}"),
        Some(json!({ "status": "disabled" })),
    )
    .await;
    assert_eq!((s, v["status"].as_str()), (200, Some("disabled")), "{v}");
    assert!(id.store().sessions_of(&viewer).await.unwrap().is_empty());
    let kind: String = sqlx::query_scalar("SELECT kind FROM authz_changes WHERE version = $1")
        .bind(v["seq"].as_i64().unwrap())
        .fetch_one(id.store().pool())
        .await
        .unwrap();
    assert_eq!(kind, "principal.disabled");
    let (s, v) =
        call(&id, &root_t, Method::PATCH, &format!("/admin/v1/accounts/{root}"), Some(json!({ "status": "disabled" })))
            .await;
    assert_eq!((s, code(&v)), (409, Some("last_holder")));
    // Another address: proven again by mail; the old address hears of it.
    let (s, v) = call(
        &id,
        &root_t,
        Method::PATCH,
        &format!("/admin/v1/accounts/{eve}"),
        Some(json!({ "email": "eve2@example.com", "display_name": "Eve Two" })),
    )
    .await;
    assert_eq!(
        (s, v["email_verified"].as_bool(), v["display_name"].as_str()),
        (200, Some(false), Some("Eve Two")),
        "{v}"
    );
    let sent = id.mail().await;
    assert!(sent.iter().any(|m| m.template == "email_changed" && m.to == "eve@example.com"));
    assert!(sent.iter().any(|m| m.template == "verify_email" && m.to == "eve2@example.com"));

    // An identity admin sees every account.
    person(&id, "idadmin@example.com", "id.admin", ScopeRef::platform()).await;
    let (ida_t, _) = as_person(&id, "idadmin@example.com").await;
    let (s, _) = call(&id, &ida_t, Method::GET, &format!("/admin/v1/accounts/{}", nia.id), None).await;
    assert_eq!(s, 200);
    id.running.stop().await;
}

#[tokio::test]
async fn service_accounts_and_their_tokens() {
    let id = world().await;
    person(&id, "root@example.com", "gw.platform_admin", ScopeRef::platform()).await;
    person(&id, "ada@example.com", "gw.org_admin", ScopeRef::organisation("org_1")).await;
    let (root_t, _) = as_person(&id, "root@example.com").await;
    let (ada_t, _) = as_person(&id, "ada@example.com").await;

    let (s, v) = call(&id, &ada_t, Method::POST, "/admin/v1/service-accounts", Some(json!({ "name": "ci" }))).await;
    assert_eq!(s, 403, "only platform admins make platform service accounts: {v}");
    let (s, v) = call(
        &id,
        &ada_t,
        Method::POST,
        "/admin/v1/service-accounts",
        Some(json!({ "name": "acme ci", "organisation_id": "org_1" })),
    )
    .await;
    assert_eq!(s, 201, "an organisation's admins make its own: {v}");
    let (s, v) =
        call(&id, &root_t, Method::POST, "/admin/v1/service-accounts", Some(json!({ "name": "deploy bot" }))).await;
    assert_eq!(s, 201, "{v}");
    let svc = v["id"].as_str().unwrap().to_owned();
    let (s, v) = call(
        &id,
        &root_t,
        Method::POST,
        &format!("/admin/v1/service-accounts/{svc}/tokens"),
        Some(json!({ "expires_at": "2001-01-01T00:00:00Z" })),
    )
    .await;
    assert_eq!((s, code(&v)), (422, Some("bad_expiry")));
    let (s, v) =
        call(&id, &root_t, Method::POST, &format!("/admin/v1/service-accounts/{svc}/tokens"), Some(json!({}))).await;
    assert_eq!(s, 201, "{v}");
    let secret = v["token"].as_str().unwrap().to_owned();
    let token_id = v["id"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("pidt_") && secret.ends_with(v["hint"].as_str().unwrap()));
    let (_, v) = call(&id, &root_t, Method::GET, &format!("/admin/v1/service-accounts/{svc}/tokens"), None).await;
    assert!(!v.to_string().contains(&secret), "listed without the secret");

    // A script with the token acts through the gateway with its own bindings.
    let (s, _) = call(
        &id,
        &root_t,
        Method::POST,
        "/admin/v1/bindings",
        Some(json!({ "principal": svc, "role": "gw.viewer", "scope": "organisation:org_1" })),
    )
    .await;
    assert_eq!(s, 201);
    let bot_t = exchange(&id, &secret).await;
    let (s, v) = call(&id, &bot_t, Method::GET, "/admin/v1/scopes/organisation/org_1/members", None).await;
    assert_eq!(s, 200, "{v}");
    let (s, _) = call(&id, &bot_t, Method::GET, "/admin/v1/scopes/organisation/org_2/members", None).await;
    assert_eq!(s, 403);

    // Revoking the token, then deleting the account, reach the feed.
    let (s, v) = call(&id, &root_t, Method::DELETE, &format!("/admin/v1/service-tokens/{token_id}"), None).await;
    assert_eq!(s, 200, "{v}");
    let r = id
        .http
        .post(format!("{}/internal/v1/introspect", id.internal))
        .basic_auth("gw-core", Some(CORE))
        .form(&[("token", &secret)])
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["active"], false);
    let (s, v) = call(&id, &root_t, Method::DELETE, &format!("/admin/v1/service-accounts/{svc}"), None).await;
    assert_eq!(s, 200, "{v}");
    // The product's own client-credentials token stays active.
    let own: Value = id
        .http
        .post(format!("{}/oidc/token", id.internal))
        .basic_auth("gw-core", Some(CORE))
        .form(&[("grant_type", "client_credentials")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r = id
        .http
        .post(format!("{}/internal/v1/introspect", id.internal))
        .basic_auth("gw-core", Some(CORE))
        .form(&[("token", own["access_token"].as_str().unwrap())])
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["active"], true);
    // Its exchanged access token, still unexpired, is dead with it.
    let r = id
        .http
        .post(format!("{}/internal/v1/introspect", id.internal))
        .basic_auth("gw-core", Some(CORE))
        .form(&[("token", &bot_t)])
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["active"], false);
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM authz_changes WHERE principal_id = $1 ORDER BY version")
            .bind(&svc)
            .fetch_all(id.store().pool())
            .await
            .unwrap();
    assert_eq!(kinds, ["binding.added", "credential.revoked", "binding.removed", "principal.deleted"]);
    id.running.stop().await;
}

/// The token in a mail's link.
fn link_token(text: &str) -> String {
    let at = text.find("token=").expect("a link") + "token=".len();
    text[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

#[tokio::test]
async fn invitations_from_sending_to_accepting() {
    let id = world().await;
    person(&id, "ada@example.com", "gw.org_admin", ScopeRef::organisation("org_1")).await;
    let bob = person(&id, "bob@example.com", "gw.viewer", ScopeRef::organisation("org_1")).await;
    person(&id, "carol@example.com", "gw.workspace_admin", ScopeRef::of("gw.workspace", "ws_1")).await;
    let (ada_t, _) = as_person(&id, "ada@example.com").await;
    let (carol_t, _) = as_person(&id, "carol@example.com").await;
    let invite = |t: &str, body: Value| {
        let t = t.to_owned();
        let id = &id;
        async move { call(id, &t, Method::POST, "/admin/v1/invitations", Some(body)).await }
    };

    let (s, v) =
        invite(&carol_t, json!({ "email": "nia@example.com", "role": "gw.viewer", "scope": "gw.workspace:ws_1" }))
            .await;
    assert_eq!(s, 403, "workspace admins do not invite: {v}");
    let (s, v) = invite(&ada_t, json!({ "email": "Nia@Example.com", "role": "gw.viewer", "scope": "organisation:org_1", "labels": { "role": "Viewer" } })).await;
    assert_eq!((s, v["emailed"].as_bool(), v.get("token").is_none()), (201, Some(true), true), "{v}");
    let first = link_token(&id.mail().await[0].mail.text);
    // The same address and scope again: one invitation, a new link, the old one dead.
    let (s, v) =
        invite(&ada_t, json!({ "email": "nia@example.com", "role": "gw.auditor", "scope": "organisation:org_1" }))
            .await;
    assert_eq!(s, 201, "{v}");
    let sent = id.mail().await;
    assert_eq!(sent[0].template, "invitation");
    assert!(sent[0].mail.text.contains("Acme"), "the scope's name: {}", sent[0].mail.text);
    let token = link_token(&sent[0].mail.text);
    let (_, list) = call(&id, &ada_t, Method::GET, "/admin/v1/invitations?scope=organisation:org_1", None).await;
    assert_eq!(list["invitations"].as_array().unwrap().len(), 1);
    assert_eq!(list["invitations"][0]["inviter_name"], "Ada Lovelace", "named for those who cannot read accounts");

    let mut b = Browser::new(&id);
    let lookup = format!("{}/ui-api/v1/invitations/lookup", id.public);
    assert_eq!(b.post(&lookup, json!({ "token": first })).await.status, 410);
    let a = b.post(&lookup, json!({ "token": token })).await;
    assert_eq!(a.status, 200, "{a:?}");
    assert_eq!(
        (
            a.json["email"].as_str(),
            a.json["role"].as_str(),
            a.json["scope_label"].as_str(),
            a.json["account_exists"].as_bool(),
            a.json["inviter_name"].as_str()
        ),
        (Some("nia@example.com"), Some("gw.auditor"), Some("Acme"), Some(false), Some("Ada Lovelace"))
    );
    let accept = format!("{}/ui-api/v1/invitations/accept", id.public);
    let a = b.post(&accept, json!({ "token": token, "password": "ninechars" })).await;
    assert_eq!(
        (a.status, a.json["error"]["details"]["min_length"].as_i64()),
        (422, Some(12)),
        "a role holder's minimum"
    );
    let a = b
        .post(&accept, json!({ "token": token, "password": "a long passphrase for nia", "display_name": "Nia" }))
        .await;
    assert_eq!(
        (a.status, a.json["next"].as_str(), a.json["needs_confirmation"].as_bool()),
        (200, Some("sign_in"), Some(false)),
        "{a:?}"
    );
    let nia = id.store().account_by_email("nia@example.com").await.unwrap().unwrap();
    assert_eq!(
        (nia.email_verified_source.as_str(), nia.signup_realm.as_str()),
        ("invitation", "gateway"),
        "a mailed link proves the address"
    );
    assert_eq!(b.post(&accept, json!({ "token": token, "password": "a long passphrase for nia" })).await.status, 410);
    let (_, list) = call(&id, &ada_t, Method::GET, "/admin/v1/invitations?scope=organisation:org_1", None).await;
    assert!(list["invitations"].as_array().unwrap().is_empty());

    // Someone with an account accepts signed in to it; the role replaces theirs at the scope.
    let (s, v) = invite(
        &ada_t,
        json!({ "email": "bob@example.com", "role": "gw.auditor", "scope": "organisation:org_1", "deliver": "link" }),
    )
    .await;
    assert_eq!(s, 201);
    let bob_token = v["token"].as_str().expect("handed on").to_owned();
    assert!(v["link"].as_str().unwrap().ends_with(&bob_token));
    let mut stranger = Browser::new(&id);
    let a = stranger.post(&accept, json!({ "token": bob_token })).await;
    assert_eq!((a.status, a.json["error"]["details"]["code"].as_str()), (401, Some("sign_in_required")));
    let mut bobs = Browser::new(&id);
    id.sign_in(&mut bobs, "bob@example.com").await;
    let a = bobs.post(&accept, json!({ "token": bob_token })).await;
    assert_eq!((a.status, a.json["next"].as_str()), (200, Some("continue")), "{a:?}");
    let roles: Vec<String> = sqlx::query_scalar("SELECT role FROM role_bindings WHERE principal_id = $1")
        .bind(&bob)
        .fetch_all(id.store().pool())
        .await
        .unwrap();
    assert_eq!(roles, ["gw.auditor"]);

    // The account page's own invitations: accept one, decline another.
    invite(&ada_t, json!({ "email": "bob@example.com", "role": "gw.viewer", "scope": "gw.workspace:ws_1" })).await;
    invite(&ada_t, json!({ "email": "bob@example.com", "role": "gw.viewer", "scope": "gw.workspace:ws_2" })).await;
    let a = bobs.get(&format!("{}/ui-api/v1/account/invitations", id.public)).await;
    let mine = a.json["invitations"].as_array().unwrap().clone();
    assert_eq!(mine.len(), 2, "{a:?}");
    let (i1, i2) = (mine[0]["id"].as_str().unwrap(), mine[1]["id"].as_str().unwrap());
    assert_eq!(
        bobs.post(&format!("{}/ui-api/v1/account/invitations/{i1}/accept", id.public), json!({})).await.status,
        200
    );
    assert_eq!(
        bobs.post(&format!("{}/ui-api/v1/account/invitations/{i2}/decline", id.public), json!({})).await.status,
        204
    );
    assert_eq!(
        stranger.post(&format!("{}/ui-api/v1/account/invitations/{i2}/decline", id.public), json!({})).await.status,
        401
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM role_bindings WHERE principal_id = $1")
        .bind(&bob)
        .fetch_one(id.store().pool())
        .await
        .unwrap();
    assert_eq!(n, 2);

    // Another product's pending invitation at the scope is not replaced by this one.
    id.sql(
        "INSERT INTO invitations (id, token_hash, email, role, scope_type, scope_id, realm, invited_by, emailed, labels, expires_at)
         VALUES ('inv_other', '\\x00', 'dan@example.com', 'shop.user', 'organisation', 'org_1', 'gateway', 'someone', false, '{}', now() + interval '1 day')",
    )
    .await;
    let (s, v) =
        invite(&ada_t, json!({ "email": "dan@example.com", "role": "gw.viewer", "scope": "organisation:org_1" })).await;
    assert_eq!((s, code(&v)), (409, Some("invitation_exists")), "{v}");

    // An address nobody confirmed sees nothing sent to it, and takes nothing.
    invite(&ada_t, json!({ "email": "bob@example.com", "role": "gw.viewer", "scope": "gw.workspace:ws_2" })).await;
    id.sql(&format!("UPDATE accounts SET email_verified_at = NULL WHERE id = '{bob}'")).await;
    let a = bobs.get(&format!("{}/ui-api/v1/account/invitations", id.public)).await;
    assert_eq!(a.json["invitations"], json!([]), "{a:?}");
    let pending: String = sqlx::query_scalar("SELECT id FROM invitations WHERE email = 'bob@example.com'")
        .fetch_one(id.store().pool())
        .await
        .unwrap();
    let a = bobs.post(&format!("{}/ui-api/v1/account/invitations/{pending}/accept", id.public), json!({})).await;
    assert_eq!(a.status, 404, "{a:?}");

    // Resent (a new link) and revoked.
    let (s, v) =
        invite(&ada_t, json!({ "email": "zoe@example.com", "role": "gw.viewer", "scope": "organisation:org_1" })).await;
    assert_eq!(s, 201);
    let zoe = v["id"].as_str().unwrap().to_owned();
    let old = link_token(&id.mail().await.last().unwrap().mail.text);
    let (s, _) = call(&id, &ada_t, Method::POST, &format!("/admin/v1/invitations/{zoe}/resend"), None).await;
    assert_eq!(s, 200);
    assert_eq!(b.post(&lookup, json!({ "token": old })).await.status, 410);
    let (s, _) = call(&id, &carol_t, Method::DELETE, &format!("/admin/v1/invitations/{zoe}"), None).await;
    assert_eq!(s, 404);
    let (s, _) = call(&id, &ada_t, Method::DELETE, &format!("/admin/v1/invitations/{zoe}"), None).await;
    assert_eq!(s, 204);
    id.running.stop().await;
}

#[tokio::test]
async fn custom_roles_are_bound_renamed_and_deleted() {
    let id = world().await;
    // shop's catalogue, pushed by its service.
    let st = &id.running.state;
    let source: pacod_id_authz::ManifestSource = serde_json::from_value(manifest("shop")).unwrap();
    {
        let mut tx = st.store.begin().await.unwrap();
        pacod_id::store::authz::catalog_put(&mut tx, "shop", &serde_json::to_value(&source).unwrap()).await.unwrap();
        tx.commit().await.unwrap();
        st.catalog.reload(&st.store).await.unwrap();
    }
    person(&id, "idadmin@example.com", "id.admin", ScopeRef::platform()).await;
    let bob = person(&id, "bob@example.com", "", ScopeRef::platform()).await;
    person(&id, "root@example.com", "gw.platform_admin", ScopeRef::platform()).await;
    let (admin_t, _) = as_person(&id, "idadmin@example.com").await;
    let (root_t, _) = as_person(&id, "root@example.com").await;

    let (s, v) =
        call(&id, &admin_t, Method::POST, "/admin/v1/custom-roles", Some(json!({ "product": "gw", "name": "tester" })))
            .await;
    assert_eq!((s, code(&v)), (422, Some("custom_roles_off")));
    let (s, _) = call(
        &id,
        &root_t,
        Method::POST,
        "/admin/v1/custom-roles",
        Some(json!({ "product": "shop", "name": "tester" })),
    )
    .await;
    assert_eq!(s, 403, "a gateway admin is no shop admin");
    let (s, v) = call(
        &id,
        &admin_t,
        Method::POST,
        "/admin/v1/custom-roles",
        Some(json!({ "product": "shop", "name": "Bad Name" })),
    )
    .await;
    assert_eq!((s, code(&v)), (422, Some("bad_name")));
    let (s, v) = call(
        &id,
        &admin_t,
        Method::POST,
        "/admin/v1/custom-roles",
        Some(json!({ "product": "shop", "name": "tester", "description": "Tries things" })),
    )
    .await;
    assert_eq!((s, v["role"].as_str()), (201, Some("shop.custom.tester")), "{v}");
    let (s, v) = call(
        &id,
        &admin_t,
        Method::POST,
        "/admin/v1/bindings",
        Some(json!({ "principal": bob, "role": "shop.custom.tester", "scope": "platform" })),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let (s, v) =
        call(&id, &admin_t, Method::PATCH, "/admin/v1/custom-roles/shop/tester", Some(json!({ "name": "qa" }))).await;
    assert_eq!(s, 200, "{v}");
    let roles: Vec<String> = sqlx::query_scalar("SELECT role FROM role_bindings WHERE principal_id = $1")
        .bind(&bob)
        .fetch_all(id.store().pool())
        .await
        .unwrap();
    assert_eq!(roles, ["shop.custom.qa"], "the bindings follow the name");
    let (s, _) = call(&id, &admin_t, Method::DELETE, "/admin/v1/custom-roles/shop/qa", None).await;
    assert_eq!(s, 200);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM role_bindings WHERE principal_id = $1")
        .bind(&bob)
        .fetch_one(id.store().pool())
        .await
        .unwrap();
    assert_eq!(n, 0);
    let (_, v) = call(&id, &admin_t, Method::GET, "/admin/v1/custom-roles?product=shop", None).await;
    assert_eq!(v["custom_roles"], json!([]));
    // The identity audit log, for identity admins.
    let (s, v) = call(&id, &admin_t, Method::GET, "/admin/v1/audit?action=custom_role_deleted", None).await;
    assert_eq!((s, v["entries"][0]["subject"].as_str()), (200, Some("shop.custom.qa")));
    let (s, _) = call(&id, &root_t, Method::GET, "/admin/v1/audit", None).await;
    assert_eq!(s, 403);
    id.running.stop().await;
}
