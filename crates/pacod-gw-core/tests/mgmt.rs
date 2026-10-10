//! The management API end to end, against a real PostgreSQL.

mod common;

use std::time::{Duration, Instant};

use common::{ADMIN_EMAIL, PROVIDER_SECRET, TestGw};
use futures_util::StreamExt;
use serde_json::{Value, json};

#[tokio::test]
async fn login_and_session_life_cycle() {
    let gw = TestGw::start(None).await;

    let (st, me) = gw.api("GET", "/session", None).await;
    assert_eq!(st, 200);
    assert_eq!(me["display"], ADMIN_EMAIL);
    assert_eq!(me["platform_admin"], true);

    // Wrong password and unknown account answer the same way.
    for (email, pw) in [(ADMIN_EMAIL, "wrong password!!"), ("nobody@example.test", "whatever it is")] {
        let r = gw
            .http
            .post(format!("{}/api/v1/session", gw.admin))
            .json(&json!({ "email": email, "password": pw }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        assert_eq!(r.json::<Value>().await.unwrap()["error"]["message"], "wrong email or password");
    }

    // No token, a malformed token, and a data-plane key are all refused.
    for token in ["", "pgws_x_y", "pgw_abc_def"] {
        let (st, _) = gw.api_as(token, "GET", "/orgs", None).await;
        assert_eq!(st, 401, "token {token:?}");
    }

    // Logout ends the session.
    let token = gw.login(ADMIN_EMAIL, common::ADMIN_PASSWORD).await;
    assert_eq!(gw.api_as(&token, "GET", "/session", None).await.0, 200);
    assert_eq!(gw.api_as(&token, "DELETE", "/session", None).await.0, 204);
    assert_eq!(gw.api_as(&token, "GET", "/session", None).await.0, 401);

    // Repeated failures lock the account, even for the right password.
    for _ in 0..10 {
        gw.http
            .post(format!("{}/api/v1/session", gw.admin))
            .json(&json!({ "email": "victim@example.test", "password": "guess guess guess" }))
            .send()
            .await
            .unwrap();
    }
    let r = gw
        .http
        .post(format!("{}/api/v1/session", gw.admin))
        .json(&json!({ "email": "victim@example.test", "password": "guess guess guess" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
}

#[tokio::test]
async fn resources_validate_and_never_return_secrets() {
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", "https://api.openai.com/v1").await;

    // Validation.
    for (path, body, needle) in [
        ("/orgs".to_owned(), json!({ "name": "Not A Slug" }), "lower-case"),
        ("/orgs".to_owned(), json!({ "name": "acme" }), "already exists"),
        (format!("/orgs/{}/workspaces", fx.org), json!({ "name": "w", "time_zone": "Mars/Phobos" }), "time zone"),
        (
            format!("/orgs/{}/workspaces", fx.org),
            json!({ "name": "w", "tag_keys": ["a", "b", "c", "d", "e", "f", "g", "h", "i"] }),
            "eight",
        ),
        ("/providers".to_owned(), json!({ "name": "p", "family": "openai", "base_url": "ftp://x" }), "http"),
        (
            "/providers".to_owned(),
            json!({ "name": "meta", "family": "openai", "base_url": "http://169.254.169.254/latest" }),
            "private",
        ),
        ("/users".to_owned(), json!({ "email": "a@b.c", "password": "short" }), "at least 12"),
    ] {
        let (st, v) = gw.api("POST", &path, Some(body)).await;
        assert!(st == 422 || st == 409, "POST {path}: {st} {v}");
        assert!(v["error"]["message"].as_str().unwrap().contains(needle), "{v}");
    }

    // Nothing the API returns contains a secret or a stored hash.
    let (_, key_list) = gw.api("GET", &format!("/orgs/{}/workspaces/{}/keys", fx.org, fx.ws), None).await;
    let (_, creds) = gw.api("GET", &format!("/providers/{}/credentials", fx.provider), None).await;
    let (_, users) = gw.api("GET", "/users", None).await;
    let all = format!("{key_list}{creds}{users}");
    assert!(!all.contains(&fx.key), "the API key secret is shown once only");
    assert!(!all.contains(PROVIDER_SECRET));
    for forbidden in ["secret_hmac", "password_hash", "ciphertext", "wrapped_dek", "$argon2"] {
        assert!(!all.contains(forbidden), "{forbidden} leaked: {all}");
    }
    assert_eq!(key_list["items"][0]["hint"].as_str().unwrap().len(), 4);

    // The secret is not in the database in clear text either.
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT doc::text FROM config_objects").fetch_all(gw.app.store.pool()).await.unwrap();
    let dump: String = rows.into_iter().map(|r| r.0).collect();
    assert!(!dump.contains(PROVIDER_SECRET) && !dump.contains(&fx.key));

    // Objects with dependants cannot be deleted.
    let (st, v) = gw.api("DELETE", &format!("/orgs/{}", fx.org), None).await;
    assert_eq!(st, 409, "{v}");
    let (st, _) = gw.api("DELETE", &format!("/providers/{}", fx.provider), None).await;
    assert_eq!(st, 409);
}

#[tokio::test]
async fn concurrent_edits_are_detected_and_every_change_is_audited() {
    let gw = TestGw::start(None).await;
    let org = gw.create("/orgs", json!({ "name": "acme" })).await;
    let path = format!("/orgs/{}", org.id);
    let v1 = org.body["version"].as_i64().unwrap();

    let (st, updated) =
        gw.api("PUT", &path, Some(json!({ "name": "acme", "display_name": "Acme", "version": v1 }))).await;
    assert_eq!(st, 200);
    assert_eq!(updated["version"].as_i64().unwrap(), v1 + 1);

    // A second writer still holding the old version is refused.
    let (st, v) = gw.api("PUT", &path, Some(json!({ "name": "acme", "display_name": "Other", "version": v1 }))).await;
    assert_eq!(st, 409);
    assert_eq!(v["error"]["type"], "version_mismatch");
    let (_, now) = gw.api("GET", &path, None).await;
    assert_eq!(now["display_name"], "Acme");

    // Each change is one configuration version and one audit entry, in the same transaction.
    let (_, status) = gw.api("GET", "/system/status", None).await;
    let head = status["config_head"].as_i64().unwrap();
    assert_eq!(status["config_version"], head);
    let (_, changes) = gw.api("GET", "/system/config-versions?limit=3", None).await;
    let c = changes.as_array().unwrap();
    assert_eq!(c[0]["version"], head);
    assert_eq!((c[0]["op"].as_str(), c[0]["object_id"].as_str()), (Some("update"), Some(org.id.as_str())));
    assert_eq!(c[1]["op"], "create");

    let (_, log) = gw.api("GET", "/audit-log?limit=5", None).await;
    let entries = log["items"].as_array().unwrap();
    let upd = &entries[0];
    assert_eq!((upd["action"].as_str(), upd["object_id"].as_str()), (Some("update"), Some(org.id.as_str())));
    assert_eq!(upd["config_version"], head);
    assert_ne!(upd["before_hash"], upd["after_hash"]);
    assert!(upd["actor"].as_str().unwrap().starts_with("usr_"));
    // The refused write left no trace.
    let updates = entries.iter().filter(|e| e["action"] == "update" && e["object_id"] == org.id.as_str()).count();
    assert_eq!(updates, 1);
}

#[tokio::test]
async fn roles_confine_what_a_principal_can_see_and_change() {
    let gw = TestGw::start(None).await;
    let a = gw.create("/orgs", json!({ "name": "org-a" })).await.id;
    let b = gw.create("/orgs", json!({ "name": "org-b" })).await.id;
    let ws_a = gw.create(&format!("/orgs/{a}/workspaces"), json!({ "name": "one" })).await.id;
    let ws_b = gw.create(&format!("/orgs/{b}/workspaces"), json!({ "name": "one" })).await.id;

    let mut tokens = std::collections::HashMap::new();
    for (name, role, scope) in [
        ("orgadmin", "org_admin", json!({ "type": "organisation", "id": a })),
        ("wsadmin", "workspace_admin", json!({ "type": "workspace", "id": ws_a })),
        ("viewer", "viewer", json!({ "type": "organisation", "id": a })),
        ("nobody", "", Value::Null),
    ] {
        let email = format!("{name}@example.test");
        let user = gw.create("/users", json!({ "email": email, "password": "a long password 1" })).await.id;
        if !role.is_empty() {
            gw.create("/role-bindings", json!({ "principal": user, "role": role, "scope": scope })).await;
        }
        tokens.insert(name, gw.login(&email, "a long password 1").await);
    }
    let call = |who: &'static str, method: &'static str, path: String, body: Option<Value>| {
        let token = tokens[who].clone();
        let gw = &gw;
        async move { gw.api_as(&token, method, &path, body).await.0 }
    };
    let keys_a = format!("/orgs/{a}/workspaces/{ws_a}/keys");
    let keys_b = format!("/orgs/{b}/workspaces/{ws_b}/keys");
    let new_key = || Some(json!({ "name": format!("k-{}", uuid::Uuid::now_v7().simple()) }));

    // Organisation admin: full control inside org-a, nothing in org-b, no platform resources.
    assert_eq!(call("orgadmin", "POST", format!("/orgs/{a}/workspaces"), Some(json!({ "name": "two" }))).await, 201);
    assert_eq!(call("orgadmin", "POST", keys_a.clone(), new_key()).await, 201);
    assert_eq!(call("orgadmin", "POST", keys_b.clone(), new_key()).await, 403);
    assert_eq!(call("orgadmin", "GET", keys_b.clone(), None).await, 403);
    assert_eq!(call("orgadmin", "POST", "/orgs".into(), Some(json!({ "name": "org-c" }))).await, 403);
    assert_eq!(
        call(
            "orgadmin",
            "POST",
            "/providers".into(),
            Some(json!({ "name": "p", "family": "openai", "base_url": "https://x.example" }))
        )
        .await,
        403
    );
    assert_eq!(call("orgadmin", "GET", "/users".into(), None).await, 403);
    assert_eq!(call("orgadmin", "GET", "/system/status".into(), None).await, 403);

    // Workspace admin: its workspace only.
    assert_eq!(call("wsadmin", "POST", keys_a.clone(), new_key()).await, 201);
    assert_eq!(call("wsadmin", "POST", format!("/orgs/{a}/workspaces"), Some(json!({ "name": "three" }))).await, 403);
    assert_eq!(call("wsadmin", "POST", keys_b.clone(), new_key()).await, 403);

    // Viewer: reads, never writes.
    assert_eq!(call("viewer", "GET", keys_a.clone(), None).await, 200);
    assert_eq!(call("viewer", "POST", keys_a.clone(), new_key()).await, 403);
    assert_eq!(call("viewer", "GET", keys_b.clone(), None).await, 403);

    // No role: nothing.
    assert_eq!(call("nobody", "GET", keys_a.clone(), None).await, 403);

    // Listings only show what the principal has a role in.
    let (_, orgs) = gw.api_as(&tokens["orgadmin"], "GET", "/orgs", None).await;
    let names: Vec<&str> = orgs["items"].as_array().unwrap().iter().map(|o| o["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["org-a"]);
    let (_, orgs) = gw.api_as(&tokens["nobody"], "GET", "/orgs", None).await;
    assert!(orgs["items"].as_array().unwrap().is_empty());
    let (_, wss) = gw.api_as(&tokens["wsadmin"], "GET", &format!("/orgs/{a}/workspaces"), None).await;
    assert_eq!(wss["items"].as_array().unwrap().len(), 1, "a workspace admin sees only its workspace");

    // An organisation admin can give roles inside the organisation, not outside and not
    // platform-wide.
    let viewer_id = {
        let (_, users) = gw.api("GET", "/users", None).await;
        users["items"].as_array().unwrap().iter().find(|u| u["email"] == "nobody@example.test").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let bind = |scope: Value, role: &'static str| Some(json!({ "principal": viewer_id, "role": role, "scope": scope }));
    assert_eq!(
        call("orgadmin", "POST", "/role-bindings".into(), bind(json!({ "type": "workspace", "id": ws_a }), "viewer"))
            .await,
        201
    );
    assert_eq!(
        call("orgadmin", "POST", "/role-bindings".into(), bind(json!({ "type": "workspace", "id": ws_b }), "viewer"))
            .await,
        403
    );
    assert_eq!(
        call("orgadmin", "POST", "/role-bindings".into(), bind(json!({ "type": "platform" }), "platform_admin")).await,
        403
    );

    // The last platform admin cannot be removed.
    let (_, me) = gw.api("GET", "/session", None).await;
    let (st, v) = gw.api("DELETE", &format!("/users/{}", me["id"].as_str().unwrap()), None).await;
    assert_eq!(st, 409, "{v}");
}

#[tokio::test]
async fn a_change_reaches_a_second_instance_within_two_seconds() {
    let a = TestGw::start(None).await;
    let b = TestGw::start_on(&a.database_url, None, |_| {}).await;
    let mock = pacod_gw_testkit::mock::MockProvider::spawn().await;

    // Everything is created through instance A.
    let fx = a.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let call_b = || async {
        b.http
            .post(format!("{}/proxy/openai/v1/chat/completions", b.data))
            .bearer_auth(&fx.key)
            .json(&json!({ "model": "m", "messages": [] }))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    };

    // Instance B serves the new key within two seconds.
    let start = Instant::now();
    while call_b().await != 200 {
        assert!(start.elapsed() < Duration::from_secs(2), "instance B did not pick up the new key in 2 s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A revocation made on A stops the key on B within two seconds.
    let (st, _) =
        a.api("POST", &format!("/orgs/{}/workspaces/{}/keys/{}/revoke", fx.org, fx.ws, fx.key_id), None).await;
    assert_eq!(st, 200);
    let start = Instant::now();
    while call_b().await != 401 {
        assert!(start.elapsed() < Duration::from_secs(2), "the revocation did not reach instance B in 2 s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Both instances report themselves and the version they applied.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let (_, status) = a.api("GET", "/system/status", None).await;
    assert_eq!(status["instances"].as_array().unwrap().len(), 2, "{status}");
    assert_eq!(status["instances_current"], 2);
}

#[tokio::test]
async fn the_event_feed_announces_configuration_changes() {
    let gw = TestGw::start(None).await;
    let r = gw.http.get(format!("{}/api/v1/events", gw.admin)).bearer_auth(&gw.token).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "text/event-stream");
    let mut stream = r.bytes_stream();

    let org = gw.create("/orgs", json!({ "name": "acme" })).await.id;
    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !seen.contains(&org) {
        let chunk = tokio::time::timeout_at(deadline.into(), stream.next()).await.expect("an event within 5 s");
        seen.push_str(&String::from_utf8_lossy(&chunk.unwrap().unwrap()));
    }
    assert!(seen.contains("event: config"));
    assert!(seen.contains("\"op\":\"create\"") && seen.contains("\"kind\":\"organisation\""));
}

#[tokio::test]
async fn guessing_admin_tokens_is_slowed_down_per_address() {
    let gw = TestGw::start(None).await;
    let call = |token: &str| {
        let req = gw.http.get(format!("{}/api/v1/orgs", gw.admin)).bearer_auth(token);
        async move { req.send().await.unwrap().status().as_u16() }
    };
    for i in 0..60 {
        assert_eq!(call(&format!("pgwa_guess{i}_secret")).await, 401);
    }
    assert_eq!(call("pgwa_guess61_secret").await, 429, "the address is held off for a minute");
    // Sessions come from the console's own address and are not held off with it.
    assert_eq!(call(&gw.token).await, 200);
}

#[tokio::test]
async fn the_event_feed_tells_each_user_only_about_what_they_may_read() {
    let gw = TestGw::start(None).await;
    let a = gw.create("/orgs", json!({ "name": "org-a" })).await.id;
    let ws_a = gw.create(&format!("/orgs/{a}/workspaces"), json!({ "name": "one" })).await.id;
    let user = gw.create("/users", json!({ "email": "w@example.test", "password": "a long password 1" })).await.id;
    gw.create(
        "/role-bindings",
        json!({ "principal": user, "role": "viewer", "scope": { "type": "workspace", "id": ws_a } }),
    )
    .await;
    let token = gw.login("w@example.test", "a long password 1").await;
    let r = gw.http.get(format!("{}/api/v1/events", gw.admin)).bearer_auth(&token).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let mut stream = r.bytes_stream();

    // Another organisation, with a workspace, a key that is then deleted, and a provider.
    let b = gw.create("/orgs", json!({ "name": "org-b" })).await.id;
    let ws_b = gw.create(&format!("/orgs/{b}/workspaces"), json!({ "name": "one" })).await.id;
    let key_b = gw.create(&format!("/orgs/{b}/workspaces/{ws_b}/keys"), json!({ "name": "k" })).await.id;
    let (st, _) = gw.api("DELETE", &format!("/orgs/{b}/workspaces/{ws_b}/keys/{key_b}"), None).await;
    assert!(st < 300, "{st}");
    let provider =
        gw.create("/providers", json!({ "name": "p", "family": "openai", "base_url": "https://x.example" })).await.id;
    // What the user may read: a model, and a key in their own workspace, which comes last.
    let model = gw.draft_model(&provider, "p/m", "m", json!({})).await;
    let key_a = gw.create(&format!("/orgs/{a}/workspaces/{ws_a}/keys"), json!({ "name": "k" })).await.id;

    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !seen.contains(&key_a) {
        let chunk = tokio::time::timeout_at(deadline.into(), stream.next()).await.expect("an event within 5 s");
        seen.push_str(&String::from_utf8_lossy(&chunk.unwrap().unwrap()));
    }
    assert!(seen.contains(&model), "models are listed to everyone: {seen}");
    for hidden in [&b, &ws_b, &key_b, &provider] {
        assert!(!seen.contains(hidden.as_str()), "{hidden} leaked: {seen}");
    }
}

#[tokio::test]
async fn the_event_feed_reports_job_progress_to_platform_roles_only() {
    let gw = TestGw::start(None).await;
    let a = gw.create("/orgs", json!({ "name": "org-a" })).await.id;
    let ws = gw.create(&format!("/orgs/{a}/workspaces"), json!({ "name": "one" })).await.id;
    let user = gw.create("/users", json!({ "email": "v@example.test", "password": "a long password 1" })).await.id;
    gw.create(
        "/role-bindings",
        json!({ "principal": user, "role": "viewer", "scope": { "type": "workspace", "id": ws } }),
    )
    .await;
    let token = gw.login("v@example.test", "a long password 1").await;
    let open = |token: String| {
        let req = gw.http.get(format!("{}/api/v1/events", gw.admin)).bearer_auth(token);
        async move { req.send().await.unwrap().bytes_stream() }
    };
    let mut admin = open(gw.token.clone()).await;
    let mut viewer = open(token).await;

    let (st, _) = gw.api("POST", "/discovery/runs", None).await;
    assert!(st < 300, "{st}");
    // A change the viewer may read marks the end of what they are sent.
    let key = gw.create(&format!("/orgs/{a}/workspaces/{ws}/keys"), json!({ "name": "k" })).await.id;

    let seen = read_until(&mut admin, "\"status\":\"finished\"").await;
    assert!(seen.contains("event: job") && seen.contains("\"job\":\"discovery\""), "{seen}");
    assert!(seen.contains("\"status\":\"started\""), "{seen}");
    let seen = read_until(&mut viewer, &key).await;
    assert!(!seen.contains("event: job"), "jobs are for platform roles: {seen}");
}

/// Reads a server-sent event stream until `needle` appears, for at most five seconds.
async fn read_until(
    stream: &mut (impl futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin),
    needle: &str,
) -> String {
    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !seen.contains(needle) {
        let chunk = tokio::time::timeout_at(deadline.into(), stream.next()).await.expect("an event within 5 s");
        seen.push_str(&String::from_utf8_lossy(&chunk.unwrap().unwrap()));
    }
    seen
}

#[tokio::test]
async fn credentials_are_wrapped_again_under_a_new_key_encryption_key() {
    use pacod_gw_core::crypto::envelope::Kek;
    let db = common::fresh_database().await;
    let (old, new) = (Kek::new("2026-01", [1u8; 32]), Kek::new("2027-01", [2u8; 32]));
    let before = TestGw::start_on(&db, None, |b| b.keks = vec![old.clone()]).await;
    let provider = before
        .create("/providers", json!({ "name": "p", "family": "openai", "base_url": "https://x.example" }))
        .await
        .id;
    let creds = format!("/providers/{provider}/credentials");
    before.create(&creds, json!({ "secret": "sk-old-0001" })).await;

    // After the rotation the instances know both keys. While one still runs with only the old
    // key, re-wrapping would take the credentials away from it, so it is refused.
    let rotated = TestGw::start_on(&db, None, |b| b.keks = vec![new.clone(), old.clone()]).await;
    let (st, refused) = rotated.api("POST", "/system/rewrap-secrets", None).await;
    assert_eq!(st, 409, "{refused}");
    assert!(refused.to_string().contains(&before.app.gw.boot.instance_id), "{refused}");
    before.app.shutdown().await;
    let (st, report) = rotated.api("POST", "/system/rewrap-secrets", None).await;
    assert_eq!(st, 200, "{report}");
    assert_eq!((report["kek_id"].as_str(), report["rewrapped"].as_u64()), (Some("2027-01"), Some(1)), "{report}");
    let (_, again) = rotated.api("POST", "/system/rewrap-secrets", None).await;
    assert_eq!((again["rewrapped"].as_u64(), again["unchanged"].as_u64()), (Some(0), Some(1)), "{again}");
    let (_, list) = rotated.api("GET", &creds, None).await;
    assert_eq!(list["items"][0]["kek_id"], "2027-01");

    // The old key can now go.
    let after = TestGw::start_on(&db, None, |b| b.keks = vec![new.clone()]).await;
    let (_, status) = after.api("GET", "/system/status", None).await;
    assert_eq!(status["problems"], json!([]), "{status}");
    assert!(after.app.gw.config.snapshot().providers[&provider].credentials.global.is_some());
}

#[tokio::test]
async fn the_openapi_document_describes_the_routes() {
    let gw = TestGw::start(None).await;
    let r = gw.http.get(format!("{}/api/v1/openapi.json", gw.admin)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let doc: Value = r.json().await.unwrap();
    assert_eq!(doc["openapi"], "3.1.0");
    for (path, method) in [
        ("/session", "post"),
        ("/orgs", "get"),
        ("/orgs/{org}", "put"),
        ("/orgs/{org}/workspaces", "post"),
        ("/orgs/{org}/workspaces/{ws}/keys", "post"),
        ("/orgs/{org}/workspaces/{ws}/keys/{id}/rotate", "post"),
        ("/providers/{id}/credentials", "post"),
        ("/role-bindings", "post"),
        ("/audit-log", "get"),
        ("/requests", "get"),
    ] {
        assert!(doc["paths"][path][method].is_object(), "missing {method} {path}");
    }
    // Every referenced schema exists.
    let text = doc.to_string();
    let schemas = doc["components"]["schemas"].as_object().unwrap();
    for part in text.split("#/components/schemas/").skip(1) {
        let name: String = part.chars().take_while(|c| *c != '"').collect();
        assert!(schemas.contains_key(&name), "dangling schema reference: {name}");
    }
    // Operation ids are unique: the client generator needs that.
    let mut ids = Vec::new();
    for item in doc["paths"].as_object().unwrap().values() {
        for op in item.as_object().unwrap().values() {
            ids.push(op["operationId"].as_str().unwrap().to_owned());
        }
    }
    let total = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), total, "duplicate operation ids");
    // The key secret schema is documented as shown once.
    assert!(schemas.contains_key("KeyWithSecret"));
    // The document the binary prints is the document the server serves.
    assert_eq!(doc, pacod_gw_core::mgmt::openapi_document());
}

#[tokio::test]
async fn health_endpoints_and_metrics() {
    let gw = TestGw::start(None).await;
    for base in [&gw.data, &gw.admin] {
        for path in ["/healthz", "/readyz"] {
            let r = gw.http.get(format!("{base}{path}")).send().await.unwrap();
            assert_eq!(r.status(), 200, "{base}{path}");
        }
    }
    // The management API is not served on the data-plane listener.
    let r = gw.http.get(format!("{}/api/v1/orgs", gw.data)).bearer_auth(&gw.token).send().await.unwrap();
    assert_eq!(r.status(), 404);
    let r = gw.http.get(format!("http://{}/metrics", gw.app.metrics_addr)).send().await.unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn a_workspace_admin_keeps_the_settings_of_its_own_workspace() {
    let gw = TestGw::start(None).await;
    let org = gw.create("/orgs", json!({ "name": "acme" })).await.id;
    let mine = gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "mine" })).await.id;
    let other = gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "other" })).await.id;
    let user = gw.create("/users", json!({ "email": "ws@example.test", "password": "a long password 1" })).await.id;
    gw.create(
        "/role-bindings",
        json!({ "principal": user, "role": "workspace_admin", "scope": { "type": "workspace", "id": mine } }),
    )
    .await;
    let token = gw.login("ws@example.test", "a long password 1").await;

    let settings = json!({ "name": "mine", "tag_keys": ["purpose"], "time_zone": "Europe/Vienna" });
    let (st, v) = gw.api_as(&token, "PUT", &format!("/orgs/{org}/workspaces/{mine}"), Some(settings.clone())).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!((v["time_zone"].as_str(), v["tag_keys"][0].as_str()), (Some("Europe/Vienna"), Some("purpose")));
    // Not a neighbour's workspace, and creating or deleting workspaces stays with the organisation.
    assert_eq!(gw.api_as(&token, "PUT", &format!("/orgs/{org}/workspaces/{other}"), Some(settings)).await.0, 403);
    assert_eq!(
        gw.api_as(&token, "POST", &format!("/orgs/{org}/workspaces"), Some(json!({ "name": "new" }))).await.0,
        403
    );
    assert_eq!(gw.api_as(&token, "DELETE", &format!("/orgs/{org}/workspaces/{mine}"), None).await.0, 403);

    // A wrong current password is a validation error; the session stays valid.
    let (st, v) = gw
        .api_as(
            &token,
            "PUT",
            "/session/password",
            Some(json!({ "current": "not my password", "new": "another long password" })),
        )
        .await;
    assert_eq!(st, 422, "{v}");
    assert_eq!(v["error"]["details"]["field"], "current");
    assert_eq!(gw.api_as(&token, "GET", "/session", None).await.0, 200);

    // The document says what a replace takes and which fields are never returned.
    let (_, doc) = gw.api("GET", "/openapi.json", None).await;
    let put =
        &doc["paths"]["/orgs/{org}/workspaces/{ws}"]["put"]["requestBody"]["content"]["application/json"]["schema"];
    assert_eq!(put["allOf"][1]["properties"]["version"]["type"], "integer");
    assert_eq!(doc["components"]["schemas"]["CredentialInput"]["properties"]["secret"]["writeOnly"], true);
    let limit =
        doc["paths"]["/orgs"]["get"]["parameters"].as_array().unwrap().iter().find(|p| p["name"] == "limit").unwrap();
    assert_eq!(limit["schema"]["type"], "integer");
}

#[tokio::test]
async fn an_organisation_admin_keeps_the_organisation_but_not_its_name_or_status() {
    let gw = TestGw::start(None).await;
    let org = gw.create("/orgs", json!({ "name": "acme", "display_name": "Acme" })).await.id;
    let ws = gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "one" })).await.id;
    let mut tokens = std::collections::HashMap::new();
    for (name, role, scope) in [
        ("orgadmin", "org_admin", json!({ "type": "organisation", "id": org })),
        ("wsviewer", "viewer", json!({ "type": "workspace", "id": ws })),
    ] {
        let email = format!("{name}@example.test");
        let user = gw.create("/users", json!({ "email": email, "password": "a long password 1" })).await.id;
        gw.create("/role-bindings", json!({ "principal": user, "role": role, "scope": scope })).await;
        tokens.insert(name, gw.login(&email, "a long password 1").await);
    }

    // Display name, settings and locks change; the slug and the status stay what they were.
    let (st, v) = gw
        .api_as(
            &tokens["orgadmin"],
            "PUT",
            &format!("/orgs/{org}"),
            Some(json!({
                "name": "renamed", "display_name": "Acme Inc.", "status": "disabled",
                "settings": { "locked": ["provider_tools"], "provider_tools": ["web_search"] }
            })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        (v["name"].as_str(), v["display_name"].as_str(), v["status"].as_str()),
        (Some("acme"), Some("Acme Inc."), Some("active"))
    );
    assert_eq!(v["settings"]["locked"][0], "provider_tools");

    // So does the routing assignment, which is part of the organisation's settings.
    let (st, a) = gw.api_as(&tokens["orgadmin"], "GET", &format!("/orgs/{org}/routing-assignment"), None).await;
    assert_eq!((st, a["writable"].as_bool()), (200, Some(true)), "{a}");
    let (st, v) = gw
        .api_as(
            &tokens["orgadmin"],
            "PUT",
            &format!("/orgs/{org}/routing-assignment"),
            Some(json!({ "scheme": null, "locked": true })),
        )
        .await;
    assert_eq!(st, 200, "{v}");

    // A workspace member reads which models the organisation has, and its roles carry names.
    assert_eq!(gw.api_as(&tokens["wsviewer"], "GET", &format!("/orgs/{org}/grants"), None).await.0, 200);
    assert_eq!(
        gw.api_as(&tokens["wsviewer"], "PUT", &format!("/orgs/{org}"), Some(json!({ "name": "acme" }))).await.0,
        403
    );
    let (_, me) = gw.api_as(&tokens["wsviewer"], "GET", "/session", None).await;
    let role = &me["roles"][0];
    assert_eq!(
        (role["scope_name"].as_str(), role["organisation_id"].as_str(), role["organisation_name"].as_str()),
        (Some("one"), Some(org.as_str()), Some("Acme Inc."))
    );

    // A platform admin still changes the slug and the status.
    let (st, v) = gw.api("PUT", &format!("/orgs/{org}"), Some(json!({ "name": "acme-2", "status": "disabled" }))).await;
    assert_eq!((st, v["name"].as_str(), v["status"].as_str()), (200, Some("acme-2"), Some("disabled")), "{v}");
}

#[tokio::test]
async fn an_organisation_admin_loads_the_organisation_template() {
    let gw = TestGw::start(None).await;
    let org = gw.create("/orgs", json!({ "name": "acme" })).await.id;
    let user =
        gw.create("/users", json!({ "email": "orgadmin@example.test", "password": "a long password 1" })).await.id;
    let scope = json!({ "type": "organisation", "id": org });
    gw.create("/role-bindings", json!({ "principal": user, "role": "org_admin", "scope": scope })).await;
    let token = gw.login("orgadmin@example.test", "a long password 1").await;

    // What the console does: the schemes first, then the settings that name them.
    let (st, t) = gw.api_as(&token, "GET", "/org-config-template", None).await;
    assert_eq!(st, 200, "{t}");
    // Its schemes are the default ones, which the routing page offers on their own too.
    let (st, defaults) = gw.api_as(&token, "GET", "/routing-scheme-defaults", None).await;
    assert_eq!((st, &defaults), (200, &t["routing_schemes"]));
    for scheme in t["routing_schemes"].as_array().unwrap() {
        let (st, v) = gw.api_as(&token, "POST", &format!("/orgs/{org}/routing-schemes"), Some(scheme.clone())).await;
        assert_eq!(st, 201, "{v}");
    }
    let (_, mut o) = gw.api_as(&token, "GET", &format!("/orgs/{org}"), None).await;
    o["settings"] = t["settings"].clone();
    let (st, v) = gw.api_as(&token, "PUT", &format!("/orgs/{org}"), Some(o)).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["settings"], t["settings"]);
    let (_, a) = gw.api_as(&token, "GET", &format!("/orgs/{org}/routing-assignment"), None).await;
    assert_eq!((a["scheme"].as_str(), a["locked"].as_bool()), (Some("standard"), Some(false)), "{a}");
}

#[tokio::test]
async fn role_bindings_go_with_the_user_organisation_or_workspace_they_bind() {
    let gw = TestGw::start(None).await;
    let org = gw.create("/orgs", json!({ "name": "acme" })).await.id;
    let ws = gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "one" })).await.id;
    let user = gw.create("/users", json!({ "email": "a@example.test", "password": "a long password 1" })).await.id;
    let other = gw.create("/users", json!({ "email": "b@example.test", "password": "a long password 1" })).await.id;
    for (who, scope) in [
        (&user, json!({ "type": "organisation", "id": org })),
        (&other, json!({ "type": "workspace", "id": ws })),
        (&other, json!({ "type": "organisation", "id": org })),
    ] {
        gw.create("/role-bindings", json!({ "principal": who, "role": "viewer", "scope": scope })).await;
    }
    let count = || async {
        let (_, v) = gw.api("GET", "/role-bindings", None).await;
        v["items"].as_array().unwrap().iter().filter(|b| b["role"] == "viewer").count()
    };
    assert_eq!(count().await, 3);

    assert_eq!(gw.api("DELETE", &format!("/users/{user}"), None).await.0, 204);
    assert_eq!(count().await, 2, "the deleted user's binding is gone");
    assert_eq!(gw.api("DELETE", &format!("/orgs/{org}/workspaces/{ws}"), None).await.0, 204);
    assert_eq!(count().await, 1, "the deleted workspace's binding is gone");
    assert_eq!(gw.api("DELETE", &format!("/orgs/{org}"), None).await.0, 204);
    assert_eq!(count().await, 0, "the deleted organisation's binding is gone");
}

#[tokio::test]
async fn a_change_answers_with_its_version_and_where_it_is_applied() {
    let gw = TestGw::start(None).await;
    let r = gw
        .http
        .post(format!("{}/api/v1/orgs", gw.admin))
        .bearer_auth(&gw.token)
        .json(&json!({ "name": "acme" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let version: i64 = r.headers()["x-gw-config-version"].to_str().unwrap().parse().unwrap();
    assert_eq!(r.headers()["x-gw-applied"], "1/1");
    // A read changes nothing and says nothing about versions.
    let r = gw.http.get(format!("{}/api/v1/orgs", gw.admin)).bearer_auth(&gw.token).send().await.unwrap();
    assert!(r.headers().get("x-gw-config-version").is_none());
    let (st, v) = gw.api("GET", &format!("/system/config-versions/{version}"), None).await;
    assert_eq!((st, v["applied"].as_u64(), v["of"].as_u64()), (200, Some(1), Some(1)), "{v}");
}

#[tokio::test]
async fn lists_filter_sort_and_page() {
    let gw = TestGw::start(None).await;
    for name in ["delta", "alpha", "charlie", "bravo", "echo"] {
        gw.create("/orgs", json!({ "name": name })).await;
    }
    // By name, newest letter first, two at a time, only names with an "a".
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let after = cursor.as_ref().map(|c| format!("&cursor={c}")).unwrap_or_default();
        let (st, page) = gw.api("GET", &format!("/orgs?sort=name&order=desc&q=A&limit=2{after}"), None).await;
        assert_eq!(st, 200, "{page}");
        seen.extend(page["items"].as_array().unwrap().iter().map(|o| o["name"].as_str().unwrap().to_owned()));
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    assert_eq!(seen, ["delta", "charlie", "bravo", "alpha"], "echo has no a; _system has none either");
    let (st, _) = gw.api("GET", "/orgs?sort=colour", None).await;
    assert_eq!(st, 400);
    let (_, page) = gw.api("GET", "/orgs?status=disabled", None).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn updates_name_their_version_and_harmful_changes_can_be_tried_first() {
    let gw = TestGw::start(None).await;
    let org = gw.create("/orgs", json!({ "name": "acme" })).await.id;
    // Without a version an update is refused; a stale one is a conflict.
    let raw =
        |body: Value| gw.http.put(format!("{}/api/v1/orgs/{org}", gw.admin)).bearer_auth(&gw.token).json(&body).send();
    assert_eq!(raw(json!({ "name": "acme", "display_name": "A" })).await.unwrap().status(), 428);
    let (_, cur) = gw.api("GET", &format!("/orgs/{org}"), None).await;
    let v = cur["version"].as_i64().unwrap();
    assert_eq!(raw(json!({ "name": "acme", "display_name": "A", "version": v })).await.unwrap().status(), 200);
    assert_eq!(raw(json!({ "name": "acme", "display_name": "B", "version": v })).await.unwrap().status(), 409);

    // A dry run of an update answers the object as it would be, and changes nothing.
    let (st, dry) = gw
        .api("PUT", &format!("/orgs/{org}?dry_run=true"), Some(json!({ "name": "acme", "display_name": "Dry" })))
        .await;
    assert_eq!(
        (st, dry["applied"].as_bool(), dry["object"]["display_name"].as_str()),
        (200, Some(false), Some("Dry")),
        "{dry}"
    );
    let (_, now) = gw.api("GET", &format!("/orgs/{org}"), None).await;
    assert_eq!(now["display_name"], "A");

    // A dry run of a deletion says what it would do; the object stays.
    let provider =
        gw.create("/providers", json!({ "name": "p", "family": "openai", "base_url": "https://x.example" })).await.id;
    let (st, dry) = gw.api("DELETE", &format!("/providers/{provider}?dry_run=true"), None).await;
    assert_eq!((st, dry["applied"].as_bool()), (200, Some(false)), "{dry}");
    assert!(dry["effects"].to_string().contains("/proxy/p/"), "{dry}");
    assert_eq!(gw.api("GET", &format!("/providers/{provider}"), None).await.0, 200);

    // And of revoking a key.
    let ws = gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "w" })).await.id;
    let key = gw.create(&format!("/orgs/{org}/workspaces/{ws}/keys"), json!({ "name": "k" })).await.id;
    let (st, dry) = gw.api("POST", &format!("/orgs/{org}/workspaces/{ws}/keys/{key}/revoke?dry_run=true"), None).await;
    assert_eq!((st, dry["object"]["status"].as_str()), (200, Some("revoked")), "{dry}");
    let (_, k) = gw.api("GET", &format!("/orgs/{org}/workspaces/{ws}/keys/{key}"), None).await;
    assert_eq!(k["status"], "active");

    // A change without a dry run is refused when one is asked for, not made.
    let credential =
        gw.create(&format!("/providers/{provider}/credentials"), json!({ "secret": "sk-test-1234567890" })).await.id;
    for q in ["dry_run=true", "dry_run=1", "dry_run=false"] {
        let (st, v) = gw.api("DELETE", &format!("/providers/{provider}/credentials/{credential}?{q}"), None).await;
        assert_eq!(st, 422, "{q}: {v}");
    }
    let (st, v) = gw.api("PUT", "/settings?dry_run=true", Some(json!({}))).await;
    assert_eq!(st, 422, "{v}");
    let (_, listed) = gw.api("GET", &format!("/providers/{provider}/credentials"), None).await;
    assert_eq!(listed["items"].as_array().unwrap().len(), 1, "the credential is still there");

    // The global settings and the price job are versioned like every other object.
    for path in ["/settings", "/price-job"] {
        // Saved once, so that a stored object exists.
        let (_, first) = gw.api("GET", path, None).await;
        assert_eq!(gw.api("PUT", path, Some(first)).await.0, 200, "{path}");
        let (_, mut read) = gw.api("GET", path, None).await;
        let version = read["version"].as_i64();
        assert!(version.is_some(), "{path} answers its version: {read}");
        read.as_object_mut().unwrap().remove("version");
        let put = |body: Value| {
            let gw = &gw;
            async move {
                let r = gw
                    .http
                    .put(format!("{}/api/v1{path}", gw.admin))
                    .bearer_auth(&gw.token)
                    .json(&body)
                    .send()
                    .await
                    .unwrap();
                r.status().as_u16()
            }
        };
        if let Some(v) = version {
            assert_eq!(put(read.clone()).await, 428, "{path} without a version");
            let mut stale = read.clone();
            stale["version"] = json!(v - 1);
            assert_eq!(put(stale).await, 409, "{path} with an old version");
            let mut current = read.clone();
            current["version"] = json!(v);
            assert_eq!(put(current).await, 200, "{path} with the version read");
        }
    }
}

#[tokio::test]
async fn organisations_and_workspaces_manage_their_own_provider_keys() {
    let gw = TestGw::start(None).await;
    let a = gw.create("/orgs", json!({ "name": "org-a" })).await.id;
    let b = gw.create("/orgs", json!({ "name": "org-b" })).await.id;
    let ws_a = gw.create(&format!("/orgs/{a}/workspaces"), json!({ "name": "one" })).await.id;
    let ws_b = gw.create(&format!("/orgs/{b}/workspaces"), json!({ "name": "one" })).await.id;
    let provider =
        gw.create("/providers", json!({ "name": "p", "family": "openai", "base_url": "https://x.example" })).await.id;
    let mut tokens = std::collections::HashMap::new();
    for (name, role, scope) in [
        ("orgadmin", "org_admin", json!({ "type": "organisation", "id": a })),
        ("wsadmin", "workspace_admin", json!({ "type": "workspace", "id": ws_a })),
        ("viewer", "viewer", json!({ "type": "organisation", "id": a })),
    ] {
        let email = format!("{name}@example.test");
        let user = gw.create("/users", json!({ "email": email, "password": "a long password 1" })).await.id;
        gw.create("/role-bindings", json!({ "principal": user, "role": role, "scope": scope })).await;
        tokens.insert(name, gw.login(&email, "a long password 1").await);
    }
    let path = format!("/providers/{provider}/credentials");
    let add = |who: &'static str, scope: Value| {
        let token = tokens[who].clone();
        let (gw, path) = (&gw, path.clone());
        async move {
            gw.api_as(
                &token,
                "POST",
                &path,
                Some(json!({ "label": who, "secret": "sk-test-1234567890", "scope": scope })),
            )
            .await
        }
    };
    let org_scope = |id: &str| json!({ "type": "organisation", "id": id });
    let ws_scope = |id: &str| json!({ "type": "workspace", "id": id });

    // Each admin adds keys for what they administer, and nothing else.
    let (st, org_key) = add("orgadmin", org_scope(&a)).await;
    assert_eq!(st, 201, "{org_key}");
    assert!(org_key.get("secret").is_none(), "the secret never comes back: {org_key}");
    assert_eq!(add("orgadmin", ws_scope(&ws_a)).await.0, 201, "an organisation admin keeps its workspaces' keys");
    assert_eq!(add("orgadmin", org_scope(&b)).await.0, 403);
    assert_eq!(add("orgadmin", ws_scope(&ws_b)).await.0, 403);
    assert_eq!(add("orgadmin", json!({ "type": "global" })).await.0, 403, "the shared key is the platform's");
    assert_eq!(add("wsadmin", ws_scope(&ws_a)).await.0, 201);
    assert_eq!(add("wsadmin", org_scope(&a)).await.0, 403);
    assert_eq!(add("viewer", org_scope(&a)).await.0, 403);
    gw.create(&path, json!({ "label": "platform-b", "secret": "sk-test-0987654321", "scope": org_scope(&b) })).await;

    // Each sees the keys it may manage: the organisation admin its own and its workspaces'.
    let labels = |who: &'static str| {
        let token = tokens[who].clone();
        let (gw, path) = (&gw, path.clone());
        async move {
            let (st, v) = gw.api_as(&token, "GET", &path, None).await;
            assert_eq!(st, 200, "{v}");
            let mut l: Vec<String> =
                v["items"].as_array().unwrap().iter().map(|c| c["label"].as_str().unwrap().to_owned()).collect();
            l.sort();
            l
        }
    };
    assert_eq!(labels("orgadmin").await, ["orgadmin", "orgadmin", "wsadmin"]);
    assert_eq!(labels("wsadmin").await, ["orgadmin", "wsadmin"]);
    assert!(labels("viewer").await.is_empty());

    // Disabling and deleting follow the same line.
    let id = org_key["id"].as_str().unwrap();
    let wsadmin = tokens["wsadmin"].clone();
    let state = json!({ "state": "disabled" });
    assert_eq!(gw.api_as(&wsadmin, "POST", &format!("{path}/{id}/state"), Some(state.clone())).await.0, 403);
    assert_eq!(gw.api_as(&tokens["orgadmin"], "POST", &format!("{path}/{id}/state"), Some(state)).await.0, 200);
    assert_eq!(gw.api_as(&wsadmin, "DELETE", &format!("{path}/{id}"), None).await.0, 403);
    assert_eq!(gw.api_as(&tokens["orgadmin"], "DELETE", &format!("{path}/{id}"), None).await.0, 204);
}
