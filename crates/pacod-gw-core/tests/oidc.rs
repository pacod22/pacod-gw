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
