//! The identity outbox (identity concept 13.1): in OIDC mode the gateway's organisations and
//! workspaces become scopes at pacod-id (started in process), in order, with what pacod-id
//! refuses for good parked rather than holding up the rest.

mod common;
#[path = "common/id.rs"]
mod id;

use std::time::Duration;

use id::Id;
use pacod_id_authz::ScopeRef;
use serde_json::json;
use sqlx::PgPool;

struct World {
    id: Id,
    gw: common::TestGw,
    /// The gateway's database.
    db: PgPool,
}

/// pacod-id and a gateway in OIDC mode, signed in as a platform admin.
async fn world() -> World {
    let id = Id::start().await;
    let mut gw = id::gateway(&id, |_| {}).await;
    id.wait_for_gw_catalog().await;
    let ada = id.account("ada@example.com").await;
    id.bind(&ada, "gw.platform_admin", ScopeRef::platform()).await;
    gw.token = id.sign_in("ada@example.com").await["access_token"].as_str().unwrap().to_owned();
    for _ in 0..60 {
        if gw.api("GET", "/session", None).await.1["platform_admin"] == true {
            let db = PgPool::connect(&gw.database_url).await.unwrap();
            return World { id, gw, db };
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("ada's role never reached the gateway");
}

/// The organisation as pacod-id has it: slug, display name, products.
async fn organisation_at(pid: &Id, id: &str) -> Option<(String, String, Vec<String>)> {
    sqlx::query_as(
        "SELECT slug, display_name,
                    (SELECT array_agg(product ORDER BY product) FROM organisation_products WHERE organisation_id = o.id)
             FROM organisations o WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pid.store().pool())
    .await
    .unwrap()
}

/// Waits until `check` holds, for at most ten seconds.
async fn until<F: AsyncFn() -> bool>(what: &str, check: F) {
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("never: {what}");
}

/// The outbox deliverer's advisory lock (`store/idp.rs`): held, no round runs.
const DELIVERER: i64 = 0x6964_706f_7574_6278;

impl World {
    async fn organisation(&self, id: &str) -> Option<(String, String, Vec<String>)> {
        organisation_at(&self.id, id).await
    }

    /// The workspace as pacod-id has it: parent and display name.
    async fn workspace(&self, id: &str) -> Option<(String, String)> {
        sqlx::query_as(
            "SELECT parent_type || ':' || parent_id, display_name FROM scopes WHERE type = 'gw.workspace' AND id = $1",
        )
        .bind(id)
        .fetch_optional(self.id.store().pool())
        .await
        .unwrap()
    }

    /// Runs `sql` on the outbox between two delivery rounds.
    async fn between_rounds(&self, sql: &str) {
        let mut tx = self.db.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(DELIVERER).execute(&mut *tx).await.unwrap();
        sqlx::query(sql).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// The outbox's rows: target, attempts, parked, last error.
    async fn outbox(&self) -> Vec<(String, i32, bool, String)> {
        sqlx::query_as("SELECT target, attempts, parked_at IS NOT NULL, last_error FROM idp_outbox ORDER BY id")
            .fetch_all(&self.db)
            .await
            .unwrap()
    }

    async fn enqueue(&self, op: &str, target: &str, payload: serde_json::Value) {
        sqlx::query("INSERT INTO idp_outbox (op, target, payload) VALUES ($1, $2, $3)")
            .bind(op)
            .bind(target)
            .bind(payload)
            .execute(&self.db)
            .await
            .unwrap();
    }

    async fn stop(self) {
        self.gw.app.shutdown().await;
        self.id.running.stop().await;
    }
}

#[tokio::test]
async fn organisations_and_workspaces_follow_the_gateway() {
    let w = world().await;
    // The built-in ones are registered at start, by the sweep.
    until("the built-in organisation and workspace reach pacod-id", async || {
        w.organisation("org_system").await.is_some() && w.workspace("ws_system").await.is_some()
    })
    .await;
    let system: bool = sqlx::query_scalar("SELECT system FROM organisations WHERE id = 'org_system'")
        .fetch_one(w.id.store().pool())
        .await
        .unwrap();
    assert!(system);

    let org = w.gw.create("/orgs", json!({ "name": "acme", "display_name": "Acme" })).await.id;
    let ws =
        w.gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "prod", "display_name": "Prod" })).await.id;
    until("the organisation and its workspace reach pacod-id", async || {
        w.workspace(&ws).await == Some((format!("organisation:{org}"), "Prod".into()))
    })
    .await;
    assert_eq!(w.organisation(&org).await, Some(("acme".into(), "Acme".into(), vec!["gw".into()])));

    // A rename follows.
    let (s, v) =
        w.gw.api("PUT", &format!("/orgs/{org}"), Some(json!({ "name": "acme-corp", "display_name": "Acme Corp" })))
            .await;
    assert_eq!(s, 200, "{v}");
    until("the new name reaches pacod-id", async || {
        w.organisation(&org).await.is_some_and(|(slug, name, _)| slug == "acme-corp" && name == "Acme Corp")
    })
    .await;

    // A dry run queues nothing.
    let (s, v) = w.gw.api("DELETE", &format!("/orgs/{org}/workspaces/{ws}?dry_run=true"), None).await;
    assert_eq!((s, &v["applied"]), (200, &json!(false)), "{v}");
    until("the outbox is empty", async || w.outbox().await.is_empty()).await;
    assert!(w.workspace(&ws).await.is_some());

    // A role at the workspace goes when the workspace does, and the organisation after it.
    let bob = w.id.account("bob@example.com").await;
    w.id.bind(&bob, "gw.workspace_admin", ScopeRef::of("gw.workspace", &ws)).await;
    assert_eq!(w.gw.api("DELETE", &format!("/orgs/{org}/workspaces/{ws}"), None).await.0, 204);
    until("the workspace leaves pacod-id", async || w.workspace(&ws).await.is_none()).await;
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM role_bindings WHERE principal_id = $1")
        .bind(&bob)
        .fetch_one(w.id.store().pool())
        .await
        .unwrap();
    assert_eq!(held, 0);
    assert_eq!(w.gw.api("DELETE", &format!("/orgs/{org}"), None).await.0, 204);
    until("the organisation leaves pacod-id", async || w.organisation(&org).await.is_none()).await;
    until("everything was delivered", async || w.outbox().await.is_empty()).await;
    w.stop().await;
}

#[tokio::test]
async fn what_pacod_id_refuses_does_not_hold_up_the_rest() {
    let w = world().await;
    until("the sweep is done", async || w.workspace("ws_system").await.is_some()).await;

    // Refused for good (an id pacod-id does not take): parked; the row behind it goes through.
    w.enqueue("put_organisation", "organisation:bad id", json!({ "id": "bad id", "slug": "bad" })).await;
    w.enqueue("put_organisation", "organisation:org_after", json!({ "id": "org_after", "slug": "after" })).await;
    until("the row behind the refused one is delivered", async || w.organisation("org_after").await.is_some()).await;
    until("the round is saved", async || w.outbox().await.len() == 1).await;
    let rows = w.outbox().await;
    assert_eq!((rows[0].0.as_str(), rows[0].1, rows[0].2), ("organisation:bad id", 1, true));
    assert!(!rows[0].3.is_empty());

    // A workspace whose organisation pacod-id does not know waits, and what comes after it too...
    w.between_rounds("DELETE FROM idp_outbox").await;
    w.enqueue(
        "put_scope",
        "gw.workspace:ws_lost",
        json!({ "kind": "gw.workspace", "id": "ws_lost", "parent": "organisation:org_lost" }),
    )
    .await;
    w.enqueue("put_organisation", "organisation:org_later", json!({ "id": "org_later", "slug": "later" })).await;
    until("the lost workspace was tried", async || w.outbox().await.first().is_some_and(|r| r.1 >= 1)).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let rows = w.outbox().await;
    assert_eq!((rows.len(), rows[0].2), (2, false), "waiting, in order: {rows:?}");
    assert!(w.organisation("org_later").await.is_none());
    // ... until it has been tried long enough: then it is parked, and the rest goes through.
    w.between_rounds(
        "UPDATE idp_outbox SET attempts = 9, next_attempt_at = now() WHERE target = 'gw.workspace:ws_lost'",
    )
    .await;
    until("the row behind the lost workspace is delivered", async || w.organisation("org_later").await.is_some()).await;
    until("the round is saved", async || w.outbox().await.len() == 1).await;
    let rows = w.outbox().await;
    assert_eq!((rows.len(), rows[0].1, rows[0].2), (1, 10, true), "{rows:?}");

    // A newer change to a parked target replaces the parked row.
    let org = w.gw.create("/orgs", json!({ "name": "acme" })).await.id;
    until("the organisation reaches pacod-id", async || w.organisation(&org).await.is_some()).await;
    sqlx::query(
        "INSERT INTO idp_outbox (op, target, payload, parked_at, last_error) VALUES ('put_organisation', $1, '{}', now(), 'refused')",
    )
    .bind(format!("organisation:{org}"))
    .execute(&w.db)
    .await
    .unwrap();
    let (s, v) =
        w.gw.api("PUT", &format!("/orgs/{org}"), Some(json!({ "name": "acme", "display_name": "Acme" }))).await;
    assert_eq!(s, 200, "{v}");
    until("the rename reaches pacod-id", async || {
        w.organisation(&org).await.is_some_and(|(_, name, _)| name == "Acme")
    })
    .await;
    until("the parked row is gone, the lost workspace's stays", async || {
        w.outbox().await.into_iter().map(|r| r.0).collect::<Vec<_>>() == ["gw.workspace:ws_lost"]
    })
    .await;
    w.stop().await;
}

#[tokio::test]
async fn the_sweep_registers_what_the_outbox_never_saw() {
    let w = world().await;
    until("the sweep is done", async || w.workspace("ws_system").await.is_some()).await;
    // An organisation pacod-id lost track of (made in local mode, or a restore): the next
    // start's sweep registers it again.
    let org = w.gw.create("/orgs", json!({ "name": "acme", "display_name": "Acme" })).await.id;
    until("the organisation reaches pacod-id", async || w.organisation(&org).await.is_some()).await;
    until("everything was delivered", async || w.outbox().await.is_empty()).await;
    sqlx::query("DELETE FROM organisations WHERE id = $1").bind(&org).execute(w.id.store().pool()).await.unwrap();
    let World { id, gw, .. } = w;
    let db = gw.database_url.clone();
    gw.app.shutdown().await;
    let mut boot = pacod_gw_core::bootstrap::Bootstrap::for_tests(&db, common::temp_dir("spool"));
    boot.auth = pacod_gw_core::bootstrap::AuthMode::Oidc(id.settings());
    let app = pacod_gw_core::app::App::start(boot, None).await.expect("the gateway starts again");
    until("the restarted gateway registers it again", async || {
        organisation_at(&id, &org).await == Some(("acme".into(), "Acme".into(), vec!["gw".into()]))
    })
    .await;
    app.shutdown().await;
    id.running.stop().await;
}

#[tokio::test]
async fn an_import_registers_what_it_brings() {
    let w = world().await;
    let org = w.gw.create("/orgs", json!({ "name": "acme", "display_name": "Acme" })).await.id;
    let ws =
        w.gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "prod", "display_name": "Prod" })).await.id;
    let (s, export) = w.gw.api("GET", "/system/config-export", None).await;
    assert_eq!(s, 200, "{export}");
    assert_eq!(w.gw.api("DELETE", &format!("/orgs/{org}/workspaces/{ws}"), None).await.0, 204);
    assert_eq!(w.gw.api("DELETE", &format!("/orgs/{org}"), None).await.0, 204);
    until("both leave pacod-id", async || w.organisation(&org).await.is_none()).await;

    // A dry run queues nothing; the import does.
    let (s, v) = w.gw.api("POST", "/system/config-import?dry_run=true", Some(export.clone())).await;
    assert_eq!((s, &v["applied"]), (200, &json!(false)), "{v}");
    until("the outbox is empty", async || w.outbox().await.is_empty()).await;
    let (s, v) = w.gw.api("POST", "/system/config-import", Some(export)).await;
    assert_eq!((s, &v["applied"]), (200, &json!(true)), "{v}");
    until("the imported organisation and workspace reach pacod-id", async || {
        w.workspace(&ws).await == Some((format!("organisation:{org}"), "Prod".into()))
    })
    .await;
    assert_eq!(w.organisation(&org).await, Some(("acme".into(), "Acme".into(), vec!["gw".into()])));
    w.stop().await;
}
