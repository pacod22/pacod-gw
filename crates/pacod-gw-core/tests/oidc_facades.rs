//! The identity facades (identity concept 13.2): users, role bindings and admin tokens in OIDC
//! mode, answered from pacod-id (started in process) in the gateway's shapes.

mod common;
#[path = "common/id.rs"]
mod id;

use std::time::Duration;

use id::Id;
use pacod_id_authz::ScopeRef;
use serde_json::{Value, json};

/// pacod-id and a gateway in OIDC mode, with `ada` a platform admin and `bob` without a role.
struct World {
    id: Id,
    gw: common::TestGw,
    ada: String,
    bob: String,
    ada_t: String,
    bob_t: String,
}

async fn world() -> World {
    let id = Id::start().await;
    let gw = id::gateway(&id, |_| {}).await;
    id.wait_for_gw_catalog().await;
    let ada = id.account("ada@example.com").await;
    let bob = id.account("bob@example.com").await;
    id.bind(&ada, "gw.platform_admin", ScopeRef::platform()).await;
    let ada_t = id.sign_in("ada@example.com").await["access_token"].as_str().unwrap().to_owned();
    let bob_t = id.sign_in("bob@example.com").await["access_token"].as_str().unwrap().to_owned();
    let w = World { id, gw, ada, bob, ada_t, bob_t };
    for _ in 0..60 {
        if w.call(&w.ada_t, "GET", "/session", None).await.1["platform_admin"] == true {
            return w;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("ada's role never reached the gateway");
}

impl World {
    async fn call(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.gw.api_as(token, method, path, body).await
    }

    /// As `call`, with the answer's headers.
    async fn raw(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> reqwest::Response {
        let mut r =
            self.gw.http.request(method.parse().unwrap(), format!("{}/api/v1{path}", self.gw.admin)).bearer_auth(token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        r.send().await.unwrap()
    }

    async fn stop(self) {
        self.gw.app.shutdown().await;
        self.id.running.stop().await;
    }
}

#[tokio::test]
async fn users_are_pacod_ids_accounts() {
    let w = world().await;
    let (s, v) = w.call(&w.ada_t, "GET", "/users?limit=50", None).await;
    assert_eq!(s, 200, "{v}");
    let emails: Vec<&str> = v["items"].as_array().unwrap().iter().map(|u| u["email"].as_str().unwrap()).collect();
    assert_eq!(emails, ["ada@example.com", "bob@example.com"]);
    assert_eq!(w.call(&w.bob_t, "GET", "/users", None).await.0, 403, "platform admins only, as today");

    // Create, read, change, delete: the gateway's shapes and answers.
    let (s, carol) = w
        .call(
            &w.ada_t,
            "POST",
            "/users",
            Some(json!({ "email": "Carol@Example.com", "password": "a long passphrase for carol" })),
        )
        .await;
    assert_eq!(
        (s, carol["email"].as_str(), carol["version"].as_i64()),
        (201, Some("carol@example.com"), Some(1)),
        "{carol}"
    );
    let carol_id = carol["id"].as_str().unwrap().to_owned();
    assert!(carol_id.starts_with("usr_"));
    let (s, v) =
        w.call(&w.ada_t, "POST", "/users", Some(json!({ "email": "dan@example.com", "password": "short" }))).await;
    assert_eq!(s, 422, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("at least 12"), "{v}");
    let (s, _) = w
        .call(
            &w.ada_t,
            "POST",
            "/users",
            Some(json!({ "email": "carol@example.com", "password": "another long passphrase" })),
        )
        .await;
    assert_eq!(s, 409);
    let path = format!("/users/{carol_id}");
    let r = w
        .raw(
            &w.ada_t,
            "PUT",
            &path,
            Some(json!({ "email": "carol@example.com", "display_name": "Carol", "status": "active" })),
        )
        .await;
    assert_eq!(r.status(), 428, "a replace sends the version");
    let (s, v) = w
        .call(
            &w.ada_t,
            "PUT",
            &path,
            Some(json!({ "email": "carol@example.com", "display_name": "Carol", "status": "disabled", "version": 1 })),
        )
        .await;
    assert_eq!((s, v["display_name"].as_str(), v["status"].as_str()), (200, Some("Carol"), Some("disabled")), "{v}");
    assert_eq!(w.call(&w.ada_t, "GET", &path, None).await.1["status"], "disabled");
    let (s, v) = w.call(&w.ada_t, "POST", &format!("/users/{}/verification", w.bob), None).await;
    assert_eq!((s, v["error"]["message"].as_str()), (409, Some("this user's email address is already confirmed")));
    assert_eq!(w.call(&w.ada_t, "DELETE", &path, None).await.0, 204);
    assert_eq!(w.call(&w.ada_t, "GET", &path, None).await.0, 404);

    // The last platform admin stays.
    let (s, v) = w.call(&w.ada_t, "DELETE", &format!("/users/{}", w.ada), None).await;
    assert_eq!(s, 409, "{v}");
    w.stop().await;
}

#[tokio::test]
async fn role_bindings_are_read_from_the_feed_and_written_to_pacod_id() {
    let w = world().await;
    // A role given through the gateway: the answer waits until this instance's copy has it.
    let r = w
        .raw(
            &w.ada_t,
            "POST",
            "/role-bindings",
            Some(json!({ "principal": w.bob, "role": "viewer", "scope": { "type": "platform" } })),
        )
        .await;
    assert_eq!(r.status(), 201);
    assert_eq!(r.headers()["x-gw-applied"], "1/1");
    assert!(r.headers().contains_key("x-pid-seq"));
    let made: Value = r.json().await.unwrap();
    let binding = made["id"].as_str().unwrap().to_owned();
    assert!(binding.starts_with("rb_"));
    let (_, me) = w.call(&w.bob_t, "GET", "/session", None).await;
    assert_eq!(me["roles"][0]["role"], "viewer", "read your own writes: {me}");

    let (s, v) = w.call(&w.ada_t, "GET", "/role-bindings?limit=500", None).await;
    assert_eq!(s, 200);
    let held: Vec<(&str, &str)> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["principal"].as_str().unwrap(), b["role"].as_str().unwrap()))
        .collect();
    assert!(held.contains(&(w.ada.as_str(), "platform_admin")) && held.contains(&(w.bob.as_str(), "viewer")), "{v}");
    // Bob sees his own binding only.
    let (_, v) = w.call(&w.bob_t, "GET", "/role-bindings", None).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(w.call(&w.bob_t, "GET", &format!("/role-bindings/{binding}"), None).await.0, 200);

    // Bob may not give roles; the last platform admin binding stays.
    let (s, _) = w
        .call(
            &w.bob_t,
            "POST",
            "/role-bindings",
            Some(json!({ "principal": w.bob, "role": "auditor", "scope": { "type": "platform" } })),
        )
        .await;
    assert_eq!(s, 403);
    let ada_binding = held_id(&w, &w.ada, "platform_admin").await;
    let (s, v) = w.call(&w.ada_t, "DELETE", &format!("/role-bindings/{ada_binding}"), None).await;
    assert_eq!(s, 409, "{v}");

    assert_eq!(w.call(&w.ada_t, "DELETE", &format!("/role-bindings/{binding}"), None).await.0, 204);
    assert_eq!(w.call(&w.bob_t, "GET", "/session", None).await.1["roles"], json!([]));
    w.stop().await;
}

async fn held_id(w: &World, principal: &str, role: &str) -> String {
    let (_, v) = w.call(&w.ada_t, "GET", "/role-bindings?limit=500", None).await;
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["principal"] == principal && b["role"] == role)
        .and_then(|b| b["id"].as_str())
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn admin_tokens_are_service_accounts_with_one_token() {
    let w = world().await;
    let (s, made) = w.call(&w.ada_t, "POST", "/admin-tokens", Some(json!({ "name": "deploy bot" }))).await;
    assert_eq!(s, 201, "{made}");
    let secret = made["secret"].as_str().unwrap().to_owned();
    let tok = made["id"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("pidt_") && tok.starts_with("svc_"), "{made}");
    assert_eq!(made["hint"].as_str().unwrap(), &secret[secret.len() - 4..]);
    let (s, _) = w.call(&w.ada_t, "POST", "/admin-tokens", Some(json!({ "name": "Deploy Bot" }))).await;
    assert_eq!(s, 409, "the name is taken");

    // The token signs in to the gateway's API.
    let (s, me) = w.call(&secret, "GET", "/session", None).await;
    assert_eq!((s, me["kind"].as_str(), me["display"].as_str()), (200, Some("token"), Some("deploy bot")), "{me}");

    let (_, list) = w.call(&w.ada_t, "GET", "/admin-tokens", None).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    let path = format!("/admin-tokens/{tok}");
    let (s, v) =
        w.call(&w.ada_t, "PUT", &path, Some(json!({ "name": "release bot", "status": "active", "version": 1 }))).await;
    assert_eq!((s, v["name"].as_str()), (200, Some("release bot")), "{v}");
    let (s, v) = w
        .call(&w.ada_t, "PUT", &path, Some(json!({ "name": "release bot", "status": "active", "expires_at": "2099-01-01T00:00:00Z", "version": 1 })))
        .await;
    assert_eq!(s, 422, "{v}");
    assert_eq!(w.call(&w.bob_t, "GET", "/admin-tokens", None).await.0, 403);

    // Deleted: the token stops working within seconds.
    assert_eq!(w.call(&w.ada_t, "DELETE", &path, None).await.0, 204);
    let mut refused = false;
    for _ in 0..60 {
        if w.call(&secret, "GET", "/session", None).await.0 == 401 {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(refused, "the deleted token is refused");
    w.stop().await;
}
