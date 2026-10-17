//! Stage 6: deleting an account removes everything stored about the person.

mod common;

use common::{Browser, TestId};
use pacod_id::authz::bindings::{self, NewBinding, Protect};
use pacod_id_authz::{Manifest, ScopeRef};
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::Row;

const CORE: &str = "a-secret-for-the-gateway-core-0123456789";

fn manifest(name: &str) -> Value {
    let text = std::fs::read_to_string(format!("{}/tests/fixtures/{name}.yaml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    serde_json::to_value(Manifest::from_yaml(&text).unwrap().source()).unwrap()
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
    r.json::<Value>().await.unwrap()["access_token"].as_str().unwrap().to_owned()
}

async fn facade(id: &TestId, b: &mut Browser, email: &str) -> (String, Value) {
    let (code, verifier) = id.sign_in(b, email).await;
    let (_, t) = id.redeem(&code, &verifier).await;
    (exchange(id, t["access_token"].as_str().unwrap()).await, t)
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

/// Every text and JSON column of every table, with the rows that mention `needle`.
async fn mentions(id: &TestId, needle: &str) -> Vec<(String, String, i64)> {
    let columns = sqlx::query(
        "SELECT table_name, column_name FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations'
           AND data_type IN ('text', 'character varying', 'jsonb', 'ARRAY')
         ORDER BY table_name, column_name",
    )
    .fetch_all(id.store().pool())
    .await
    .unwrap();
    let mut found = Vec::new();
    for c in columns {
        let (table, column): (String, String) = (c.get("table_name"), c.get("column_name"));
        let n: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE {column}::text LIKE '%' || $1 || '%'"))
                .bind(needle)
                .fetch_one(id.store().pool())
                .await
                .unwrap();
        if n > 0 {
            found.push((table, column, n));
        }
    }
    found
}

#[tokio::test]
async fn deleting_an_account_leaves_nothing_of_the_person() {
    let id = TestId::start().await;
    id.service_secret("gw-core", CORE).await;
    let put = |path: &'static str, body: Value| id.internal_call(Method::PUT, path, "gw-core", CORE, Some(body));
    put("/internal/v1/catalog/gw", json!({ "manifest": manifest("gw") })).await;
    put("/internal/v1/organisations/org_1", json!({ "slug": "acme" })).await;
    put("/internal/v1/organisations/org_2", json!({ "slug": "other" })).await;
    let root = common::account(&id, "root@example.com").await.id;
    bind(&id, &root, "gw.platform_admin", ScopeRef::platform()).await;
    let bob = common::account(&id, "bob@example.com").await.id;
    let vic = common::account(&id, "vic@example.com").await;
    let v_id = vic.id.clone();
    bind(&id, &v_id, "gw.viewer", ScopeRef::organisation("org_1")).await;
    bind(&id, &v_id, "gw.org_admin", ScopeRef::organisation("org_2")).await;
    bind(&id, &v_id, "id.admin", ScopeRef::platform()).await;

    // Vic's traces: sessions, grants, a device, a failed sign-in, links, consents, a Google
    // link, an id in the gateway, mail, an invitation to the address, and things Vic did.
    let mut vb = Browser::new(&id);
    let (vic_t, _) = facade(&id, &mut vb, "vic@example.com").await;
    let mut other = Browser::new(&id);
    let (_, ch) = common::pkce();
    let flow =
        common::param(&other.get(&common::authorize_url(&id, &ch, &[])).await.location.unwrap(), "flow").unwrap();
    other
        .post(
            &format!("{}/ui-api/v1/flows/{flow}/password", id.public),
            json!({ "email": "vic@example.com", "password": "wrong" }),
        )
        .await;
    id.store().consent(&v_id, "gateway", "terms", "1").await.unwrap();
    id.sql(&format!(
        "INSERT INTO account_external_ids (source, external_id, account_id) VALUES ('gw', 'usr_old_vic', '{v_id}')"
    ))
    .await;
    id.sql(&format!(
        "INSERT INTO federated_identities (provider, provider_subject, account_id, email_at_link) VALUES ('google', '1234', '{v_id}', 'vic@example.com')"
    ))
    .await;
    id.sql(&format!(
        "INSERT INTO throttle (bucket, key, window_start, count) VALUES ('login_device', 'abcd:{v_id}', now(), 1)"
    ))
    .await;
    let (s, v) = call(
        &id,
        &vic_t,
        Method::PUT,
        &format!("/admin/v1/scopes/organisation/org_2/members/{bob}"),
        Some(json!({ "role": "gw.viewer" })),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = call(
        &id,
        &vic_t,
        Method::POST,
        "/admin/v1/invitations",
        Some(json!({ "email": "zoe@example.com", "role": "gw.viewer", "scope": "organisation:org_2" })),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let mut rb = Browser::new(&id);
    let (root_t, _) = facade(&id, &mut rb, "root@example.com").await;
    let (s, _) = call(
        &id,
        &root_t,
        Method::POST,
        "/admin/v1/invitations",
        Some(json!({ "email": "vic@example.com", "role": "gw.viewer", "scope": "organisation:org_1" })),
    )
    .await;
    assert_eq!(s, 201);
    // A platform admin, under the realm policy, does not reach an identity admin.
    let (s, v) = call(&id, &root_t, Method::POST, &format!("/admin/v1/accounts/{v_id}/password-reset"), None).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (403, Some("holds_more_rights")));
    let (s, _) = call(&id, &vic_t, Method::POST, &format!("/admin/v1/accounts/{v_id}/password-reset"), None).await;
    assert_eq!(s, 202, "a link and a mail waiting");
    assert!(!mentions(&id, &v_id).await.is_empty());

    // The last identity admin stays.
    let (s, v) = call(&id, &vic_t, Method::DELETE, &format!("/admin/v1/accounts/{v_id}"), None).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (409, Some("last_holder")), "{v}");
    bind(&id, &root, "id.admin", ScopeRef::platform()).await;
    let (s, v) = call(&id, &root_t, Method::DELETE, &format!("/admin/v1/accounts/{v_id}"), None).await;
    assert_eq!(s, 200, "{v}");

    // Nothing of Vic is left, except the tombstone until the gateway confirms, and the feed's
    // own history (7 days) that tells the products.
    let allowed = |t: &str| matches!(t, "account_deletions" | "authz_changes" | "backchannel_outbox");
    let left: Vec<_> = mentions(&id, &v_id).await.into_iter().filter(|(t, _, _)| !allowed(t)).collect();
    assert!(left.is_empty(), "rows that still name the account: {left:?}");
    let left: Vec<_> = mentions(&id, "vic@example.com").await;
    assert!(left.is_empty(), "rows that still hold the address: {left:?}");
    let event = sqlx::query("SELECT kind, payload FROM authz_changes WHERE version = $1")
        .bind(v["seq"].as_i64().unwrap())
        .fetch_one(id.store().pool())
        .await
        .unwrap();
    assert_eq!(event.get::<String, _>("kind"), "principal.deleted");
    assert_eq!(event.get::<Value, _>("payload")["external_ids"], json!({ "gw": "usr_old_vic" }));
    // What Vic did stays, done by "deleted account".
    let by: Vec<String> = sqlx::query_scalar("SELECT DISTINCT created_by FROM role_bindings WHERE principal_id = $1")
        .bind(&bob)
        .fetch_all(id.store().pool())
        .await
        .unwrap();
    assert_eq!(by, ["deleted account"]);
    let invited_by: String = sqlx::query_scalar("SELECT invited_by FROM invitations WHERE email = 'zoe@example.com'")
        .fetch_one(id.store().pool())
        .await
        .unwrap();
    assert_eq!(invited_by, "deleted account");
    // Vic's facade token, sessions and refresh tokens are gone with the account.
    let (s, _) = call(&id, &vic_t, Method::GET, "/admin/v1/bindings", None).await;
    assert_eq!(s, 401);

    // The gateway finds the deletion in its snapshot and confirms it.
    let (_, snap) =
        id.internal_call(Method::GET, "/internal/v1/authz/snapshot?product=gw", "gw-core", CORE, None).await;
    assert_eq!(snap["deleted"], json!([v_id]));
    let (s, _) = id
        .internal_call(Method::POST, &format!("/internal/v1/deletions/{v_id}/done?product=shop"), "gw-core", CORE, None)
        .await;
    assert_eq!(s, 403, "for its own product only");
    let (s, _) = id
        .internal_call(Method::POST, &format!("/internal/v1/deletions/{v_id}/done?product=gw"), "gw-core", CORE, None)
        .await;
    assert_eq!(s, 204);
    let tombstones: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_deletions").fetch_one(id.store().pool()).await.unwrap();
    assert_eq!(tombstones, 0);
    id.running.stop().await;
}
