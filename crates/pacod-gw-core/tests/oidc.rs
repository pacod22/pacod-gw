//! The core in OIDC mode (identity concept, phase 2), against pacod-id started in process.

mod common;
#[path = "common/id.rs"]
mod id;

use id::Id;
use pacod_id_client::{IdClient, Verifier};

#[tokio::test]
async fn pacod_id_runs_in_process_and_its_tokens_verify_for_the_gateway() {
    let id = Id::start().await;
    id.account("ada@example.com").await;
    let tokens = id.sign_in("ada@example.com").await;
    let access = tokens["access_token"].as_str().unwrap_or_else(|| panic!("{tokens}"));
    let verified = Verifier::new(IdClient::new(id.client_config())).verify(access).await.unwrap();
    assert_eq!((verified.kind.as_str(), verified.client_id.as_str()), ("user", "pacod-gw-console"));
    assert!(verified.sid.is_some());
    id.running.stop().await;
}

/// Calls the gateway's management API with `token`.
async fn call(gw: &common::TestGw, token: &str, method: &str, path: &str) -> (u16, serde_json::Value) {
    gw.api_as(token, method, path, None).await
}

/// Waits until `check` holds, at most `secs` seconds.
async fn within(secs: u64, what: &str, mut check: impl AsyncFnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if check().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("not within {secs} seconds: {what}");
}

#[tokio::test]
async fn people_sign_in_at_pacod_id_and_their_roles_come_from_its_feed() {
    let id = Id::start().await;
    let gw = id::gateway(&id, |_| {}).await;
    id.wait_for_gw_catalog().await;
    let ada = id.account("ada@example.com").await;
    let bob = id.account("bob@example.com").await;
    let ada_t = id.sign_in("ada@example.com").await["access_token"].as_str().unwrap().to_owned();
    let bob_t = id.sign_in("bob@example.com").await["access_token"].as_str().unwrap().to_owned();

    // Without a role: known, and allowed nothing.
    let (s, v) = call(&gw, &bob_t, "GET", "/session").await;
    assert_eq!(
        (s, v["display"].as_str(), v["platform_admin"].as_bool()),
        (200, Some("bob@example.com"), Some(false)),
        "{v}"
    );
    assert_eq!(call(&gw, &bob_t, "GET", "/users").await.0, 403);

    // A role given at pacod-id reaches the gateway through the feed, within seconds.
    id.bind(&ada, "gw.platform_admin", pacod_id_authz::ScopeRef::platform()).await;
    within(3, "ada's role reaches the gateway", async || {
        call(&gw, &ada_t, "GET", "/session").await.1["platform_admin"] == true
    })
    .await;
    assert_eq!(call(&gw, &ada_t, "GET", "/orgs").await.0, 200);
    assert!(bob != ada);

    // Signing out at the gateway ends the session at pacod-id, and so everywhere.
    assert_eq!(call(&gw, &ada_t, "DELETE", "/session").await.0, 204);
    within(3, "the ended session is refused", async || call(&gw, &ada_t, "GET", "/session").await.0 == 401).await;
    gw.app.shutdown().await;
    id.running.stop().await;
}

#[tokio::test]
async fn what_pacod_id_took_over_answers_where_to_go() {
    let id = Id::start().await;
    let gw = id::gateway(&id, |_| {}).await;
    id.wait_for_gw_catalog().await;
    let r = gw
        .http
        .post(format!("{}/api/v1/session", gw.admin))
        .json(&serde_json::json!({ "email": "ada@example.com", "password": "whatever it is" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 410);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(
        (v["error"]["type"].as_str(), v["error"]["details"]["issuer"].as_str()),
        (Some("moved_to_identity"), Some(id.public.as_str()))
    );
    let r = gw
        .http
        .post(format!("{}/api/v1/session/password-reset", gw.admin))
        .json(&serde_json::json!({ "email": "ada@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 410);
    // The gateway's own session tokens are not taken any more.
    let (s, v) = call(&gw, "pgws_01a1x_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "GET", "/session").await;
    assert_eq!((s, v["error"]["details"]["code"].as_str()), (401, Some("moved_to_identity")));
    gw.app.shutdown().await;
    id.running.stop().await;
}

#[tokio::test]
async fn a_disabled_account_is_refused_within_seconds() {
    let id = Id::start().await;
    let gw = id::gateway(&id, |_| {}).await;
    id.wait_for_gw_catalog().await;
    let ada = id.account("ada@example.com").await;
    let ada_t = id.sign_in("ada@example.com").await["access_token"].as_str().unwrap().to_owned();
    assert_eq!(call(&gw, &ada_t, "GET", "/session").await.0, 200);
    let st = &id.running.state;
    let mut tx = st.store.begin().await.unwrap();
    pacod_id::store::authz::lock_head(&mut tx).await.unwrap();
    pacod_id::store::accounts::account_set_status(&mut tx, &ada, "disabled").await.unwrap();
    pacod_id::store::changes::record(
        &mut tx,
        "principal.disabled",
        None,
        Some(&ada),
        serde_json::json!({ "sub": ada }),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    within(3, "the disabled account is refused", async || call(&gw, &ada_t, "GET", "/session").await.0 == 401).await;
    gw.app.shutdown().await;
    id.running.stop().await;
}

#[tokio::test]
async fn an_old_copy_of_the_roles_stops_management_calls() {
    let id = Id::start().await;
    let gw = id::gateway(&id, |b| {
        if let pacod_gw_core::bootstrap::AuthMode::Oidc(s) = &mut b.auth {
            s.max_staleness_seconds = 1;
        }
    })
    .await;
    id.wait_for_gw_catalog().await;
    id.account("ada@example.com").await;
    let ada_t = id.sign_in("ada@example.com").await["access_token"].as_str().unwrap().to_owned();
    within(3, "the copy is current", async || call(&gw, &ada_t, "GET", "/session").await.0 == 200).await;
    id.running.stop().await;
    within(10, "management answers 503 once the copy is old", async || {
        let (s, v) = call(&gw, &ada_t, "GET", "/session").await;
        s == 503 && v["error"]["type"] == "identity_unavailable"
    })
    .await;
    // The data plane does not ask pacod-id: its health stays.
    let r = gw.http.get(format!("{}/healthz", gw.data)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    gw.app.shutdown().await;
}
