//! Audit records: round trip, retention, the list without bodies, and erasure.

use pacod_gw_events::ingest::AUDIT_PATH;
use pacod_gw_events::{AuditPart, AuditRecord};
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::support::{TestApp, at, audit_record, id, ndjson};

const DAY: &str = "from=2026-03-12T00:00:00Z&to=2026-03-13T00:00:00Z";

/// Five records: three of alice in two sessions, one of bob, one of another organisation.
fn records() -> Vec<AuditRecord> {
    let alice_1 = audit_record("aud-1", 1, "2026-03-12T10:05:00Z");
    let alice_2 = AuditRecord { trace_id: "trace-3".into(), ..audit_record("aud-2", 2, "2026-03-12T10:20:00Z") };
    let alice_3 = AuditRecord {
        session_id: "sess-3".into(),
        trace_id: "trace-4".into(),
        template: "writer".into(),
        model: "claude-sonnet".into(),
        agent_id: "writer-bot".into(),
        task: "write".into(),
        complexity: "medium".into(),
        ..audit_record("aud-3", 5, "2026-03-12T11:00:00Z")
    };
    let bob = AuditRecord {
        key_id: "key-2".into(),
        end_user: "bob".into(),
        session_id: "sess-2".into(),
        trace_id: "trace-2".into(),
        template: String::new(),
        model: "gpt-mini".into(),
        mode: "proxy".into(),
        agent_id: "chat-bot".into(),
        user_id: "u-2".into(),
        task: "chat".into(),
        complexity: "low".into(),
        ..audit_record("aud-4", 3, "2026-03-12T08:00:00Z")
    };
    let other_organisation = AuditRecord {
        organisation_id: "org-b".into(),
        workspace_id: "ws-9".into(),
        key_id: "key-9".into(),
        end_user: "zed".into(),
        // The same session id as alice's, in another organisation.
        session_id: "sess-1".into(),
        trace_id: "trace-9".into(),
        ..audit_record("aud-5", 7, "2026-03-12T10:10:00Z")
    };
    vec![alice_1, alice_2, alice_3, bob, other_organisation]
}

async fn app_with_records() -> TestApp {
    let app = TestApp::start().await;
    app.store_audit("audit-1", &records()).await;
    app
}

/// The audit ids of a list response, in order.
fn audit_ids(page: &Value) -> Vec<&str> {
    page["records"]
        .as_array()
        .expect("records")
        .iter()
        .map(|record| record["audit_id"].as_str().expect("audit_id"))
        .collect()
}

#[tokio::test]
async fn an_audit_record_survives_storage_in_full() {
    let app = TestApp::start().await;

    let stream_body = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n".repeat(3);
    let record = AuditRecord {
        audit_id: "aud_01JABC/with space".into(),
        event_id: id(1),
        timestamp: at("2026-03-12T10:05:00.123456789Z"),
        expires_at: at("2099-06-30T23:59:59.987654321Z"),
        organisation_id: "org-a".into(),
        workspace_id: "ws-1".into(),
        key_id: "key-1".into(),
        end_user: "alice".into(),
        session_id: "sess-1".into(),
        trace_id: "trace-1".into(),
        agent_id: "planner-bot".into(),
        user_id: "staff 'ü'".into(),
        task: "plan".into(),
        complexity: "high".into(),
        template: "planner".into(),
        model: "claude-haiku".into(),
        mode: "managed".into(),
        client_request: AuditPart {
            method: "POST".into(),
            url: "/v1/chat/completions?stream=true".into(),
            status: 0,
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("x-gw-audit".into(), "1".into()),
                ("x-repeated".into(), "a".into()),
                ("x-repeated".into(), "b".into()),
            ],
            body: "{\"model\":\"@planner\",\"messages\":[{\"role\":\"user\",\"content\":\"Zürich \\\"quoted\\\" \\n 😀\"}]}".into(),
            body_encoding: "utf8".into(),
            body_bytes: 88,
            same_as: String::new(),
            chunks: Vec::new(),
            truncated: false,
        },
        provider_requests: vec![
            AuditPart {
                method: "POST".into(),
                url: "https://api.anthropic.com/v1/messages".into(),
                body: "{\"model\":\"claude-haiku\",\"max_tokens\":400}".into(),
                body_encoding: "utf8".into(),
                body_bytes: 41,
                ..AuditPart::default()
            },
            AuditPart {
                method: "POST".into(),
                url: "https://api.anthropic.com/v1/messages".into(),
                body_encoding: "ref".into(),
                same_as: "provider_requests[0]".into(),
                body_bytes: 41,
                ..AuditPart::default()
            },
        ],
        provider_responses: vec![
            AuditPart {
                status: 529,
                headers: vec![("retry-after".into(), "1".into())],
                body: "{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}".into(),
                body_encoding: "utf8".into(),
                body_bytes: 52,
                ..AuditPart::default()
            },
            AuditPart {
                status: 200,
                headers: vec![("content-type".into(), "text/event-stream".into())],
                body_bytes: stream_body.len() as u64,
                body: stream_body,
                body_encoding: "utf8".into(),
                chunks: vec![(480_200, 57), (481_950, 57), (2_000_800, 57)],
                ..AuditPart::default()
            },
        ],
        client_response: AuditPart {
            status: 200,
            headers: vec![("x-gw-delay-ms".into(), "1.25".into())],
            // A body that is not text travels as base64.
            body: "AAECAwT//v0=".into(),
            body_encoding: "base64".into(),
            body_bytes: 8,
            chunks: vec![(480_900, 8)],
            truncated: true,
            ..AuditPart::default()
        },
        resolution: json!({
            "template": { "name": "planner", "version": 7 },
            "parameters": { "max_tokens": { "value": 400, "from": "template ceiling" } },
            "dropped": ["top_k"],
            "limits": [{ "name": "monthly-budget", "state": "ok", "used": 0.25 }],
        }),
        timeline: json!([["received", 0], ["provider_start", 900], ["first_byte", 480_200]]),
        outcome: json!({
            "usage": { "input_tokens": 812, "output_tokens": 350 },
            "cost": 0.012,
            "finish_reason": "stop",
            "error": null,
        }),
        truncated: true,
    };

    app.store_audit("audit-full-1", std::slice::from_ref(&record)).await;

    // The id contains a slash and a space: it is one path segment when percent-encoded.
    let response = app.get("/reports/audit/aud_01JABC%2Fwith%20space").await;
    assert_eq!(response.status(), StatusCode::OK);
    let stored: AuditRecord = response.json().await.expect("an audit record");
    assert_eq!(stored, record);

    // The four parts and the three JSON values are text columns compressed with ZSTD.
    let columns = app
        .select(
            "SELECT name, type, compression_codec FROM system.columns \
             WHERE database = currentDatabase() AND table = 'message_audit' ORDER BY position",
        )
        .await;
    let text_columns: Vec<&str> = columns
        .iter()
        .filter(|column| column["compression_codec"].as_str().expect("codec").contains("ZSTD"))
        .map(|column| {
            assert_eq!(column["type"], "String");
            column["name"].as_str().expect("name")
        })
        .collect();
    assert_eq!(
        text_columns,
        [
            "client_request",
            "provider_requests",
            "provider_responses",
            "client_response",
            "resolution",
            "timeline",
            "outcome"
        ]
    );

    // What is stored in such a column is the JSON of the part.
    let row = app.select("SELECT client_response, timeline FROM message_audit").await;
    let part: AuditPart = serde_json::from_str(row[0]["client_response"].as_str().expect("text")).expect("a part");
    assert_eq!(part, record.client_response);
    assert_eq!(row[0]["timeline"], "[[\"received\",0],[\"provider_start\",900],[\"first_byte\",480200]]");
}

#[tokio::test]
async fn expires_at_sets_the_ttl_and_expired_records_are_gone() {
    let app = TestApp::start().await;

    let keep = audit_record("aud-keep", 1, "2026-03-12T10:05:00Z");
    let expired = AuditRecord {
        expires_at: at("2026-03-26T10:05:00.5Z"),
        ..audit_record("aud-expired", 2, "2026-03-12T10:06:00Z")
    };
    // Merges are stopped so that the expired row is still on disk for the first half.
    app.command("SYSTEM STOP MERGES message_audit").await;
    app.store_audit("ttl-1", &[keep.clone(), expired]).await;

    // The column holds exactly the record's `expires_at`, and it is the table's TTL.
    let stored = app.select("SELECT audit_id, expires_at FROM message_audit ORDER BY audit_id").await;
    assert_eq!(
        stored,
        [
            json!({ "audit_id": "aud-expired", "expires_at": "2026-03-26T10:05:00.500000000Z" }),
            json!({ "audit_id": "aud-keep", "expires_at": "2099-01-01T00:00:00.000000000Z" }),
        ]
    );
    let table = app
        .select(
            "SELECT engine_full FROM system.tables \
             WHERE database = currentDatabase() AND name = 'message_audit'",
        )
        .await;
    let engine = table[0]["engine_full"].as_str().expect("engine");
    assert!(engine.contains("TTL expires_at"), "{engine}");
    assert!(engine.contains("ORDER BY (workspace_id, session_id, timestamp, audit_id)"), "{engine}");

    // While the expired row waits for its merge, the service already hides it.
    app.error("/reports/audit/aud-expired", StatusCode::NOT_FOUND).await;
    let page = app.json("/reports/audit?from=2026-03-12T00:00:00Z&to=2026-03-13T00:00:00Z").await;
    assert_eq!(audit_ids(&page), ["aud-keep"]);

    // The merge applies the TTL: the row is deleted from disk.
    app.command("SYSTEM START MERGES message_audit").await;
    app.command("OPTIMIZE TABLE message_audit FINAL").await;
    let stored = app.select("SELECT audit_id FROM message_audit").await;
    assert_eq!(stored, [json!({ "audit_id": "aud-keep" })]);

    let response = app.get("/reports/audit/aud-keep").await;
    assert_eq!(response.status(), StatusCode::OK);
    let record: AuditRecord = response.json().await.expect("record");
    assert_eq!(record, keep);
}

#[tokio::test]
async fn the_audit_list_has_metadata_only_filters_and_a_cursor() {
    let app = app_with_records().await;

    let page = app.json(&format!("/reports/audit?{DAY}")).await;
    assert_eq!(page["from"], "2026-03-12T00:00:00Z");
    assert_eq!(page["to"], "2026-03-13T00:00:00Z");
    assert_eq!(page["limit"], 50);
    assert_eq!(page["next_cursor"], Value::Null);
    // Newest first.
    assert_eq!(audit_ids(&page), ["aud-3", "aud-2", "aud-5", "aud-1", "aud-4"]);

    // No bodies: an entry is exactly the scalar fields of the record.
    assert_eq!(
        page["records"][3],
        json!({
            "audit_id": "aud-1",
            "event_id": id(1).to_string(),
            "timestamp": "2026-03-12T10:05:00Z",
            "expires_at": "2099-01-01T00:00:00Z",
            "organisation_id": "org-a",
            "workspace_id": "ws-1",
            "key_id": "key-1",
            "end_user": "alice",
            "session_id": "sess-1",
            "trace_id": "trace-1",
            "agent_id": "planner-bot",
            "user_id": "u-1",
            "task": "plan",
            "complexity": "high",
            "template": "planner",
            "model": "claude-haiku",
            "mode": "managed",
            "truncated": false,
        })
    );

    let cases: [(&str, &[&str]); 16] = [
        ("organisation_id=org-a", &["aud-3", "aud-2", "aud-1", "aud-4"]),
        ("organisation_id=org-b", &["aud-5"]),
        ("organisation_id=org-a&workspace_id=ws-9", &[]),
        ("end_user=alice", &["aud-3", "aud-2", "aud-1"]),
        ("end_user=bob&end_user=zed", &["aud-5", "aud-4"]),
        ("session_id=sess-1", &["aud-2", "aud-5", "aud-1"]),
        ("session_id=sess-1&organisation_id=org-a", &["aud-2", "aud-1"]),
        ("trace_id=trace-1", &["aud-1"]),
        ("model=gpt-mini", &["aud-4"]),
        ("template=writer", &["aud-3"]),
        ("mode=proxy", &["aud-4"]),
        ("key_id=key-9", &["aud-5"]),
        ("agent_id=writer-bot", &["aud-3"]),
        ("user_id=u-2", &["aud-4"]),
        ("task=chat&task=write", &["aud-3", "aud-4"]),
        ("complexity=high", &["aud-2", "aud-5", "aud-1"]),
    ];
    for (filter, expected) in cases {
        let page = app.json(&format!("/reports/audit?{DAY}&{filter}")).await;
        assert_eq!(audit_ids(&page), expected, "{filter}");
    }

    // The time range applies.
    let page = app.json("/reports/audit?from=2026-03-12T10:00:00Z&to=2026-03-12T10:15:00Z").await;
    assert_eq!(audit_ids(&page), ["aud-5", "aud-1"]);

    // Pages of two.
    let first = app.json(&format!("/reports/audit?{DAY}&limit=2")).await;
    assert_eq!(audit_ids(&first), ["aud-3", "aud-2"]);
    let cursor = first["next_cursor"].as_str().expect("a second page");
    let second = app.json(&format!("/reports/audit?{DAY}&limit=2&cursor={cursor}")).await;
    assert_eq!(audit_ids(&second), ["aud-5", "aud-1"]);
    let cursor = second["next_cursor"].as_str().expect("a third page");
    let third = app.json(&format!("/reports/audit?{DAY}&limit=2&cursor={cursor}")).await;
    assert_eq!(audit_ids(&third), ["aud-4"]);
    assert_eq!(third["next_cursor"], Value::Null);

    // Filters on columns an audit record does not have are refused, not ignored.
    for filter in ["provider=openai", "status=ok", "tag.purpose=plan"] {
        let path = format!("/reports/audit?{DAY}&{filter}");
        let error = app.error(&path, StatusCode::BAD_REQUEST).await;
        assert_eq!(error["code"], "unknown_parameter", "{filter}");
    }
}

#[tokio::test]
async fn one_audit_record_is_scoped() {
    let app = app_with_records().await;
    app.json("/reports/audit/aud-5").await;
    app.json("/reports/audit/aud-5?organisation_id=org-b&workspace_id=ws-9").await;
    app.error("/reports/audit/aud-5?organisation_id=org-a", StatusCode::NOT_FOUND).await;
    app.error("/reports/audit/unknown", StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn an_audit_batch_delivered_twice_is_stored_once() {
    let app = app_with_records().await;
    assert_eq!(app.stored_rows("message_audit").await, 5);

    // Same batch id: dropped on insert.
    app.store_audit("audit-1", &records()).await;
    assert_eq!(app.stored_rows("message_audit").await, 5);

    // Another batch id: stored again, and still listed once.
    app.command("SYSTEM STOP MERGES message_audit").await;
    app.store_audit("audit-2", &records()).await;
    assert_eq!(app.stored_rows("message_audit").await, 10);
    let page = app.json(&format!("/reports/audit?{DAY}")).await;
    assert_eq!(audit_ids(&page), ["aud-3", "aud-2", "aud-5", "aud-1", "aud-4"]);
    app.json("/reports/audit/aud-1").await;
}

#[tokio::test]
async fn a_malformed_audit_line_is_named() {
    let app = TestApp::start().await;
    let body = format!("{}{}", ndjson(&records()[..2]), "{\"audit_id\":\"aud-x\",\"event_id\":\"nope\"}\n");
    let response = app.post_batch(AUDIT_PATH, "audit-bad-1", body).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let reply: Value = response.json().await.expect("JSON error");
    assert_eq!(reply["error"]["code"], "malformed_line");
    assert_eq!(reply["error"]["line"], 3);
    assert_eq!(app.stored_rows("message_audit").await, 0);
}

async fn erase(app: &TestApp, query: &str) -> reqwest::Response {
    app.request(reqwest::Method::DELETE, &format!("/audit?{query}")).send().await.expect("answer")
}

async fn erased(app: &TestApp, query: &str) -> u64 {
    let response = erase(app, query).await;
    assert_eq!(response.status(), StatusCode::OK, "DELETE /audit?{query}");
    let reply: Value = response.json().await.expect("JSON");
    reply["deleted"].as_u64().expect("deleted")
}

#[tokio::test]
async fn audit_records_are_erased_by_end_user_or_session() {
    let app = app_with_records().await;

    // An erasure must say whose records, and never matches on an empty value.
    for query in ["", "organisation_id=org-a", "end_user=", "session_id="] {
        let response = erase(&app, query).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
    }
    let response = erase(&app, "end_user=alice&model=claude-haiku").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(app.stored_rows("message_audit").await, 5);

    // Out of scope: alice has no records in org-b.
    assert_eq!(erased(&app, "end_user=alice&organisation_id=org-b").await, 0);
    assert_eq!(app.stored_rows("message_audit").await, 5);

    // One session, within a scope: sess-1 of org-a is aud-1 and aud-2; aud-5 has the same
    // session id in org-b and stays.
    assert_eq!(erased(&app, "session_id=sess-1&organisation_id=org-a").await, 2);
    let page = app.json(&format!("/reports/audit?{DAY}")).await;
    assert_eq!(audit_ids(&page), ["aud-3", "aud-5", "aud-4"]);
    app.error("/reports/audit/aud-1", StatusCode::NOT_FOUND).await;

    // One end user: what is left of alice is aud-3.
    assert_eq!(erased(&app, "end_user=alice").await, 1);
    let page = app.json(&format!("/reports/audit?{DAY}")).await;
    assert_eq!(audit_ids(&page), ["aud-5", "aud-4"]);

    // Both given: both must match.
    assert_eq!(erased(&app, "end_user=bob&session_id=sess-9").await, 0);
    assert_eq!(erased(&app, "end_user=bob&session_id=sess-2").await, 1);

    // Erased means gone from the table, not hidden.
    let left = app.select("SELECT audit_id FROM message_audit").await;
    assert_eq!(left, [json!({ "audit_id": "aud-5" })]);

    // Erasing again finds nothing.
    assert_eq!(erased(&app, "end_user=alice").await, 0);
}
