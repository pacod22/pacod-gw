//! Stage 4: the service comes up, serves its metadata and keys, and keeps its keys.

mod common;

use chrono::{Duration, Utc};
use common::TestId;
use pacod_id::keys;
use pacod_id_jose::{Alg, JwkSet, Verifier};

#[tokio::test]
async fn discovery_answers_per_realm_by_host() {
    let id = TestId::start().await;
    let port = id.running.public.port();
    let (status, doc) = id.get_json(&format!("{}/.well-known/openid-configuration", id.public)).await;
    assert_eq!(status, 200);
    let issuer = format!("http://127.0.0.1:{port}");
    assert_eq!(doc["issuer"], issuer);
    assert_eq!(doc["authorization_endpoint"], format!("{issuer}/oidc/authorize"));
    assert_eq!(doc["code_challenge_methods_supported"], serde_json::json!(["S256"]));
    assert_eq!(doc["id_token_signing_alg_values_supported"][0], "RS256", "OIDC Discovery requires RS256");
    assert_eq!(doc["request_uri_parameter_supported"], false);
    assert_eq!(doc["authorization_response_iss_parameter_supported"], true);

    // The second realm under another name for the same address.
    let r = id
        .http
        .get(format!("{}/.well-known/openid-configuration", id.public))
        .header("host", format!("localhost:{port}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.json::<serde_json::Value>().await.unwrap()["issuer"], format!("http://localhost:{port}"));

    // No realm, no metadata.
    let r = id
        .http
        .get(format!("{}/.well-known/openid-configuration", id.public))
        .header("host", "elsewhere.example")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(r.json::<serde_json::Value>().await.unwrap()["error"]["type"], "not_found");
    id.running.stop().await;
}

#[tokio::test]
async fn the_jwks_holds_one_rs256_key_on_both_listeners() {
    let id = TestId::start().await;
    let r = id.http.get(format!("{}/oidc/jwks", id.public)).send().await.unwrap();
    assert_eq!(r.headers()["cache-control"], "public, max-age=3600");
    let set: JwkSet = r.json().await.unwrap();
    assert_eq!(set.keys.len(), 1);
    assert_eq!((set.keys[0].kty.as_str(), set.keys[0].alg.as_deref()), ("RSA", Some("RS256")));
    let internal: JwkSet =
        id.http.get(format!("{}/oidc/jwks", id.internal)).send().await.unwrap().json().await.unwrap();
    assert_eq!(internal, set);
    // A token the service signs verifies against what it publishes.
    let signer = id.running.state.keys.signer(Alg::RS256).unwrap();
    let token = signer.sign("JWT", &serde_json::json!({ "sub": "x" })).unwrap();
    Verifier::from_jwks(&set).verify(&token, &[Alg::RS256]).unwrap();
    id.running.stop().await;
}

#[tokio::test]
async fn health_readiness_and_metrics() {
    let id = TestId::start().await;
    for (path, want) in [("/healthz", "ok"), ("/readyz", "ready")] {
        let r = id.http.get(format!("{}{path}", id.metrics)).send().await.unwrap();
        assert_eq!((r.status().as_u16(), r.text().await.unwrap()), (200, want.to_owned()));
    }
    // A request on the public listener shows up in the metrics.
    id.http.get(format!("{}/oidc/jwks", id.public)).send().await.unwrap();
    let text = id.http.get(format!("{}/metrics", id.metrics)).send().await.unwrap().text().await.unwrap();
    assert!(text.contains("pid_http_requests_total"), "{text}");
    // Unknown paths answer 404 in the API's error shape.
    let (status, body) = id.get_json(&format!("{}/nope", id.internal)).await;
    assert_eq!((status, body["error"]["type"].as_str()), (404, Some("not_found")));
    id.running.stop().await;
}

#[tokio::test]
async fn keys_rotate_on_schedule() {
    let id = TestId::start().await;
    let store = id.store();
    let kek = id.running.state.boot.kek().clone();
    let first = store.keys().await.unwrap().remove(0);
    let at = |days: i64, hours: i64| first.activates_at + Duration::days(days) + Duration::hours(hours);

    // Nothing to do in the first 89 days.
    assert_eq!(keys::rotate(store, &kek, at(88, 0)).await.unwrap(), keys::Rotation::default());
    // A day before the 90 days are up, the next key is published, not yet signing.
    let r = keys::rotate(store, &kek, at(89, 1)).await.unwrap();
    assert_eq!(r.published.len(), 1);
    id.running.state.keys.reload(store, std::slice::from_ref(&kek)).await.unwrap();
    assert_eq!(id.running.state.keys.jwks().keys.len(), 2);
    assert_eq!(id.running.state.keys.signer(Alg::RS256).unwrap().kid(), first.kid);
    // After 90 days it signs; the old one is retired and still published.
    let r = keys::rotate(store, &kek, at(90, 1)).await.unwrap();
    assert_eq!(r.promoted.len(), 1);
    id.running.state.keys.reload(store, std::slice::from_ref(&kek)).await.unwrap();
    assert_ne!(id.running.state.keys.signer(Alg::RS256).unwrap().kid(), first.kid);
    let old = store.keys().await.unwrap().into_iter().find(|k| k.kid == first.kid).unwrap();
    assert_eq!(old.state, "retired");
    // Two days and an hour later it is gone.
    let r = keys::rotate(store, &kek, old.retires_at.unwrap() + Duration::minutes(1)).await.unwrap();
    assert_eq!(r.deleted, vec![first.kid.clone()]);
    id.running.state.keys.reload(store, &[kek]).await.unwrap();
    assert_eq!(id.running.state.keys.jwks().keys.len(), 1);
    id.running.stop().await;
}

#[tokio::test]
async fn an_emergency_rotation_can_revoke_the_old_key() {
    let id = TestId::start().await;
    let store = id.store();
    let kek = id.running.state.boot.kek().clone();
    let old = store.keys().await.unwrap().remove(0).kid;
    let new = keys::rotate_now(store, &kek, Alg::RS256, Some(&old)).await.unwrap();
    id.running.state.keys.reload(store, &[kek]).await.unwrap();
    let kids: Vec<String> = id.running.state.keys.jwks().keys.into_iter().map(|k| k.kid).collect();
    assert_eq!(kids, vec![new]);
    id.running.stop().await;
}

#[tokio::test]
async fn two_instances_share_one_key_and_one_maintenance_lease() {
    let db = common::fresh_database().await;
    let (a, b) = tokio::join!(TestId::start_on(&db), TestId::start_on(&db));
    let active: Vec<_> = a.store().keys().await.unwrap().into_iter().filter(|k| k.state == "active").collect();
    assert_eq!(active.len(), 1, "both started at once, one key");
    assert_eq!(a.running.state.keys.jwks(), b.running.state.keys.jwks());
    let lease = |id: &TestId| {
        let store = id.store().clone();
        let holder = id.running.state.boot.instance_id.clone();
        async move { store.job_lease("maintenance", &holder, 60).await.unwrap() }
    };
    // Whoever took it in the background keeps it; the other cannot.
    let (got_a, got_b) = (lease(&a).await, lease(&b).await);
    assert!(got_a ^ got_b, "exactly one holds the lease ({got_a}, {got_b})");
    a.running.stop().await;
    b.running.stop().await;
}

#[tokio::test]
async fn rewrap_moves_keys_under_the_new_kek() {
    use pacod_id::crypto::envelope::Kek;
    let id = TestId::start().await;
    let store = id.store().clone();
    let old = id.running.state.boot.kek().clone();
    let new = Kek::new("next", [9; 32]);
    assert_eq!(keys::rewrap(&store, &[new.clone(), old.clone()]).await.unwrap(), 1);
    assert_eq!(keys::rewrap(&store, &[new.clone(), old]).await.unwrap(), 0, "nothing left to move");
    // Readable with the new key alone.
    keys::KeyRing::load(&store, &[new]).await.unwrap();
    id.running.stop().await;
}

#[tokio::test]
async fn client_secrets_from_the_configuration_are_stored_as_hmacs() {
    let id = TestId::start().await;
    let secrets = id.store().client_secrets().await.unwrap();
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0].0, "console");
    assert_eq!(secrets[0].1, id.running.state.boot.token_pepper.hash(common::CLIENT_SECRET).to_vec());
    id.running.stop().await;
}

#[tokio::test]
async fn throttles_count_lock_and_clear() {
    let id = TestId::start().await;
    let s = id.store();
    for n in 1..=2 {
        let hit = s.throttle_hit("login_account", "a@example.com", 900, 3, 900).await.unwrap();
        assert_eq!((hit.count, hit.locked_until), (n, None));
    }
    let hit = s.throttle_hit("login_account", "a@example.com", 900, 3, 900).await.unwrap();
    assert!(hit.locked_until.is_some_and(|t| t > Utc::now()));
    assert!(s.throttle_locked("login_account", "a@example.com").await.unwrap().is_some());
    assert!(s.throttle_locked("login_account", "b@example.com").await.unwrap().is_none());
    s.throttle_clear("login_account", "a@example.com").await.unwrap();
    assert!(s.throttle_locked("login_account", "a@example.com").await.unwrap().is_none());
    id.running.stop().await;
}
