//! `pacod-id import gw`: the gateway's people, admin tokens, roles and invitations, taken over
//! with their ids and hashes, all or nothing; refused over conflicts and over writes since.

mod common;

use common::{Browser, PASSWORD, TestId};
use pacod_id::import::{self, GwExport, ImportError};
use serde_json::{Value, json};

const CORE: &str = "a-secret-for-the-gateway-core-0123456789";
const GW_PEPPER: &str = "the-gateway-s-key-pepper-0123456789";
const ADMIN_TOKEN: &str = "pgwa_0193_an-admin-token-from-the-gateway";
const INVITE: &str = "an-invitation-link-token-from-the-gateway";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn gw_manifest() -> Value {
    let text = std::fs::read_to_string(format!("{}/tests/fixtures/gw.yaml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    serde_json::to_value(pacod_id_authz::Manifest::from_yaml(&text).unwrap().source()).unwrap()
}

/// An export as `pacod-gw-core identity export` writes it.
fn export() -> Value {
    let hash = pacod_id::crypto::password::hash(PASSWORD, Default::default()).unwrap();
    let pepper = pacod_id::crypto::Pepper::new(GW_PEPPER.as_bytes().to_vec()).unwrap();
    let at = "2026-09-01T10:00:00Z";
    json!({
        "format": "pacod-gw-identity/1",
        "exported_at": "2026-10-13T08:00:00Z",
        "config_version": 42,
        "manifest": gw_manifest(),
        "settings": { "invitation_link_days": 7 },
        "organisations": [{ "id": "org_1", "name": "acme", "display_name": "Acme", "system": false }],
        "workspaces": [{ "id": "ws_1", "organisation_id": "org_1", "name": "prod", "display_name": "Prod", "system": false }],
        "users": [
            { "id": "usr_ada", "created_at": at, "updated_at": at,
              "doc": { "email": "Ada@Example.com", "display_name": "Ada", "password_hash": hash, "status": "active", "email_verified": true } },
            { "id": "usr_bob", "created_at": at, "updated_at": at,
              "doc": { "email": "bob@example.com", "display_name": "", "password_hash": hash, "status": "active" } },
            { "id": "usr_cy", "created_at": at, "updated_at": at,
              "doc": { "email": "cy@example.com", "display_name": "Cy", "password_hash": hash, "status": "disabled", "email_verified": false } }
        ],
        "admin_tokens": [
            { "id": "tok_0193", "created_at": at,
              "doc": { "name": "deploy bot", "secret_hmac": hex(&pepper.hash(ADMIN_TOKEN)), "hint": "eway", "status": "active" } }
        ],
        "role_bindings": [
            { "id": "rb_1", "created_at": at, "doc": { "principal": "usr_ada", "role": "platform_admin", "scope": { "type": "platform" } } },
            { "id": "rb_2", "created_at": at, "doc": { "principal": "usr_bob", "role": "org_admin", "scope": { "type": "organisation", "id": "org_1" } } },
            { "id": "rb_3", "created_at": at, "doc": { "principal": "tok_0193", "role": "viewer", "scope": { "type": "workspace", "id": "ws_1" } } },
            { "id": "rb_4", "created_at": at, "doc": { "principal": "usr_gone", "role": "viewer", "scope": { "type": "organisation", "id": "org_1" } } }
        ],
        "invitations": [
            { "id": "inv_1", "token_hash": hex(&pacod_id::crypto::sha256(INVITE.as_bytes())), "email": "nia@example.com",
              "role": "viewer", "organisation_id": "org_1", "workspace_id": "ws_1", "invited_by": "usr_bob", "emailed": true,
              "created_at": at, "expires_at": "2099-01-01T00:00:00Z" }
        ]
    })
}

fn parsed(v: &Value) -> GwExport {
    serde_json::from_value(v.clone()).unwrap()
}

async fn start() -> TestId {
    let db = common::fresh_database().await;
    let id = TestId::start_with(
        &db,
        |_| String::new(),
        |boot| {
            boot.legacy_peppers
                .insert("gw".into(), pacod_id::crypto::Pepper::new(GW_PEPPER.as_bytes().to_vec()).unwrap());
        },
    )
    .await;
    id.service_secret("gw-core", CORE).await;
    id
}

async fn count(id: &TestId, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(id.store().pool()).await.unwrap()
}

async fn since(id: &TestId) -> (u16, Value) {
    id.internal_call(reqwest::Method::GET, "/internal/v1/imports/gw", "gw-core", CORE, None).await
}

#[tokio::test]
async fn the_gateway_s_people_come_over_with_their_ids_and_secrets() {
    let id = start().await;
    let file = export();
    assert_eq!(since(&id).await.0, 404, "never imported");

    // A dry run reports and changes nothing.
    let report = import::gw(id.store(), &id.running.state.config, "gateway", &parsed(&file), true).await.unwrap();
    assert_eq!((report.dry_run, report.applied, report.catalog), (true, false, "pushed"));
    assert_eq!((report.accounts.created, report.service_accounts.created, report.bindings.created), (3, 1, 3));
    assert_eq!(report.invitations.created, 1);
    assert_eq!(report.skipped, ["role binding rb_4: its principal usr_gone is not in the export"]);
    assert_eq!(count(&id, "SELECT count(*) FROM accounts").await, 0);
    assert_eq!(count(&id, "SELECT count(*) FROM imports").await, 0);

    let report = import::gw(id.store(), &id.running.state.config, "gateway", &parsed(&file), false).await.unwrap();
    assert!(report.applied);
    assert_eq!(
        (report.organisations.created, report.workspaces.created, report.accounts.created, report.bindings.created),
        (1, 1, 3, 3)
    );
    // Ids kept; addresses proven as the export says.
    let proofs: Vec<(String, String, String, bool)> = sqlx::query_as(
        "SELECT id, email, email_verified_source, email_verified_at IS NOT NULL FROM accounts ORDER BY id",
    )
    .fetch_all(id.store().pool())
    .await
    .unwrap();
    assert_eq!(
        proofs,
        [
            ("usr_ada".into(), "ada@example.com".into(), "verified".into(), true),
            ("usr_bob".into(), "bob@example.com".into(), "legacy_assumed".into(), true),
            ("usr_cy".into(), "cy@example.com".into(), String::new(), false),
        ]
    );
    let held: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, principal_id, role FROM role_bindings ORDER BY id")
            .fetch_all(id.store().pool())
            .await
            .unwrap();
    assert_eq!(
        held,
        [
            ("rb_1".into(), "usr_ada".into(), "gw.platform_admin".into()),
            ("rb_2".into(), "usr_bob".into(), "gw.org_admin".into()),
            ("rb_3".into(), "tok_0193".into(), "gw.viewer".into()),
        ]
    );

    // The running service takes the catalogue the import stored when the gateway pushes it.
    let (s, v) = id
        .internal_call(
            reqwest::Method::PUT,
            "/internal/v1/catalog/gw",
            "gw-core",
            CORE,
            Some(json!({ "manifest": gw_manifest() })),
        )
        .await;
    assert_eq!((s, v["changed"].as_bool()), (200, Some(false)), "{v}");
    assert!(id.running.state.catalog.get().manifest("gw").is_some());

    // The old password signs in; the old admin token is a service token; the old link works.
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "ada@example.com").await;
    assert_eq!(id.redeem(&code, &verifier).await.0, 200);
    let r = id
        .http
        .post(format!("{}/internal/v1/introspect", id.internal))
        .basic_auth("gw-core", Some(CORE))
        .form(&[("token", ADMIN_TOKEN)])
        .send()
        .await
        .unwrap();
    let v: Value = r.json().await.unwrap();
    assert_eq!((v["active"].as_bool(), v["sub"].as_str()), (Some(true), Some("tok_0193")), "{v}");
    let mut b = Browser::new(&id);
    let a = b.post(&format!("{}/ui-api/v1/invitations/lookup", id.public), json!({ "token": INVITE })).await;
    assert_eq!((a.status, a.json["inviter_name"].as_str()), (200, Some("bob@example.com")), "{a:?}");

    // Again: nothing to do.
    let report = import::gw(id.store(), &id.running.state.config, "gateway", &parsed(&file), false).await.unwrap();
    assert_eq!((report.accounts.unchanged, report.bindings.unchanged, report.invitations.unchanged), (3, 3, 1));
    assert_eq!(report.accounts.created + report.accounts.updated + report.service_accounts.updated, 0);

    // A sign-in is no identity write; a change of roles is, and it blocks a rollback and a new import.
    let (s, v) = since(&id).await;
    assert_eq!((s, v["writes"].as_i64(), v["source_version"].as_i64()), (200, Some(0), Some(42)), "{v}");
    id.sql("DELETE FROM role_bindings WHERE id = 'rb_3'").await;
    pacod_id::store::changes::record(
        &mut id.store().pool().acquire().await.unwrap(),
        "binding.removed",
        Some("gw"),
        Some("tok_0193"),
        json!({ "id": "rb_3" }),
    )
    .await
    .unwrap();
    let (_, v) = since(&id).await;
    assert_eq!(v["writes"].as_i64(), Some(1), "{v}");
    match import::gw(id.store(), &id.running.state.config, "gateway", &parsed(&file), false).await {
        Err(ImportError::WrittenSince { count: 1, .. }) => {}
        other => panic!("{other:?}"),
    }
    id.running.stop().await;
}

#[tokio::test]
async fn an_address_pacod_id_has_already_is_a_conflict_and_nothing_is_imported() {
    let id = start().await;
    common::account(&id, "bob@example.com").await;
    match import::gw(id.store(), &id.running.state.config, "gateway", &parsed(&export()), false).await {
        Err(ImportError::Conflicts(report)) => {
            assert_eq!(report.conflicts.len(), 1, "{:?}", report.conflicts);
            assert!(report.conflicts[0].contains("usr_bob") && report.conflicts[0].contains("not merged"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(count(&id, "SELECT count(*) FROM accounts").await, 1);
    assert_eq!(count(&id, "SELECT count(*) FROM organisations").await, 0);
    id.running.stop().await;
}

#[tokio::test]
async fn an_id_pacod_id_has_for_something_else_is_a_conflict() {
    let id = start().await;
    // An account made here (not by an import) under the id the export names.
    let hash = pacod_id::crypto::password::hash(PASSWORD, Default::default()).unwrap();
    id.store()
        .account_create(&pacod_id::store::NewAccount {
            id: "usr_ada".into(),
            email: "someone@example.com".into(),
            password_hash: Some(hash),
            signup_realm: "gateway".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut file = export();
    file["admin_tokens"][0]["doc"]["expires_at"] = json!("next tuesday");
    match import::gw(id.store(), &id.running.state.config, "gateway", &parsed(&file), true).await {
        Err(ImportError::Conflicts(report)) => {
            let all = report.conflicts.join(" | ");
            assert!(all.contains("usr_ada: pacod-id has an account by this id that is not the gateway's"), "{all}");
            assert!(all.contains("tok_0193: its expires_at"), "{all}");
        }
        other => panic!("{other:?}"),
    }
    id.running.stop().await;
}

#[tokio::test]
async fn a_second_import_brings_what_the_gateway_removed_meanwhile_to_an_end() {
    let id = start().await;
    let config = id.running.state.config.clone();
    import::gw(id.store(), &config, "gateway", &parsed(&export()), false).await.unwrap();
    let mut b = Browser::new(&id);
    let (code, verifier) = id.sign_in(&mut b, "bob@example.com").await;
    assert_eq!(id.redeem(&code, &verifier).await.0, 200);

    // Rolled back, the gateway removed its admin token, Ada and an invitation, and gave Bob a
    // new password; then the people move again.
    let mut file = export();
    file["admin_tokens"] = json!([]);
    file["role_bindings"] = json!([file["role_bindings"][1].clone()]);
    file["invitations"] = json!([]);
    file["users"] = json!([file["users"][1].clone(), file["users"][2].clone()]);
    let other = pacod_id::crypto::password::hash("another long passphrase", Default::default()).unwrap();
    file["users"][0]["doc"]["password_hash"] = json!(other);
    file["users"][0]["doc"]["display_name"] = json!("Bob");
    let report = import::gw(id.store(), &config, "gateway", &parsed(&file), false).await.unwrap();
    assert_eq!(
        (report.accounts.retired, report.service_accounts.retired, report.bindings.retired, report.invitations.retired),
        (1, 1, 2, 1),
        "{report:?}"
    );
    assert_eq!(report.sessions_ended, 1, "Bob's session ends with his old password");
    let status: Vec<(String, String)> =
        sqlx::query_as("SELECT id, status FROM accounts ORDER BY id").fetch_all(id.store().pool()).await.unwrap();
    assert_eq!(status[0], ("usr_ada".into(), "disabled".into()));
    assert_eq!(count(&id, "SELECT count(*) FROM service_tokens WHERE status = 'active'").await, 0);
    assert_eq!(count(&id, "SELECT count(*) FROM role_bindings").await, 1);
    assert_eq!(count(&id, "SELECT count(*) FROM invitations").await, 0);
    let kinds: Vec<String> = sqlx::query_scalar("SELECT DISTINCT kind FROM authz_changes ORDER BY kind")
        .fetch_all(id.store().pool())
        .await
        .unwrap();
    for kind in ["account.updated", "binding.removed", "credential.revoked", "principal.disabled", "session.revoked"] {
        assert!(kinds.iter().any(|k| k == kind), "{kind} in {kinds:?}");
    }
    id.running.stop().await;
}

#[tokio::test]
async fn only_writes_about_the_gateway_s_people_block_a_rollback() {
    let id = start().await;
    let config = id.running.state.config.clone();
    import::gw(id.store(), &config, "gateway", &parsed(&export()), false).await.unwrap();
    // Someone else's account changes (another realm, another person): not the gateway's business.
    let carol = common::account(&id, "carol@example.com").await;
    let mut conn = id.store().pool().acquire().await.unwrap();
    pacod_id::store::changes::record(&mut conn, "account.updated", None, Some(&carol.id), json!({ "sub": carol.id }))
        .await
        .unwrap();
    sqlx::query("INSERT INTO audit_log (actor, actor_kind, action, subject, realm) VALUES ($1, 'user', 'profile_updated', $1, 'bazaar')")
        .bind(&carol.id)
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(since(&id).await.1["writes"].as_i64(), Some(0));
    // Bob changes his password on another realm's pages: that counts.
    sqlx::query("INSERT INTO audit_log (actor, actor_kind, action, subject, realm) VALUES ('usr_bob', 'user', 'password_changed', 'usr_bob', 'bazaar')")
        .execute(&mut *conn)
        .await
        .unwrap();
    assert_eq!(since(&id).await.1["writes"].as_i64(), Some(1));
    id.running.stop().await;
}
