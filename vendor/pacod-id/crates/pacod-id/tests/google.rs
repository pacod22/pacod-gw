//! Stage 7: Google sign-in, against a mock OpenID provider.

mod common;

use common::{Browser, TestId, authorize_url, param, pkce};
use pacod_id_mock_idp::{Identity, MockIdp};
use serde_json::json;

const GATEWAY_SECRET: &str = "google-secret-of-the-gateway-realm";
const BAZAAR_SECRET: &str = "google-secret-of-the-bazaar-realm";
const BAZAAR_REDIRECT: &str = "https://bazaar.example/native/oidc/callback";

struct World {
    id: TestId,
    google: MockIdp,
}

/// pacod-id whose two realms sign in with the mock provider, each with its own client.
async fn world() -> World {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let google = MockIdp::new(&issuer, &[("gw-client", GATEWAY_SECRET), ("bazaar-client", BAZAAR_SECRET)]).unwrap();
    tokio::spawn(google.clone().serve(listener));
    let db = common::fresh_database().await;
    let id = TestId::start_full(
        &db,
        |port| {
            common::config_for(port)
                .replace(
                    "accounts_managed_by = [\"gw.platform_admin\"]\n",
                    &format!("accounts_managed_by = [\"gw.platform_admin\"]\ngoogle = {{ client_id = \"gw-client\", secret_env = \"G1\", issuer = \"{issuer}\" }}\n"),
                )
                .replace(
                    "self_signup = true\n",
                    &format!("self_signup = true\ngoogle = {{ client_id = \"bazaar-client\", secret_env = \"G2\", issuer = \"{issuer}\" }}\n"),
                )
        },
        |boot| {
            boot.google_secrets.insert("gateway".into(), GATEWAY_SECRET.into());
            boot.google_secrets.insert("bazaar".into(), BAZAAR_SECRET.into());
        },
    )
    .await;
    World { id, google }
}

/// localhost (the bazaar realm) reached on 127.0.0.1; the browser sends the Host.
fn local(url: &str) -> String {
    url.replacen("://localhost:", "://127.0.0.1:", 1)
}

/// A sign-in flow in `b` for the console (gateway) or the bazaar app.
async fn flow(id: &TestId, b: &mut Browser, bazaar: bool) -> String {
    let (_, challenge) = pkce();
    let extra: Vec<(&str, &str)> =
        if bazaar { vec![("client_id", "bazaar-android"), ("redirect_uri", BAZAAR_REDIRECT)] } else { vec![] };
    let to = b.get(&authorize_url(id, &challenge, &extra)).await.location.unwrap();
    param(&to, "flow").unwrap()
}

/// Starts Google in a flow and follows the browser through the provider back to pacod-id;
/// where pacod-id sent the browser.
async fn through_google(b: &mut Browser, flow: &str) -> String {
    let a = b.post(&local(&format!("{}/ui-api/v1/flows/{flow}/google", b.origin)), json!({})).await;
    assert_eq!(a.json["next"], "redirect", "{a:?}");
    let at_google = b.get(a.json["to"].as_str().unwrap()).await;
    let back = at_google.location.expect("the provider sends the browser back");
    let done = b.get(&local(&back)).await;
    done.location.clone().unwrap_or_else(|| panic!("pacod-id sent the browser nowhere: {done:?}"))
}

async fn federated(id: &TestId, account: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT provider_subject FROM federated_identities WHERE account_id = $1")
        .bind(account)
        .fetch_all(id.store().pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_new_bazaar_user_signs_up_with_google_and_comes_back_by_subject() {
    let World { id, google } = world().await;
    let mut b = Browser::bazaar(&id);
    let f = flow(&id, &mut b, true).await;
    google.queue(Identity::new("g-111", "Nia@Example.com", true));
    let to = through_google(&mut b, &f).await;
    assert!(to.starts_with(BAZAAR_REDIRECT) && param(&to, "code").is_some(), "{to}");
    let seen = &google.seen()[0];
    assert_eq!(
        (seen["client_id"].as_str(), seen["code_challenge_method"].as_str()),
        ("bazaar-client", "S256"),
        "the realm's own client, with PKCE"
    );
    assert!(seen["redirect_uri"].starts_with("http://localhost:") && seen.contains_key("nonce"));
    let nia = id.store().account_by_email("nia@example.com").await.unwrap().expect("made");
    assert_eq!(
        (nia.email_verified_source.as_str(), nia.signup_realm.as_str(), nia.password_hash.is_none()),
        ("federated", "bazaar", true)
    );
    assert_eq!(federated(&id, &nia.id).await, ["g-111"]);

    // Google's address changed: still the same account, found by subject.
    let mut b = Browser::bazaar(&id);
    let f = flow(&id, &mut b, true).await;
    google.queue(Identity::new("g-111", "nia.new@example.com", true));
    let to = through_google(&mut b, &f).await;
    assert!(param(&to, "code").is_some(), "{to}");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts").fetch_one(id.store().pool()).await.unwrap();
    assert_eq!(n, 1);
    id.running.stop().await;
}

#[tokio::test]
async fn the_gateway_makes_accounts_by_invitation_only() {
    let World { id, google } = world().await;
    let mut b = Browser::new(&id);
    let f = flow(&id, &mut b, false).await;
    google.queue(Identity::new("g-222", "eve@example.com", true));
    let to = through_google(&mut b, &f).await;
    assert_eq!(
        (param(&to, "error").as_deref(), param(&to, "flow").as_deref()),
        (Some("google_no_account"), Some(f.as_str())),
        "{to}"
    );
    assert!(id.store().account_by_email("eve@example.com").await.unwrap().is_none());

    // With a pending invitation, Google makes the account.
    id.sql("INSERT INTO invitations (id, token_hash, email, role, scope_type, scope_id, realm, expires_at)
            VALUES ('inv_1', '\\x01', 'eve@example.com', 'gw.viewer', 'platform', '', 'gateway', now() + interval '1 day')")
        .await;
    let f = flow(&id, &mut b, false).await;
    google.queue(Identity::new("g-222", "eve@example.com", true));
    let to = through_google(&mut b, &f).await;
    assert!(to.starts_with(common::CONSOLE_REDIRECT) && param(&to, "code").is_some(), "{to}");
    let (_, tokens) = id.redeem(&param(&to, "code").unwrap(), "x").await;
    assert_eq!(tokens["error"], "invalid_grant", "the PKCE verifier still counts");
    id.running.stop().await;
}

#[tokio::test]
async fn linking_by_address_needs_proof_on_both_sides() {
    let World { id, google } = world().await;
    let ada = common::account(&id, "ada@example.com").await;
    let bob = common::account_with(&id, "bob@example.com", common::PASSWORD, false).await;

    // Both sides proved the address: linked at once.
    let mut b = Browser::new(&id);
    let f = flow(&id, &mut b, false).await;
    google.queue(Identity::new("g-ada", "ada@example.com", true));
    let to = through_google(&mut b, &f).await;
    assert!(param(&to, "code").is_some(), "{to}");
    assert_eq!(federated(&id, &ada.id).await, ["g-ada"]);

    // Google did not prove it: nothing is linked.
    let mut c = Browser::new(&id);
    let f = flow(&id, &mut c, false).await;
    google.queue(Identity::new("g-x", "bob@example.com", false));
    let to = through_google(&mut c, &f).await;
    assert_eq!(param(&to, "error").as_deref(), Some("google_unverified"));

    // The account's address is unproven: its password links it, and proves the address.
    let mut d = Browser::new(&id);
    let f = flow(&id, &mut d, false).await;
    google.queue(Identity::new("g-bob", "bob@example.com", true));
    let to = through_google(&mut d, &f).await;
    assert_eq!(
        (param(&to, "link").as_deref(), param(&to, "email").as_deref()),
        (Some("google"), Some("bob@example.com")),
        "{to}"
    );
    assert!(federated(&id, &bob.id).await.is_empty(), "not before the password");
    let a = d.get(&format!("{}/ui-api/v1/flows/{f}", id.public)).await;
    assert_eq!(
        (a.json["stage"].as_str(), a.json["link_email"].as_str(), a.json["google"].as_bool()),
        (Some("link_google"), Some("bob@example.com"), Some(true))
    );
    let password = format!("{}/ui-api/v1/flows/{f}/password", id.public);
    let a = d.post(&password, json!({ "email": "ada@example.com", "password": common::PASSWORD })).await;
    assert_eq!(a.json["error"]["details"]["code"], "wrong_account");
    let a = d.post(&password, json!({ "email": "bob@example.com", "password": "not it" })).await;
    assert_eq!(a.status, 401);
    let a = d.post(&password, json!({ "email": "bob@example.com", "password": common::PASSWORD })).await;
    assert_eq!(a.json["next"], "redirect", "{a:?}");
    assert_eq!(federated(&id, &bob.id).await, ["g-bob"]);
    let bob = id.store().account(&bob.id).await.unwrap().unwrap();
    assert_eq!(bob.email_verified_source, "federated");
    id.running.stop().await;
}

#[tokio::test]
async fn bad_answers_from_the_provider_are_refused() {
    let World { id, google } = world().await;
    common::account(&id, "ada@example.com").await;
    // Cancelled at Google.
    let mut b = Browser::new(&id);
    let f = flow(&id, &mut b, false).await;
    google.deny_next();
    let to = through_google(&mut b, &f).await;
    assert_eq!(param(&to, "error").as_deref(), Some("google_cancelled"));

    // Tokens the provider's client must refuse.
    for tamper in [
        json!({ "nonce": "another" }),
        json!({ "nonce": null }),
        json!({ "aud": "another-client" }),
        json!({ "aud": ["gw-client", "another-client"], "azp": "another-client" }),
        json!({ "iss": "https://evil.example" }),
        json!({ "exp": 1_000_000 }),
    ] {
        let mut b = Browser::new(&id);
        let f = flow(&id, &mut b, false).await;
        google.queue(Identity::new("g-ada", "ada@example.com", true));
        google.tamper_next(tamper.clone());
        let a = b.post(&format!("{}/ui-api/v1/flows/{f}/google", id.public), json!({})).await;
        let back = b.get(a.json["to"].as_str().unwrap()).await.location.unwrap();
        let done = b.get(&back).await;
        assert_eq!((done.status, done.location.as_deref()), (400, None), "{tamper}");
        let n: i64 =
            sqlx::query_scalar("SELECT count(*) FROM federated_identities").fetch_one(id.store().pool()).await.unwrap();
        assert_eq!(n, 0, "{tamper}");
    }

    // The callback in another browser, a replayed one, a forged state.
    let mut b = Browser::new(&id);
    let f = flow(&id, &mut b, false).await;
    google.queue(Identity::new("g-ada", "ada@example.com", true));
    let a = b.post(&format!("{}/ui-api/v1/flows/{f}/google", id.public), json!({})).await;
    let back = b.get(a.json["to"].as_str().unwrap()).await.location.unwrap();
    assert_eq!(Browser::new(&id).get(&back).await.status, 400, "the flow belongs to its browser");
    let forged = back.replace(&param(&back, "state").unwrap(), &format!("{f}.forged"));
    assert_eq!(b.get(&forged).await.status, 400);
    // A forged state costs the real sign-in nothing; the real callback works once.
    let done = b.get(&back).await;
    assert!(done.location.as_deref().is_some_and(|l| l.starts_with(common::CONSOLE_REDIRECT)), "{done:?}");
    assert_eq!(b.get(&back).await.status, 400, "replayed");
    id.running.stop().await;
}

#[tokio::test]
async fn an_account_keeps_a_way_to_sign_in() {
    let World { id, google } = world().await;
    let mut b = Browser::bazaar(&id);
    let f = flow(&id, &mut b, true).await;
    google.queue(Identity::new("g-nia", "nia@example.com", true));
    through_google(&mut b, &f).await;
    let connections = format!("{}/ui-api/v1/account/connections", b.origin);
    let a = b.get(&local(&connections)).await;
    assert_eq!(
        (a.json["password"].as_bool(), a.json["google"]["email"].as_str()),
        (Some(false), Some("nia@example.com")),
        "{a:?}"
    );
    let a = b.delete_json(&local(&format!("{connections}/google"))).await;
    assert_eq!((a.status, a.json["error"]["details"]["code"].as_str()), (409, Some("last_sign_in_method")));
    // With a password, Google can go.
    let nia = id.store().account_by_email("nia@example.com").await.unwrap().unwrap();
    let hash = pacod_id::crypto::password::hash(common::PASSWORD, Default::default()).unwrap();
    id.store().account_set_password(&nia.id, &hash).await.unwrap();
    let a = b.delete_json(&local(&format!("{connections}/google"))).await;
    assert_eq!(a.status, 204);
    assert!(federated(&id, &nia.id).await.is_empty());
    // A disabled account does not sign in with Google either.
    id.sql(&format!(
        "INSERT INTO federated_identities (provider, provider_subject, account_id) VALUES ('google', 'g-nia', '{}')",
        nia.id
    ))
    .await;
    id.sql(&format!("UPDATE accounts SET status = 'disabled' WHERE id = '{}'", nia.id)).await;
    let mut c = Browser::bazaar(&id);
    let f = flow(&id, &mut c, true).await;
    google.queue(Identity::new("g-nia", "nia@example.com", true));
    let to = through_google(&mut c, &f).await;
    assert_eq!(param(&to, "error").as_deref(), Some("account_disabled"));
    id.running.stop().await;
}
