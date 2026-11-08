//! The records of tool and agent calls: their list, their breakdown, the traces they belong to,
//! and their erasure.

use reqwest::StatusCode;
use serde_json::Value;

use crate::support::{TestApp, app_with_fixture, tool_calls};

const MARCH_12: &str = "from=2026-03-12T00:00:00Z&to=2026-03-13T00:00:00Z";

fn ids(page: &Value) -> Vec<u64> {
    page["tool_calls"]
        .as_array()
        .expect("tool_calls")
        .iter()
        .map(|r| u64::from_str_radix(&r["id"].as_str().unwrap()[24..], 16).unwrap())
        .collect()
}

async fn with_tool_calls() -> TestApp {
    let app = app_with_fixture().await;
    app.store_tool_calls("tools-1", &tool_calls()).await;
    app
}

#[tokio::test]
async fn the_list_filters_and_pages_newest_first() {
    let app = with_tool_calls().await;
    assert_eq!(ids(&app.json(&format!("/reports/tool-calls?{MARCH_12}")).await), [103, 102, 101]);

    // Every filter compares one column; `tool` is the name the caller used.
    for (query, expected) in [
        ("protocol=a2a", vec![103]),
        ("verdict=denied", vec![102]),
        ("tool=github__search_issues", vec![101]),
        ("target=github&status=ok", vec![101]),
        ("agent_id=planner-bot&trace_id=trace-1", vec![102, 101]),
        ("workspace_id=ws-2", vec![103]),
        ("tag.purpose=plan", vec![101]),
    ] {
        assert_eq!(ids(&app.json(&format!("/reports/tool-calls?{MARCH_12}&{query}")).await), expected, "{query}");
    }

    // A page at a time, by the cursor.
    let first = app.json(&format!("/reports/tool-calls?{MARCH_12}&limit=2")).await;
    assert_eq!(ids(&first), [103, 102]);
    let cursor = first["next_cursor"].as_str().expect("a next page");
    let second = app.json(&format!("/reports/tool-calls?{MARCH_12}&limit=2&cursor={cursor}")).await;
    assert_eq!(ids(&second), [101]);
    assert!(second["next_cursor"].is_null());

    // A model's column is not a filter here.
    app.error(&format!("/reports/tool-calls?{MARCH_12}&model=x"), StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn the_breakdown_counts_refusals_and_failed_results() {
    let app = with_tool_calls().await;
    let by_verdict = app.json(&format!("/reports/tool-calls/breakdown?{MARCH_12}&by=verdict")).await;
    let rows: Vec<(String, u64, u64, u64)> = by_verdict["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["group"].as_str().unwrap().to_owned(),
                r["calls"].as_u64().unwrap(),
                r["refused"].as_u64().unwrap(),
                r["result_errors"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(rows, [("allowed".to_owned(), 2, 0, 1), ("denied".to_owned(), 1, 1, 0)]);

    let csv = app.get(&format!("/reports/tool-calls/breakdown?{MARCH_12}&by=agent_id&format=csv")).await;
    assert_eq!(csv.status(), StatusCode::OK);
    let text = csv.text().await.unwrap();
    assert!(text.starts_with("group,calls,errors,refused,result_errors"), "{text}");
    app.error(&format!("/reports/tool-calls/breakdown?{MARCH_12}"), StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn a_trace_shows_its_tool_calls_and_may_have_nothing_else() {
    let app = with_tool_calls().await;
    // trace-1 has model calls and two tool calls; the totals are the model calls'.
    let trace = app.json("/reports/traces/trace-1").await;
    assert_eq!(ids(&trace), [101, 102]);
    assert_eq!(trace["totals"]["requests"], 2);
    // trace-a2a has a tool call and no model call: it is found, and its times are the call's.
    let trace = app.json("/reports/traces/trace-a2a").await;
    assert_eq!(ids(&trace), [103]);
    assert_eq!(trace["totals"]["requests"], 0);
    assert_eq!(trace["totals"]["first_request_at"], "2026-03-12T11:00:00Z");
    assert_eq!(trace["totals"]["last_request_at"], "2026-03-12T11:00:00Z");
    // Out of scope it is not found.
    app.error("/reports/traces/trace-a2a?workspace_id=ws-1", StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn an_erasure_takes_the_users_tool_calls_and_nobody_elses() {
    let app = with_tool_calls().await;
    let answer: Value = app
        .request(reqwest::Method::DELETE, "/erasure?organisation_id=org-a&workspace_id=ws-1&user_id=u-1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answer["tool_calls"], 2, "{answer}");
    assert_eq!(ids(&app.json(&format!("/reports/tool-calls?{MARCH_12}")).await), [103]);
}
