//! `POST /orgs/{org}/workspaces/{ws}/erasure`: the core checks who may
//! erase and what, then has the analytics service erase one user's data in the workspace.

mod common;

use std::sync::{Arc, Mutex};

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use common::TestGw;
use pacod_gw_core::store::responses::StoredResponse;
use serde_json::{Value, json};

/// The analytics service as far as erasure goes: it records each query and answers counts, or
/// fails when told to.
#[derive(Default)]
struct Analytics {
    seen: Mutex<Vec<String>>,
    failing: Mutex<bool>,
}

async fn start_analytics() -> (String, Arc<Analytics>) {
    async fn erase(
        State(a): State<Arc<Analytics>>,
        headers: HeaderMap,
        RawQuery(q): RawQuery,
    ) -> (StatusCode, axum::Json<Value>) {
        assert_eq!(headers["x-gw-internal-token"], "test-internal-token-0123456789");
        a.seen.lock().unwrap().push(q.unwrap_or_default());
        if *a.failing.lock().unwrap() {
            return (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "error": { "code": "storage_error" } })));
        }
        (StatusCode::OK, axum::Json(json!({ "usage_events": 3, "audit_records": 1, "end_user_rollups": 0 })))
    }
    let state = Arc::new(Analytics::default());
    let app = axum::Router::new().route("/erasure", axum::routing::delete(erase)).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, state)
}

#[tokio::test]
async fn a_workspace_s_admins_erase_one_user_s_data() {
    let (url, analytics) = start_analytics().await;
    let db = common::fresh_database().await;
    let gw = TestGw::start_on(&db, None, |boot| boot.analytics_url = Some(url)).await;
    let org = gw.create("/orgs", json!({ "name": "acme" })).await.id;
    let ws = gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "prod" })).await.id;
    let path = format!("/orgs/{org}/workspaces/{ws}/erasure");

    // A viewer may not; the workspace must exist; exactly one of the two, not empty.
    let viewer = gw.create("/users", json!({ "email": "vic@example.test", "password": "a long password 1" })).await.id;
    gw.create(
        "/role-bindings",
        json!({ "principal": viewer, "role": "viewer", "scope": { "type": "workspace", "id": ws } }),
    )
    .await;
    let vic = gw.login("vic@example.test", "a long password 1").await;
    assert_eq!(gw.api_as(&vic, "POST", &path, Some(json!({ "user_id": "u-1" }))).await.0, 403);
    assert_eq!(
        gw.api("POST", &format!("/orgs/{org}/workspaces/ws_nope/erasure"), Some(json!({ "user_id": "u-1" }))).await.0,
        404
    );
    for body in
        [json!({}), json!({ "user_id": "" }), json!({ "user_id": "u-1", "end_user": "alice" }), json!({ "who": "u-1" })]
    {
        assert_eq!(gw.api("POST", &path, Some(body.clone())).await.0, 422, "{body}");
    }
    assert!(analytics.seen.lock().unwrap().is_empty(), "nothing reached the analytics service");

    let (s, v) = gw.api("POST", &path, Some(json!({ "user_id": "u-1 & co" }))).await;
    assert_eq!((s, v["usage_events"].as_u64(), v["audit_records"].as_u64()), (200, Some(3), Some(1)), "{v}");
    assert_eq!(
        analytics.seen.lock().unwrap().as_slice(),
        [format!("organisation_id={org}&workspace_id={ws}&user_id=u-1+%26+co")],
        "the workspace of the path, the value encoded"
    );
    // Recorded, without whose data it was.
    let (_, log) = gw.api("GET", "/audit-log?limit=5", None).await;
    let entry = log["items"].as_array().unwrap().iter().find(|e| e["action"] == "erasure").expect("an entry").clone();
    assert_eq!((entry["detail"]["by"].as_str(), entry["workspace_id"].as_str()), (Some("user_id"), Some(ws.as_str())));
    assert!(!entry.to_string().contains("u-1"), "{entry}");

    // The analytics service failing: nothing is claimed.
    *analytics.failing.lock().unwrap() = true;
    assert_eq!(gw.api("POST", &path, Some(json!({ "end_user": "alice" }))).await.0, 503);
}

fn stored(id: &str, ws: &str, end_user: &str, user_id: &str) -> StoredResponse {
    let now = chrono::Utc::now();
    StoredResponse {
        id: id.into(),
        organisation_id: String::new(),
        workspace_id: ws.into(),
        key_id: "key_1".into(),
        end_user: end_user.into(),
        user_id: user_id.into(),
        previous_id: None,
        conversation_id: None,
        model: "openai/gpt".into(),
        status: "completed".into(),
        input: b"[]".to_vec(),
        response: b"{}".to_vec(),
        created_at: now,
        expires_at: now + chrono::Duration::days(30),
    }
}

#[tokio::test]
async fn stored_responses_go_with_an_erasure_and_with_their_workspace() {
    let (url, _analytics) = start_analytics().await;
    let db = common::fresh_database().await;
    let gw = TestGw::start_on(&db, None, |boot| boot.analytics_url = Some(url)).await;
    let org = gw.create("/orgs", json!({ "name": "acme" })).await.id;
    let ws = gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "prod" })).await.id;
    let other = gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "test" })).await.id;
    let store = gw.app.gw.objects.clone().unwrap();
    for r in [
        stored("resp_1", &ws, "alice", ""),
        stored("resp_2", &ws, "alice", "u-1"),
        stored("resp_3", &ws, "bob", "u-1"),
        stored("resp_4", &other, "alice", "u-1"),
    ] {
        store.response_keep(&r).await.unwrap();
    }

    let path = format!("/orgs/{org}/workspaces/{ws}/erasure");
    let (s, v) = gw.api("POST", &path, Some(json!({ "end_user": "alice" }))).await;
    assert_eq!((s, v["stored_responses"].as_u64()), (200, Some(2)), "{v}");
    let (s, v) = gw.api("POST", &path, Some(json!({ "user_id": "u-1" }))).await;
    assert_eq!((s, v["stored_responses"].as_u64()), (200, Some(1)), "{v}");
    assert!(store.response(&ws, "resp_3").await.unwrap().is_none());
    assert!(store.response(&other, "resp_4").await.unwrap().is_some(), "another workspace's are not erased");
    let (_, log) = gw.api("GET", "/audit-log?limit=5", None).await;
    let entry = log["items"].as_array().unwrap().iter().find(|e| e["action"] == "erasure").expect("an entry").clone();
    assert_eq!(entry["detail"]["stored_responses"], 1, "{entry}");

    // Deleting a workspace deletes what its calls were answered.
    let (s, v) = gw.api("DELETE", &format!("/orgs/{org}/workspaces/{other}"), None).await;
    assert_eq!(s, 204, "{v}");
    assert!(store.response(&other, "resp_4").await.unwrap().is_none());
}
