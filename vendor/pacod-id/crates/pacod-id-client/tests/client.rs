//! The client against a real pacod-id, started in process (the server's own test harness).

#[path = "../../pacod-id/tests/common/mod.rs"]
mod common;

use std::collections::HashMap;
use std::time::Duration;

use common::{Browser, TestId};
use pacod_id::authz::bindings::{self, NewBinding, Protect};
use pacod_id_authz::{Manifest, ScopeRef};
use pacod_id_client::{
    AuthError, Authenticator, AuthzCache, IdClient, IdConfig, TokenExchanger, Verifier, VerifyError,
};
use serde_json::{Value, json};
use tokio::sync::watch;

const CORE: &str = "a-secret-for-the-gateway-core-0123456789";

fn config(id: &TestId) -> IdConfig {
    IdConfig {
        issuers: vec![id.public.clone()],
        internal_url: id.internal.clone(),
        client_id: "gw-core".into(),
        client_secret: CORE.into(),
        product: "gw".into(),
        audience: "pacod-gw".into(),
    }
}

async fn start() -> (TestId, IdClient) {
    let id = TestId::start().await;
    id.service_secret("gw-core", CORE).await;
    let client = IdClient::new(config(&id));
    (id, client)
}

fn gw_manifest() -> pacod_id_authz::ManifestSource {
    let text =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../pacod-id/tests/fixtures/gw.yaml")).unwrap();
    Manifest::from_yaml(&text).unwrap().source().clone()
}

async fn bind(id: &TestId, principal: &str, role: &str, scope: ScopeRef) -> i64 {
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
    let seq = bindings::apply(&mut tx, &st.catalog.get(), Protect::Skip, &[], &add).await.unwrap().seq.unwrap();
    tx.commit().await.unwrap();
    seq
}

/// A signed-in person's tokens from the console.
async fn tokens(id: &TestId, email: &str) -> Value {
    let mut b = Browser::new(id);
    let (code, verifier) = id.sign_in(&mut b, email).await;
    id.redeem(&code, &verifier).await.1
}

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..80 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("not within 4 seconds: {what}");
}

#[tokio::test]
async fn only_access_tokens_for_the_product_count() {
    let (id, client) = start().await;
    common::account(&id, "ada@example.com").await;
    let t = tokens(&id, "ada@example.com").await;
    let verifier = Verifier::new(client.clone());
    let access = verifier.verify(t["access_token"].as_str().unwrap()).await.unwrap();
    assert_eq!(
        (access.kind.as_str(), access.client_id.as_str(), access.issuer.as_str()),
        ("user", "console", id.public.as_str())
    );
    assert!(access.sid.is_some());
    // The console's scope has email and profile: who it is, for display.
    assert_eq!((access.email.as_deref(), access.name.as_deref()), (Some("ada@example.com"), Some("Ada Lovelace")));

    let facade = client.exchange_for_admin(t["access_token"].as_str().unwrap()).await.unwrap().access_token;
    let service = client.service_token().await.unwrap();
    for (what, token) in [
        ("an id_token", t["id_token"].as_str().unwrap()),
        ("a token for pacod-id", &facade),
        ("the service's own", &service),
    ] {
        assert!(matches!(verifier.verify(token).await, Err(VerifyError::Invalid(_))), "{what}");
    }
    let elsewhere =
        Verifier::new(IdClient::new(IdConfig { issuers: vec!["https://id.other.example".into()], ..config(&id) }));
    assert!(
        matches!(elsewhere.verify(t["access_token"].as_str().unwrap()).await, Err(VerifyError::Invalid(_))),
        "another issuer"
    );
    id.running.stop().await;
}

#[tokio::test]
async fn the_copy_follows_the_feed_and_the_caller_is_checked_against_it() {
    let (id, client) = start().await;
    let pushed = client.push_catalog(&gw_manifest(), &HashMap::new()).await.unwrap();
    assert!(pushed.changed);
    assert!(!client.push_catalog(&gw_manifest(), &HashMap::new()).await.unwrap().changed, "the same manifest again");
    assert_eq!(client.last_import().await.unwrap(), None, "the gateway was never imported");
    client.put_organisation("org_1", "acme", "Acme", false).await.unwrap();
    let err = client.put_scope("gw.workspace", "ws_1", "organisation:org_9", "Prod", false).await.unwrap_err();
    assert_eq!(err.code(), Some("scope_unknown"), "the outbox waits for the organisation");
    let seq = client.put_scope("gw.workspace", "ws_1", "organisation:org_1", "Prod", false).await.unwrap();

    let (_stop_tx, stop) = watch::channel(false);
    let cache = AuthzCache::spawn(client.clone(), stop);
    assert!(cache.wait_for(seq, Duration::from_secs(5)).await);
    assert!(cache.is_ready() && cache.feed_age().is_some());
    assert_eq!(cache.snapshot().scope_names.get("gw.workspace:ws_1").map(String::as_str), Some("Prod"));
    let mut events = cache.subscribe();

    let ada = common::account(&id, "ada@example.com").await;
    let seq = bind(&id, &ada.id, "gw.workspace_admin", ScopeRef::of("gw.workspace", "ws_1")).await;
    assert!(cache.wait_for(seq, Duration::from_secs(2)).await, "within the feed's promise");
    let held = cache.bindings_of(&ada.id);
    assert_eq!(held.len(), 1);
    assert_eq!(
        held[0].path.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["platform", "organisation:org_1", "gw.workspace:ws_1"]
    );
    let e = events.recv().await.unwrap();
    assert_eq!((e.kind.as_str(), e.data["principal"].as_str()), ("binding.added", Some(ada.id.as_str())));

    // One step per request: the token, the session, the account, the bindings.
    let auth = Authenticator::new(client.clone(), cache.clone());
    let t = tokens(&id, "ada@example.com").await;
    let access = t["access_token"].as_str().unwrap();
    let caller = auth.authenticate(access).await.unwrap();
    assert_eq!((caller.sub(), caller.bindings().len()), (ada.id.as_str(), 1));
    assert!(matches!(auth.authenticate("eyJhbGciOiJub25lIn0.e30.").await, Err(AuthError::Invalid)));
    // Signed out elsewhere: refused at once, though the token has minutes left.
    pacod_id::oidc::sign_out_everywhere(id.store(), &ada.id, "test").await.unwrap();
    eventually("the session ends in the copy", || cache.sid_revoked(&caller_sid(&caller))).await;
    assert!(matches!(auth.authenticate(access).await, Err(AuthError::SessionEnded)));

    // Disabled, then deleted: the product hears of it and confirms.
    let t = tokens(&id, "ada@example.com").await;
    id.sql(&format!("UPDATE accounts SET status = 'disabled' WHERE id = '{}'", ada.id)).await;
    let mut tx = id.store().begin().await.unwrap();
    let seq =
        pacod_id::store::changes::record(&mut tx, "principal.disabled", None, Some(&ada.id), json!({ "sub": ada.id }))
            .await
            .unwrap();
    tx.commit().await.unwrap();
    assert!(cache.wait_for(seq, Duration::from_secs(2)).await);
    assert!(matches!(auth.authenticate(t["access_token"].as_str().unwrap()).await, Err(AuthError::Disabled)));
    id.sql(&format!("UPDATE accounts SET status = 'active' WHERE id = '{}'", ada.id)).await;
    let subject = pacod_id_authz::Subject::user(vec![]);
    pacod_id::admin::deletion::delete_account(
        &id.running.state,
        &id.store().account(&ada.id).await.unwrap().unwrap(),
        &subject,
        "test",
    )
    .await
    .unwrap();
    let deleted = loop {
        let e = tokio::time::timeout(Duration::from_secs(4), events.recv()).await.expect("an event").unwrap();
        if e.kind == "principal.deleted" {
            break e;
        }
    };
    assert_eq!(deleted.data["sub"], ada.id.as_str());
    assert!(cache.is_disabled(&ada.id) && cache.bindings_of(&ada.id).is_empty());
    client.deletion_done(&ada.id).await.unwrap();
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_deletions").fetch_one(id.store().pool()).await.unwrap();
    assert_eq!(left, 0);
    id.running.stop().await;
}

fn caller_sid(c: &pacod_id_client::Caller) -> String {
    match c {
        pacod_id_client::Caller::User { sid, .. } => sid.clone(),
        _ => panic!("a person"),
    }
}

#[tokio::test]
async fn service_tokens_and_facades() {
    let (id, client) = start().await;
    client.push_catalog(&gw_manifest(), &HashMap::new()).await.unwrap();
    let root = common::account(&id, "root@example.com").await;
    bind(&id, &root.id, "id.admin", ScopeRef::platform()).await;
    bind(&id, &root.id, "gw.platform_admin", ScopeRef::platform()).await;
    let (_stop_tx, stop) = watch::channel(false);
    let cache = AuthzCache::spawn(client.clone(), stop);
    let auth = Authenticator::new(client.clone(), cache.clone());

    // A facade acts for the caller with pacod-id's admin API.
    let exchanger = TokenExchanger::new(client.clone());
    let t = tokens(&id, "root@example.com").await;
    let facade = exchanger.for_caller(t["access_token"].as_str().unwrap()).await.unwrap();
    assert_eq!(facade, exchanger.for_caller(t["access_token"].as_str().unwrap()).await.unwrap(), "cached");
    let r = client
        .admin(reqwest::Method::POST, "/service-accounts", &facade)
        .json(&json!({ "name": "ci" }))
        .send()
        .await
        .unwrap();
    let svc: Value = IdClient::read(r).await.unwrap();
    let svc_id = svc["id"].as_str().unwrap().to_owned();
    let r = client
        .admin(reqwest::Method::POST, &format!("/service-accounts/{svc_id}/tokens"), &facade)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let token: Value = IdClient::read(r).await.unwrap();
    let secret = token["token"].as_str().unwrap().to_owned();
    let seq = bind(&id, &svc_id, "gw.viewer", ScopeRef::platform()).await;
    assert!(cache.wait_for(seq, Duration::from_secs(2)).await);

    let caller = auth.authenticate(&secret).await.unwrap();
    assert_eq!((caller.sub(), caller.bindings().len()), (svc_id.as_str(), 1));
    assert!(matches!(auth.authenticate("pidt_nope_guess").await, Err(AuthError::Invalid)));
    // Revoked: the cached answer goes with the feed's event.
    let r = client
        .admin(reqwest::Method::DELETE, &format!("/service-tokens/{}", token["id"].as_str().unwrap()), &facade)
        .send()
        .await
        .unwrap();
    let revoked: Value = IdClient::read(r).await.unwrap();
    assert!(cache.wait_for(revoked["seq"].as_i64().unwrap(), Duration::from_secs(2)).await);
    let mut refused = false;
    for _ in 0..40 {
        if matches!(auth.authenticate(&secret).await, Err(AuthError::Invalid)) {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(refused, "a revoked token stops before its 60 cached seconds are up");

    // A delegated grant for a long task: refreshable, revocable, ended with the session.
    let grant = client.delegated_grant(t["access_token"].as_str().unwrap()).await.unwrap();
    let refreshed = client.refresh(grant.refresh_token.as_deref().unwrap()).await.unwrap();
    assert!(auth.authenticate(&refreshed.access_token).await.is_ok());
    client.revoke(refreshed.refresh_token.as_deref().unwrap()).await.unwrap();
    assert_eq!(
        client.refresh(refreshed.refresh_token.as_deref().unwrap()).await.unwrap_err().code(),
        Some("invalid_grant")
    );
    // The session ends server to server.
    let sid = match auth.authenticate(t["access_token"].as_str().unwrap()).await.unwrap() {
        pacod_id_client::Caller::User { sid, .. } => sid,
        _ => panic!("a person"),
    };
    client.revoke_session(&sid).await.unwrap();
    eventually("the session ends in the copy", || cache.sid_revoked(&sid)).await;
    id.running.stop().await;
}

#[tokio::test]
async fn starting_does_not_wait_for_pacod_id() {
    let client = IdClient::new(IdConfig {
        issuers: vec!["http://127.0.0.1:9".into()],
        internal_url: "http://127.0.0.1:9".into(),
        client_id: "gw-core".into(),
        client_secret: CORE.into(),
        product: "gw".into(),
        audience: "pacod-gw".into(),
    });
    let (stop_tx, stop) = watch::channel(false);
    let cache = AuthzCache::spawn(client.clone(), stop);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!cache.is_ready() && cache.feed_age().is_none());
    let auth = Authenticator::new(client, cache.clone());
    assert!(matches!(
        auth.authenticate("eyJhbGciOiJSUzI1NiIsImtpZCI6ImsifQ.e30.c2ln").await,
        Err(AuthError::Unavailable(_))
    ));
    assert!(matches!(
        auth.authenticate(&format!("pidt_01a1x_{}", "s".repeat(43))).await,
        Err(AuthError::Unavailable(_))
    ));
    stop_tx.send(true).unwrap();
}

#[tokio::test]
async fn a_cursor_pacod_id_no_longer_keeps_loads_the_snapshot_again() {
    let (id, client) = start().await;
    client.push_catalog(&gw_manifest(), &HashMap::new()).await.unwrap();
    let ada = common::account(&id, "ada@example.com").await;
    let (_stop_tx, stop) = watch::channel(false);
    let cache = AuthzCache::spawn(client.clone(), stop);
    eventually("the first snapshot", || cache.is_ready()).await;
    // In one transaction: a binding, another change, and the binding's change already pruned.
    // The follower's cursor is then older than what pacod-id keeps (410).
    let st = &id.running.state;
    let mut tx = st.store.begin().await.unwrap();
    pacod_id::store::authz::lock_head(&mut tx).await.unwrap();
    let add = [NewBinding {
        principal: ada.id.clone(),
        role: "gw.viewer".into(),
        scope: ScopeRef::platform(),
        source: "manual",
        created_by: "test".into(),
    }];
    let first = bindings::apply(&mut tx, &st.catalog.get(), Protect::Skip, &[], &add).await.unwrap().seq.unwrap();
    let last =
        pacod_id::store::changes::record(&mut tx, "account.updated", None, Some(&ada.id), json!({ "sub": ada.id }))
            .await
            .unwrap();
    sqlx::query("DELETE FROM authz_changes WHERE version <= $1").bind(first).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert!(cache.wait_for(last, Duration::from_secs(5)).await);
    assert_eq!(cache.bindings_of(&ada.id).len(), 1, "the binding came with the new snapshot");
    id.running.stop().await;
}

/// A TCP proxy to `to` that can be made to accept connections and never answer: pacod-id
/// behind a network that drops packets.
async fn stall_proxy(to: std::net::SocketAddr) -> (String, std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let open = std::sync::Arc::new(AtomicBool::new(true));
    let flag = open.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else { return };
            let open = flag.load(Ordering::SeqCst);
            tokio::spawn(async move {
                if open {
                    if let Ok(mut outbound) = tokio::net::TcpStream::connect(to).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                } else {
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    drop(inbound);
                }
            });
        }
    });
    (url, open)
}

#[tokio::test]
async fn a_silent_pacod_id_holds_up_no_request() {
    let (id, _) = start().await;
    common::account(&id, "ada@example.com").await;
    let t = tokens(&id, "ada@example.com").await;
    let token = t["access_token"].as_str().unwrap().to_owned();
    let (url, open) = stall_proxy(id.running.internal).await;
    let client = IdClient::new(IdConfig { internal_url: url, ..config(&id) });
    // Keys due again at every request, so that each one would read them if it could.
    let verifier = Verifier::with_refresh_every(client, Duration::ZERO);
    verifier.verify(&token).await.unwrap();

    open.store(false, std::sync::atomic::Ordering::SeqCst);
    let started = std::time::Instant::now();
    let checks: Vec<_> = (0..20)
        .map(|_| {
            let (v, t) = (verifier.clone(), token.clone());
            tokio::spawn(async move { v.verify(&t).await.is_ok() })
        })
        .collect();
    for c in checks {
        assert!(c.await.unwrap(), "verified with the known keys");
    }
    assert!(started.elapsed() < Duration::from_millis(500), "took {:?}", started.elapsed());

    // Nothing read yet and pacod-id silent: one read, given up after its timeout; the callers
    // that come meanwhile wait for it, they do not each start their own.
    let (url, open) = stall_proxy(id.running.internal).await;
    open.store(false, std::sync::atomic::Ordering::SeqCst);
    let verifier = Verifier::new(IdClient::new(IdConfig { internal_url: url, ..config(&id) }));
    let started = std::time::Instant::now();
    let checks: Vec<_> = (0..10)
        .map(|_| {
            let (v, t) = (verifier.clone(), token.clone());
            tokio::spawn(async move { matches!(v.verify(&t).await, Err(VerifyError::Unavailable(_))) })
        })
        .collect();
    for c in checks {
        assert!(c.await.unwrap(), "unavailable, not invalid");
    }
    // One read of five seconds; ten in a row would take fifty.
    let took = started.elapsed();
    assert!(took >= Duration::from_secs(4) && took < Duration::from_secs(8), "one read, not ten: {took:?}");
    id.running.stop().await;
}
