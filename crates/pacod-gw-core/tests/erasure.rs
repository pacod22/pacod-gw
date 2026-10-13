//! `POST /orgs/{org}/workspaces/{ws}/erasure` (identity concept 6.9): the core checks who may
//! erase and what, then has the analytics service erase one user's data in the workspace.

mod common;

use std::sync::{Arc, Mutex};

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use common::TestGw;
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
