//! `DELETE /erasure`: one user's usage events, audit records and end-user rollups in one
//! workspace, by the `user_id` or the `end_user` the calls carried.

use pacod_gw_events::AuditRecord;
use reqwest::StatusCode;
use serde_json::Value;

use crate::support::{TestApp, app_with_fixture, audit_record};

async fn erase(app: &TestApp, query: &str) -> reqwest::Response {
    app.request(reqwest::Method::DELETE, &format!("/erasure?{query}")).send().await.expect("answer")
}

/// How many rows `from` (a table and its conditions) has. ClickHouse may quote 64-bit numbers.
async fn count(app: &TestApp, from: &str) -> u64 {
    let n = &app.select(&format!("SELECT count() AS n FROM {from}")).await[0]["n"];
    n.as_u64().or_else(|| n.as_str()?.parse().ok()).expect("a count")
}

#[tokio::test]
async fn one_user_s_data_goes_and_nobody_else_s() {
    let app = app_with_fixture().await;
    let bob = AuditRecord {
        end_user: "bob".into(),
        user_id: "u-2".into(),
        ..audit_record("aud-b", 3, "2026-03-12T08:00:00Z")
    };
    let elsewhere = AuditRecord { workspace_id: "ws-2".into(), ..audit_record("aud-e", 9, "2026-03-12T09:00:00Z") };
    app.store_audit(
        "audit-1",
        &[
            audit_record("aud-1", 1, "2026-03-12T10:05:00Z"),
            audit_record("aud-2", 2, "2026-03-12T10:20:00Z"),
            bob,
            elsewhere,
        ],
    )
    .await;
    for end_user in ["alice", "bob"] {
        app.command(&format!(
            "INSERT INTO usage_daily_end_users (bucket, organisation_id, workspace_id, end_user, requests, generation)
             VALUES ('2026-03-12 00:00:00', 'org-a', 'ws-1', '{end_user}', 1, 1)"
        ))
        .await;
    }

    // The workspace and exactly one of user_id and end_user, none of them empty.
    for query in [
        "user_id=u-1",
        "organisation_id=org-a&user_id=u-1",
        "organisation_id=org-a&workspace_id=ws-1",
        "organisation_id=org-a&workspace_id=ws-1&user_id=u-1&end_user=alice",
        "organisation_id=org-a&workspace_id=ws-1&user_id=",
        "organisation_id=&workspace_id=ws-1&user_id=u-1",
        "organisation_id=org-a&workspace_id=ws-1&user_id=u-1&model=x",
    ] {
        assert_eq!(erase(&app, query).await.status(), StatusCode::BAD_REQUEST, "{query}");
    }

    let events_of = |column: &'static str, value: &'static str| {
        let app = &app;
        async move { count(app, &format!("usage_events FINAL WHERE workspace_id = 'ws-1' AND {column} = '{value}'")).await }
    };
    let u1_events = events_of("user_id", "u-1").await;
    assert!(u1_events > 0);
    let others = count(&app, "usage_events FINAL WHERE user_id != 'u-1' OR workspace_id != 'ws-1'").await;

    let answer: Value = erase(&app, "organisation_id=org-a&workspace_id=ws-1&user_id=u-1").await.json().await.unwrap();
    assert_eq!(
        (answer["usage_events"].as_u64(), answer["audit_records"].as_u64(), answer["end_user_rollups"].as_u64()),
        (Some(u1_events), Some(2), Some(0)),
        "{answer}"
    );
    assert_eq!(events_of("user_id", "u-1").await, 0);
    assert_eq!(count(&app, "usage_events").await, others, "nobody else's");
    let left: Vec<String> = app
        .select("SELECT audit_id FROM message_audit ORDER BY audit_id")
        .await
        .iter()
        .map(|r| r["audit_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(left, ["aud-b", "aud-e"], "the same user in another workspace stays");

    // By end user: the rollups go too.
    let bob_events = events_of("end_user", "bob").await;
    let answer: Value = erase(&app, "organisation_id=org-a&workspace_id=ws-1&end_user=bob").await.json().await.unwrap();
    assert_eq!(
        (answer["usage_events"].as_u64(), answer["audit_records"].as_u64(), answer["end_user_rollups"].as_u64()),
        (Some(bob_events), Some(1), Some(1)),
        "{answer}"
    );
    assert_eq!(count(&app, "usage_daily_end_users WHERE end_user = 'bob'").await, 0);
    assert_eq!(count(&app, "usage_daily_end_users WHERE end_user = 'alice'").await, 1);

    // Again: nothing left to erase.
    let answer: Value = erase(&app, "organisation_id=org-a&workspace_id=ws-1&end_user=bob").await.json().await.unwrap();
    assert_eq!(answer, serde_json::json!({ "usage_events": 0, "audit_records": 0, "end_user_rollups": 0 }));
}
