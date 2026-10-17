//! Stage 6: the catalogue, organisations and scopes, bindings and the last-admin rule, the feed,
//! and the bootstrap admin.

mod common;

use std::time::{Duration, Instant};

use common::{CLIENT_SECRET, TestId};
use pacod_id::authz::bindings::{self, ChangeError, NewBinding, Protect};
use pacod_id_authz::{Manifest, ScopeRef, Subject};
use reqwest::Method;
use serde_json::{Value, json};

const CORE: &str = "a-secret-for-the-gateway-core-0123456789";
const API: &str = "a-secret-for-shop-s-api-service-0123456789";

fn manifest(name: &str) -> Value {
    let text = std::fs::read_to_string(format!("{}/tests/fixtures/{name}.yaml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    serde_json::to_value(Manifest::from_yaml(&text).unwrap().source()).unwrap()
}

/// A pacod-id with gw-core and shop-api (shop's service) let in.
async fn start() -> TestId {
    let db = common::fresh_database().await;
    let id = TestId::start_with(
        &db,
        |_| {
            r#"
[[client]]
id = "shop-api"
realm = "gateway"
kind = "service"
product = "shop"
audience = "shop-api"
"#
            .to_owned()
        },
        |_| {},
    )
    .await;
    id.service_secret("gw-core", CORE).await;
    id.service_secret("shop-api", API).await;
    id
}

async fn core(id: &TestId, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
    id.internal_call(method, path, "gw-core", CORE, body).await
}

/// gw's catalogue, an organisation org_1 with a workspace ws_1.
async fn gateway(id: &TestId) {
    let (s, v) = core(id, Method::PUT, "/internal/v1/catalog/gw", Some(json!({ "manifest": manifest("gw") }))).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = core(
        id,
        Method::PUT,
        "/internal/v1/organisations/org_1",
        Some(json!({ "slug": "acme", "display_name": "Acme" })),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = core(
        id,
        Method::PUT,
        "/internal/v1/scopes/gw.workspace/ws_1",
        Some(json!({ "parent": "organisation:org_1", "display_name": "Prod" })),
    )
    .await;
    assert_eq!(s, 200, "{v}");
}

async fn bind(id: &TestId, principal: &str, role: &str, scope: ScopeRef) -> Result<bindings::Applied, ChangeError> {
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
    let r = bindings::apply(&mut tx, &st.catalog.get(), Protect::Skip, &[], &add).await;
    if r.is_ok() {
        tx.commit().await.unwrap();
    }
    r
}

/// Removes a binding as `actor` would, under the last-admin rule.
async fn unbind(id: &TestId, binding: &str, actor: &Subject) -> Result<bindings::Applied, ChangeError> {
    let st = &id.running.state;
    let mut tx = st.store.begin().await.unwrap();
    pacod_id::store::authz::lock_head(&mut tx).await.unwrap();
    let row = pacod_id::store::authz::binding(&mut tx, binding).await.unwrap().unwrap();
    let r = bindings::apply(&mut tx, &st.catalog.get(), Protect::As(actor), &[row], &[]).await;
    if r.is_ok() {
        tx.commit().await.unwrap();
    }
    r
}

#[tokio::test]
async fn products_push_their_catalogue_and_only_their_own() {
    let id = start().await;
    let gw = json!({ "manifest": manifest("gw") });
    let (s, v) =
        id.internal_call(Method::PUT, "/internal/v1/catalog/gw", "console", CLIENT_SECRET, Some(gw.clone())).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (403, Some("not_a_service")), "a BFF pushes nothing");
    let (s, v) =
        core(&id, Method::PUT, "/internal/v1/catalog/shop", Some(json!({ "manifest": manifest("shop") }))).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (403, Some("wrong_product")));
    let (s, v) = core(&id, Method::PUT, "/internal/v1/catalog/id", Some(json!({ "manifest": manifest("gw") }))).await;
    assert_eq!(s, 403, "{v}");

    let (s, first) = core(&id, Method::PUT, "/internal/v1/catalog/gw", Some(gw.clone())).await;
    assert_eq!((s, first["version"].as_i64(), first["changed"].as_bool()), (200, Some(1), Some(true)), "{first}");
    let (_, again) = core(&id, Method::PUT, "/internal/v1/catalog/gw", Some(gw.clone())).await;
    assert_eq!(
        (again["version"].as_i64(), again["changed"].as_bool()),
        (Some(1), Some(false)),
        "the same manifest changes nothing"
    );
    assert_eq!(again["seq"], first["seq"]);
    assert!(id.running.state.catalog.get().manifest("gw").is_some());

    let mut bad = manifest("gw");
    bad["grant"] = json!({});
    let (s, v) = core(&id, Method::PUT, "/internal/v1/catalog/gw", Some(json!({ "manifest": bad }))).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (422, Some("missing_grant_default")), "{v}");
    let (s, v) = core(&id, Method::GET, "/internal/v1/catalog/gw", None).await;
    assert_eq!((s, v["version"].as_i64()), (200, Some(1)));
    id.running.stop().await;
}

#[tokio::test]
async fn a_dropped_role_in_use_needs_a_migration() {
    let id = start().await;
    gateway(&id).await;
    let ada = common::account(&id, "ada@example.com").await;
    bind(&id, &ada.id, "gw.auditor", ScopeRef::organisation("org_1")).await.unwrap();
    bind(&id, &ada.id, "gw.viewer", ScopeRef::of("gw.workspace", "ws_1")).await.unwrap();
    let mut next = manifest("gw");
    next["roles"].as_object_mut().unwrap().remove("auditor");
    next["roles"]["viewer"]["at"] = json!(["platform", "organisation"]);
    let (s, v) = core(&id, Method::PUT, "/internal/v1/catalog/gw", Some(json!({ "manifest": next }))).await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(v["error"]["details"]["roles"], json!(["gw.auditor", "gw.viewer"]));
    let (s, v) = core(
        &id,
        Method::PUT,
        "/internal/v1/catalog/gw",
        Some(json!({ "manifest": next, "migrate": { "gw.auditor": "gw.viewer", "gw.viewer": null } })),
    )
    .await;
    assert_eq!((s, v["version"].as_i64()), (200, Some(2)), "{v}");
    let held: Vec<(String, String)> =
        sqlx::query_as("SELECT role, scope_type FROM role_bindings WHERE principal_id = $1 ORDER BY role")
            .bind(&ada.id)
            .fetch_all(id.store().pool())
            .await
            .unwrap();
    assert_eq!(held, [("gw.viewer".to_owned(), "organisation".to_owned())]);
    // A scope type with registered scopes stays.
    let mut flat = next.clone();
    flat.as_object_mut().unwrap().remove("scope_types");
    flat["roles"].as_object_mut().unwrap().remove("workspace_admin");
    let (s, v) = core(&id, Method::PUT, "/internal/v1/catalog/gw", Some(json!({ "manifest": flat }))).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (409, Some("scope_type_in_use")), "{v}");
    id.running.stop().await;
}

#[tokio::test]
async fn organisations_and_scopes() {
    let id = start().await;
    let (s, v) = core(
        &id,
        Method::PUT,
        "/internal/v1/scopes/gw.workspace/ws_0",
        Some(json!({ "parent": "organisation:org_1" })),
    )
    .await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (409, Some("catalog_missing")));
    gateway(&id).await;
    let put = |path: &'static str, body: Value| core(&id, Method::PUT, path, Some(body));
    let (s, v) = put("/internal/v1/scopes/gw.workspace/ws_2", json!({ "parent": "organisation:org_9" })).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (409, Some("scope_unknown")));
    let (s, v) = put("/internal/v1/scopes/gw.workspace/ws_2", json!({ "parent": "platform" })).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (422, Some("bad_parent")));
    let (s, v) = put("/internal/v1/scopes/shop.profile/p_1", json!({ "parent": "platform" })).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (403, Some("wrong_product")));
    let (s, v) = put("/internal/v1/scopes/gw.workspace/a b", json!({ "parent": "organisation:org_1" })).await;
    assert_eq!(s, 422, "{v}");
    let (_, v) = put("/internal/v1/organisations/org_2", json!({ "slug": "other" })).await;
    let before = v["seq"].as_i64().unwrap();
    let (s, v) = put("/internal/v1/scopes/gw.workspace/ws_1", json!({ "parent": "organisation:org_2" })).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (409, Some("parent_changed")));
    let (s, v) = put("/internal/v1/organisations/org_3", json!({ "slug": "acme" })).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (409, Some("slug_taken")));
    // Registering again with the same values is no change.
    let (_, v) = put("/internal/v1/organisations/org_2", json!({ "slug": "other" })).await;
    assert_eq!(v["seq"].as_i64(), Some(before));

    // shop registers the same organisation; the gateway's delete only unregisters it.
    let (s, _) = id
        .internal_call(
            Method::PUT,
            "/internal/v1/organisations/org_1",
            "shop-api",
            API,
            Some(json!({ "slug": "acme" })),
        )
        .await;
    assert_eq!(s, 200);
    let ada = common::account(&id, "ada@example.com").await;
    bind(&id, &ada.id, "gw.org_admin", ScopeRef::organisation("org_1")).await.unwrap();
    bind(&id, &ada.id, "gw.workspace_admin", ScopeRef::of("gw.workspace", "ws_1")).await.unwrap();
    // Pending invitations at the organisation: the gateway's, and one of another product's.
    for (n, role) in [(1, "gw.org_member"), (2, "shop.member")] {
        sqlx::query(
            "INSERT INTO invitations (id, token_hash, email, role, scope_type, scope_id, realm, expires_at)
             VALUES ($1, $2, $3, $4, 'organisation', 'org_1', 'gateway', now() + interval '1 day')",
        )
        .bind(format!("inv_{n}"))
        .bind(vec![n as u8; 32])
        .bind(format!("guest{n}@example.com"))
        .bind(role)
        .execute(id.store().pool())
        .await
        .unwrap();
    }
    let (s, v) = core(&id, Method::DELETE, "/internal/v1/organisations/org_1?product=gw", None).await;
    assert_eq!(s, 200);
    // The feed tells followers the organisation is now shop's only, as a snapshot would.
    let (kind, payload): (String, serde_json::Value) =
        sqlx::query_as("SELECT kind, payload FROM authz_changes WHERE version = $1")
            .bind(v["seq"].as_i64().unwrap())
            .fetch_one(id.store().pool())
            .await
            .unwrap();
    assert_eq!(
        (kind.as_str(), &payload["products"], &payload["status"]),
        ("scope.upserted", &json!(["shop"]), &json!("active"))
    );
    // What the gateway put there left with it: its workspace, its roles, its invitation.
    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM authz_changes WHERE version > $1 ORDER BY version")
        .bind(v["seq"].as_i64().unwrap() - 4)
        .fetch_all(id.store().pool())
        .await
        .unwrap();
    assert_eq!(kinds, ["binding.removed", "scope.deleted", "binding.removed", "scope.upserted"]);
    let (bindings, workspaces, invitations): (i64, i64, Vec<String>) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM role_bindings), (SELECT count(*) FROM scopes WHERE id = 'ws_1'),
                (SELECT array_agg(role) FROM invitations)",
    )
    .fetch_one(id.store().pool())
    .await
    .unwrap();
    assert_eq!((bindings, workspaces, invitations), (0, 0, vec!["shop.member".to_owned()]));
    let (s, v) =
        id.internal_call(Method::DELETE, "/internal/v1/organisations/org_1?product=gw", "shop-api", API, None).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (403, Some("wrong_product")));
    let (s, v) = id.internal_call(Method::DELETE, "/internal/v1/organisations/org_1", "shop-api", API, None).await;
    assert_eq!(s, 200, "{v}");
    // Gone, with the invitations to it.
    let left: i64 =
        sqlx::query_scalar("SELECT (SELECT count(*) FROM organisations) + (SELECT count(*) FROM invitations)")
            .fetch_one(id.store().pool())
            .await
            .unwrap();
    assert_eq!(left, 1, "org_2 stays");
    let kind: String = sqlx::query_scalar("SELECT kind FROM authz_changes WHERE version = $1")
        .bind(v["seq"].as_i64().unwrap())
        .fetch_one(id.store().pool())
        .await
        .unwrap();
    assert_eq!(kind, "scope.deleted");
    // Deleting again: nothing to do.
    let (s, _) = core(&id, Method::DELETE, "/internal/v1/organisations/org_1", None).await;
    assert_eq!(s, 200);

    // Built-in scopes stay.
    put("/internal/v1/organisations/_system", json!({ "system": true })).await;
    put("/internal/v1/scopes/gw.workspace/_system", json!({ "parent": "organisation:_system", "system": true })).await;
    let (s, v) = core(&id, Method::DELETE, "/internal/v1/scopes/gw.workspace/_system", None).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (409, Some("system_scope")));
    let (s, _) = core(&id, Method::DELETE, "/internal/v1/organisations/_system", None).await;
    assert_eq!(s, 409);
    id.running.stop().await;
}

#[tokio::test]
async fn the_last_admin_stays_even_when_two_leave_at_once() {
    let id = start().await;
    gateway(&id).await;
    let ada = common::account(&id, "ada@example.com").await;
    let bob = common::account(&id, "bob@example.com").await;
    let a = bind(&id, &ada.id, "gw.org_admin", ScopeRef::organisation("org_1")).await.unwrap().added[0].clone();
    let b = bind(&id, &bob.id, "gw.org_admin", ScopeRef::organisation("org_1")).await.unwrap().added[0].clone();
    // Unknown scopes and principals are refused.
    assert!(matches!(
        bind(&id, &ada.id, "gw.viewer", ScopeRef::of("gw.workspace", "ws_9")).await,
        Err(ChangeError::ScopeUnknown(_))
    ));
    assert!(matches!(
        bind(&id, "usr_nobody", "gw.viewer", ScopeRef::organisation("org_1")).await,
        Err(ChangeError::PrincipalUnknown(_))
    ));

    let nobody = Subject::user(vec![]);
    for _ in 0..5 {
        let (ra, rb) = tokio::join!(unbind(&id, &a, &nobody), unbind(&id, &b, &nobody));
        let ok = [ra.is_ok(), rb.is_ok()];
        assert_eq!(ok.iter().filter(|x| **x).count(), 1, "exactly one may go: {ra:?} {rb:?}");
        assert!(matches!(if ra.is_ok() { rb } else { ra }, Err(ChangeError::LastHolder(_))));
        // Put the one that went back for the next round.
        let gone = if ok[0] { &ada.id } else { &bob.id };
        let again = bind(&id, gone, "gw.org_admin", ScopeRef::organisation("org_1")).await.unwrap().added[0].clone();
        if ok[0] {
            id.sql(&format!("UPDATE role_bindings SET id = '{a}' WHERE id = '{again}'")).await;
        } else {
            id.sql(&format!("UPDATE role_bindings SET id = '{b}' WHERE id = '{again}'")).await;
        }
    }
    // A platform admin may leave an organisation without one (the manifest's bypass); nobody
    // removes the last platform admin.
    let root = Subject::user(vec![pacod_id_authz::Binding::new("gw.platform_admin", vec![ScopeRef::platform()])]);
    unbind(&id, &a, &root).await.unwrap();
    unbind(&id, &b, &root).await.unwrap();
    let p = bind(&id, &ada.id, "gw.platform_admin", ScopeRef::platform()).await.unwrap().added[0].clone();
    assert!(matches!(unbind(&id, &p, &root).await, Err(ChangeError::LastHolder(_))));
    // A disabled holder does not count.
    bind(&id, &bob.id, "gw.platform_admin", ScopeRef::platform()).await.unwrap();
    id.sql(&format!("UPDATE accounts SET status = 'disabled' WHERE id = '{}'", bob.id)).await;
    assert!(matches!(unbind(&id, &p, &root).await, Err(ChangeError::LastHolder(_))));
    id.running.stop().await;
}

async fn changes(id: &TestId, client: (&str, &str), product: &str, after: i64, wait: u64) -> (u16, Value) {
    id.internal_call(
        Method::GET,
        &format!("/internal/v1/authz/changes?product={product}&after={after}&wait={wait}"),
        client.0,
        client.1,
        None,
    )
    .await
}

#[tokio::test]
async fn the_snapshot_and_the_changes() {
    let id = start().await;
    gateway(&id).await;
    let (s, v) = id
        .internal_call(
            Method::PUT,
            "/internal/v1/catalog/shop",
            "shop-api",
            API,
            Some(json!({ "manifest": manifest("shop") })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let ada = common::account(&id, "ada@example.com").await;
    bind(&id, &ada.id, "gw.workspace_admin", ScopeRef::of("gw.workspace", "ws_1")).await.unwrap();
    bind(&id, &ada.id, "shop.admin", ScopeRef::platform()).await.unwrap();
    bind(&id, &ada.id, "id.auditor", ScopeRef::platform()).await.unwrap();

    let (s, v) = core(&id, Method::GET, "/internal/v1/authz/snapshot?product=shop", None).await;
    assert_eq!(
        (s, v["error"]["details"]["code"].as_str()),
        (403, Some("wrong_product")),
        "a product reads its own feed"
    );
    let (s, snap) = core(&id, Method::GET, "/internal/v1/authz/snapshot?product=gw", None).await;
    assert_eq!(s, 200, "{snap}");
    let roles: Vec<(&str, Vec<&str>)> = snap["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (b["role"].as_str().unwrap(), b["path"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect())
        })
        .collect();
    assert!(
        roles.contains(&("gw.workspace_admin", vec!["platform", "organisation:org_1", "gw.workspace:ws_1"])),
        "{roles:?}"
    );
    assert!(roles.contains(&("id.auditor", vec!["platform"])));
    assert!(!roles.iter().any(|(r, _)| r.starts_with("shop.")), "another product's roles are not sent");
    assert_eq!(snap["catalog"]["version"], 1);
    assert!(
        snap["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["type"] == "gw.workspace" && s["parent"] == "organisation:org_1")
    );
    let seq = snap["seq"].as_i64().unwrap();

    // Nothing new: an immediate answer without wait, the same seq.
    let (s, v) = changes(&id, ("gw-core", CORE), "gw", seq, 0).await;
    assert_eq!((s, v["seq"].as_i64(), v["events"].as_array().map(Vec::len)), (200, Some(seq), Some(0)));
    // A change for shop only moves the cursor; then gw's own and a session's come through.
    bind(&id, &ada.id, "shop.superuser", ScopeRef::platform()).await.unwrap();
    let (_, v) = changes(&id, ("gw-core", CORE), "gw", seq, 0).await;
    assert_eq!((v["seq"].as_i64(), v["events"].as_array().map(Vec::len)), (Some(seq + 1), Some(0)));
    let (_, v) = changes(&id, ("shop-api", API), "shop", seq, 0).await;
    assert_eq!(v["events"][0]["role"], "shop.superuser");

    let waiting = {
        let id_http = id.http.clone();
        let url = format!("{}/internal/v1/authz/changes?product=gw&after={}&wait=20", id.internal, seq + 1);
        tokio::spawn(async move {
            let started = Instant::now();
            let r = id_http.get(url).basic_auth("gw-core", Some(CORE)).send().await.unwrap();
            (started.elapsed(), r.json::<Value>().await.unwrap())
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    bind(&id, &ada.id, "gw.viewer", ScopeRef::organisation("org_1")).await.unwrap();
    let (took, v) = waiting.await.unwrap();
    assert!(took < Duration::from_secs(2), "the wait ends with the change: {took:?}");
    let e = &v["events"][0];
    assert_eq!(
        (e["kind"].as_str(), e["role"].as_str(), e["principal"].as_str()),
        (Some("binding.added"), Some("gw.viewer"), Some(ada.id.as_str()))
    );
    assert_eq!(e["path"], json!(["platform", "organisation:org_1"]));
    assert_eq!(v["seq"], e["seq"]);

    // Changes no longer kept: load the snapshot again.
    id.sql("DELETE FROM authz_changes WHERE version <= 3").await;
    let (s, v) = changes(&id, ("gw-core", CORE), "gw", 1, 0).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (410, Some("cursor_expired")), "{v}");
    let (s, _) = changes(&id, ("gw-core", CORE), "gw", 1_000_000, 0).await;
    assert_eq!(s, 410, "a cursor ahead of the head (a restored database) starts over too");
    id.running.stop().await;
}

#[tokio::test]
async fn followers_see_every_change_once_in_order_within_a_second() {
    let id = start().await;
    gateway(&id).await;
    let mut people = Vec::new();
    for i in 0..40 {
        people.push(common::account(&id, &format!("p{i}@example.com")).await.id);
    }
    let (_, snap) = core(&id, Method::GET, "/internal/v1/authz/snapshot?product=gw", None).await;
    let start_seq = snap["seq"].as_i64().unwrap();

    // A follower that long-polls and notes when each change reached it.
    let follower = {
        let http = id.http.clone();
        let base = id.internal.clone();
        tokio::spawn(async move {
            let mut after = start_seq;
            let mut seen: Vec<(i64, String, Instant)> = Vec::new();
            while seen.len() < 40 {
                let v: Value = http
                    .get(format!("{base}/internal/v1/authz/changes?product=gw&after={after}&wait=10"))
                    .basic_auth("gw-core", Some(CORE))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                let now = Instant::now();
                for e in v["events"].as_array().unwrap() {
                    seen.push((e["seq"].as_i64().unwrap(), e["principal"].as_str().unwrap().to_owned(), now));
                }
                after = v["seq"].as_i64().unwrap();
            }
            seen
        })
    };
    // Forty writers at once, each noting when its change committed.
    let mut writers = Vec::new();
    for p in people.clone() {
        let st = id.running.state.clone();
        writers.push(tokio::spawn(async move {
            let mut tx = st.store.begin().await.unwrap();
            pacod_id::store::authz::lock_head(&mut tx).await.unwrap();
            let add = [NewBinding {
                principal: p.clone(),
                role: "gw.viewer".into(),
                scope: ScopeRef::organisation("org_1"),
                source: "manual",
                created_by: "test".into(),
            }];
            bindings::apply(&mut tx, &st.catalog.get(), Protect::Skip, &[], &add).await.unwrap();
            tx.commit().await.unwrap();
            (p, Instant::now())
        }));
    }
    let mut committed = std::collections::BTreeMap::new();
    for w in writers {
        let (p, at) = w.await.unwrap();
        committed.insert(p, at);
    }
    let seen = tokio::time::timeout(Duration::from_secs(20), follower).await.expect("the follower sees all").unwrap();
    let seqs: Vec<i64> = seen.iter().map(|(s, _, _)| *s).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "in order: {seqs:?}");
    assert_eq!(seqs, ((start_seq + 1)..=(start_seq + 40)).collect::<Vec<_>>(), "none missed, none twice");
    let mut delays: Vec<Duration> = seen.iter().map(|(_, p, at)| at.saturating_duration_since(committed[p])).collect();
    delays.sort();
    let p99 = delays[(delays.len() * 99).div_ceil(100) - 1];
    assert!(p99 < Duration::from_secs(1), "p99 {p99:?}");
    id.running.stop().await;
}

#[tokio::test]
async fn the_bootstrap_admin() {
    let db = common::fresh_database().await;
    let admin = || pacod_id::bootstrap::BootstrapAdmin {
        email: "Root@Example.com".into(),
        password: Some("a long bootstrap passphrase".into()),
        roles: vec!["id.admin".into(), "gw.platform_admin".into()],
        realm: Some("gateway".into()),
    };
    let id = TestId::start_with(&db, |_| String::new(), |b| b.bootstrap_admin = Some(admin())).await;
    let root = id.store().account_by_email("root@example.com").await.unwrap().expect("made at start");
    assert!(root.is_verified());
    let held: Vec<(String, String)> =
        sqlx::query_as("SELECT role, source FROM role_bindings WHERE principal_id = $1 ORDER BY role")
            .bind(&root.id)
            .fetch_all(id.store().pool())
            .await
            .unwrap();
    assert_eq!(
        held,
        [("gw.platform_admin".into(), "bootstrap".into()), ("id.admin".into(), "bootstrap".into())],
        "the gw role waits for gw"
    );
    // The id role counts at once; the gw role once gw's catalogue arrives.
    let catalog = id.running.state.catalog.get();
    let mut conn = id.store().pool().acquire().await.unwrap();
    let subject = Subject::user(bindings::subject_bindings(&mut conn, &root.id).await.unwrap());
    assert!(catalog.allowed(&subject, "id.accounts.manage", &[ScopeRef::platform()]));
    id.service_secret("gw-core", CORE).await;
    core(&id, Method::PUT, "/internal/v1/catalog/gw", Some(json!({ "manifest": manifest("gw") }))).await;
    assert!(id.running.state.catalog.get().allowed(&subject, "gw.platform.manage", &[ScopeRef::platform()]));

    // A second instance, or a restart, makes nothing new.
    let second = TestId::start_with(&db, |_| String::new(), |b| b.bootstrap_admin = Some(admin())).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts").fetch_one(id.store().pool()).await.unwrap();
    assert_eq!(n, 1);
    second.running.stop().await;
    id.running.stop().await;

    // A password too short for a role holder stops the start.
    let db = common::fresh_database().await;
    let listeners = || {
        let bind = || {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.set_nonblocking(true).unwrap();
            l
        };
        pacod_id::Listeners { public: bind(), internal: bind(), metrics: bind() }
    };
    let mut boot = pacod_id::Bootstrap::for_tests(&db);
    boot.bootstrap_admin = Some(pacod_id::bootstrap::BootstrapAdmin { password: Some("tenletters".into()), ..admin() });
    let config = pacod_id::Config::from_toml(&common::config_for(1)).unwrap();
    let err = pacod_id::start(boot, config, listeners()).await.err().expect("refused");
    let text = format!("{err:#}");
    assert!(text.contains("PID_BOOTSTRAP_ADMIN_PASSWORD") && !text.contains("tenletters"), "{text}");
}

#[tokio::test]
async fn malformed_requests_answer_in_the_apis_shape() {
    let id = start().await;
    gateway(&id).await;
    for (method, path, body) in [
        (Method::PUT, "/internal/v1/organisations/org_9", Some(json!({ "slug": 7 }))),
        (Method::PUT, "/internal/v1/organisations/org_9", Some(json!({ "surprise": true }))),
        (Method::GET, "/internal/v1/authz/changes?product=gw", None),
    ] {
        let (s, v) = core(&id, method, path, body).await;
        assert!((400..500).contains(&s), "{path}: {s}");
        assert_eq!(v["error"]["details"]["code"], "bad_request", "{path}: {v}");
    }
    // Every error has a code to branch on.
    let (s, v) = core(&id, Method::GET, "/internal/v1/nothing-here", None).await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (404, Some("not_found")));
    id.running.stop().await;
}
