//! Moving a gateway's people to pacod-id (identity concept 17.1), end to end: a gateway with its
//! own sign-in, `identity export`, `pacod-id import gw`, the switch to OIDC mode, `rollback`
//! (allowed until something is written at pacod-id), `finalize` (refused while an instance still
//! signs people in itself, or something came after the export) and no way back after it.

mod common;
#[path = "common/id.rs"]
mod id;

use std::time::Duration;

use pacod_gw_core::identity_migration as migration;
use pacod_gw_core::store::Store;
use serde_json::{Value, json};

/// The local test gateway's pepper (`Bootstrap::for_tests`), which pacod-id needs for `pgwa_`.
const GW_PEPPER: &[u8] = b"test-pepper-0123456789abcdef";

#[tokio::test]
async fn a_gateway_s_people_move_to_pacod_id_and_the_gateway_lets_go() {
    // A gateway signing people in itself: an organisation admin, an admin token with a role, an
    // invitation waiting.
    let gw = common::TestGw::start(None).await;
    let db = gw.database_url.clone();
    let org = gw.create("/orgs", json!({ "name": "acme", "display_name": "Acme" })).await.id;
    let ada = gw.create("/users", json!({ "email": "ada@example.com", "password": id::PASSWORD })).await.id;
    gw.create(
        "/role-bindings",
        json!({ "principal": ada, "role": "org_admin", "scope": { "type": "organisation", "id": org } }),
    )
    .await;
    let made = gw.create("/admin-tokens", json!({ "name": "deploy bot" })).await;
    let (token_id, secret) = (made.id.clone(), made.body["secret"].as_str().unwrap().to_owned());
    gw.create(
        "/role-bindings",
        json!({ "principal": token_id, "role": "viewer", "scope": { "type": "organisation", "id": org } }),
    )
    .await;
    let invite =
        gw.create(&format!("/orgs/{org}/invitations"), json!({ "email": "nia@example.com", "role": "viewer" })).await;
    let link = invite.body["token"].as_str().unwrap().to_owned();

    // The export: a new file only its owner reads.
    let store = Store::connect(&db, 2).await.unwrap();
    let path = common::temp_dir("export").join("gw-identity.json");
    let said = migration::write_export(&store, &path).await.unwrap();
    assert!(said.contains("2 users, 1 admin tokens, 3 role bindings, 1 invitations"), "{said}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(migration::write_export(&store, &path).await.is_err(), "an existing file is not overwritten");
    let export: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    // pacod-id takes it over, with the gateway's pepper for its admin tokens.
    let pid = id::Id::start_with(&common::fresh_database().await, |boot| {
        boot.legacy_peppers.insert("gw".into(), pacod_id::crypto::Pepper::new(GW_PEPPER.to_vec()).unwrap());
    })
    .await;
    let report = pacod_id::import::gw(
        pid.store(),
        &pid.running.state.config,
        "gateway",
        &serde_json::from_value(export).unwrap(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        (report.accounts.created, report.service_accounts.created, report.bindings.created, report.invitations.created),
        (2, 1, 3, 1),
        "{report:?}"
    );

    // The gateway switches to OIDC mode.
    gw.app.shutdown().await;
    let mut gw = id::gateway_on(&pid, &db, |_| {}).await;
    gw.token = pid.sign_in("ada@example.com").await["access_token"].as_str().unwrap().to_owned();
    let mut me = Value::Null;
    for _ in 0..100 {
        me = gw.api("GET", "/session", None).await.1;
        if me["roles"].as_array().is_some_and(|r| !r.is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!((me["id"].as_str(), me["roles"][0]["role"].as_str()), (Some(ada.as_str()), Some("org_admin")), "{me}");
    let (s, v) = gw.api_as(&secret, "GET", "/session", None).await;
    assert_eq!((s, v["id"].as_str(), v["kind"].as_str()), (200, Some(token_id.as_str()), Some("token")), "{v}");
    assert_eq!(pid.ui("/invitations/lookup", json!({ "token": link })).await.0, 200, "the old link works");

    // Back to the gateway's own sign-in: fine until something is written at pacod-id.
    let client = pacod_gw_core::identity::client(&pid.settings());
    let said = migration::rollback(&store, &client).await.unwrap();
    assert!(said.contains("set PGW_AUTH=local"), "{said}");
    let (s, v) = gw.api("PUT", &format!("/orgs/{org}/members/{token_id}"), Some(json!({ "role": "auditor" }))).await;
    assert_eq!(s, 204, "{v}");
    let refused = migration::rollback(&store, &client).await.unwrap_err().to_string();
    assert!(refused.contains("identity changes since the import"), "{refused}");

    // Finalize: not while an instance still signs people in itself (one started in local mode
    // meanwhile), nor with something newer than the export.
    for _ in 0..100 {
        if store.local_auth_instances(60.0).await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        store.local_auth_instances(60.0).await.unwrap(),
        Vec::<String>::new(),
        "the OIDC instance follows the feed"
    );
    let local = common::TestGw::start_on(&db, None, |_| {}).await;
    let refused = migration::finalize(&store, &client).await.unwrap_err().to_string();
    assert!(refused.contains("still sign people in themselves"), "{refused}");
    local.app.shutdown().await;
    let pool = sqlx::PgPool::connect(&db).await.unwrap();
    sqlx::query(
        "INSERT INTO invitations (id, token_hash, email, role, organisation_id, invited_by, expires_at)
         VALUES ('inv_late', '\\x00', 'late@example.com', 'viewer', $1, $2, now() + interval '1 day')",
    )
    .bind(&org)
    .bind(&ada)
    .execute(&pool)
    .await
    .unwrap();
    let refused = migration::finalize(&store, &client).await.unwrap_err().to_string();
    assert!(refused.contains("invitation inv_late"), "{refused}");
    sqlx::query("DELETE FROM invitations WHERE id = 'inv_late'").execute(&pool).await.unwrap();
    // A deletion someone made after the export counts (pacod-id would keep the token); one the
    // system made, as an organisation's roles go with it, does not.
    sqlx::query(
        "INSERT INTO audit_log (actor, actor_kind, action, kind, object_id, config_version)
         VALUES ($1, 'user', 'delete', 'admin_token', 'tok_late', 1000000),
                ('system', 'system', 'delete', 'role_binding', 'rb_late', 1000000)",
    )
    .bind(&ada)
    .execute(&pool)
    .await
    .unwrap();
    let refused = migration::finalize(&store, &client).await.unwrap_err().to_string();
    assert!(refused.contains("delete admin_token tok_late") && !refused.contains("rb_late"), "{refused}");
    sqlx::query("DELETE FROM audit_log WHERE object_id = 'tok_late'").execute(&pool).await.unwrap();

    let said = migration::finalize(&store, &client).await.unwrap();
    assert!(said.starts_with("6 users, admin tokens and role bindings deleted"), "{said}");
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM config_objects WHERE kind IN ('user', 'admin_token', 'role_binding'))
              + (SELECT count(*) FROM invitations) + (SELECT count(*) FROM sessions)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0);
    assert!(migration::finalize(&store, &client).await.unwrap().contains("ran already"));
    // People keep working through pacod-id.
    assert_eq!(gw.api("GET", "/session", None).await.0, 200);

    // No way back.
    let rollback = migration::rollback(&store, &client).await.unwrap_err().to_string();
    assert!(rollback.contains("not possible any more"), "{rollback}");
    let boot = pacod_gw_core::bootstrap::Bootstrap::for_tests(&db, common::temp_dir("spool"));
    let refused = pacod_gw_core::app::App::start(boot, None).await.err().expect("refused").to_string();
    assert!(refused.contains("PGW_AUTH=local is not possible any more"), "{refused}");
    gw.app.shutdown().await;
    pid.running.stop().await;
}
