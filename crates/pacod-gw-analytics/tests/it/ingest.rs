//! Ingest: what goes in comes back unchanged, a batch counts once however often it arrives,
//! and a bad batch is refused whole.

use std::collections::BTreeMap;

use pacod_gw_events::ingest::{BATCH_ID_HEADER, USAGE_PATH};
use pacod_gw_events::{Timing, Usage, UsageEvent};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::support::{MARCH, TestApp, app_with_fixture, at, fixture, id, ndjson};

/// An event in which every field differs from its zero value.
fn full_event() -> UsageEvent {
    UsageEvent {
        id: Uuid::parse_str("0199a8c0-1234-7abc-8def-0123456789ab").expect("uuid"),
        timestamp: at("2026-10-04T12:34:56.123456789Z"),
        instance_id: "gw-7f9c".into(),
        organisation_id: "org_acme".into(),
        workspace_id: "ws_prod".into(),
        key_id: "key_01HZX".into(),
        end_user: "user-42".into(),
        session_id: "sess-9".into(),
        trace_id: "trace-77".into(),
        tags: BTreeMap::from([
            ("purpose".to_owned(), "planner".to_owned()),
            ("odd key ' \" \\ ü".to_owned(), "line one\nline two\ttab 😀".to_owned()),
        ]),
        agent_id: "planner-bot".into(),
        user_id: "staff-7".into(),
        task: "plan the week".into(),
        complexity: "medium".into(),
        mode: "managed".into(),
        interface: "anthropic".into(),
        provider: "anthropic".into(),
        model_requested: "@planner".into(),
        model_served: "claude-haiku-4-5".into(),
        model_reported: "claude-haiku-4-5-20261001".into(),
        template: "planner".into(),
        template_version: 7,
        path: "/v1/messages?beta=true".into(),
        routing_scheme: "by-complexity".into(),
        routing_scheme_version: 3,
        routing_source: "organisation".into(),
        routing_rule: "cheap".into(),
        failovers: 1,
        models_tried: vec!["anthropic/claude-haiku-4-5".into()],
        status: "ok".into(),
        http_status: 200,
        error_type: "overloaded".into(),
        error_message: "upstream said: \"slow down\"\nsecond line".into(),
        finish_reason: "stop".into(),
        attempts: 3,
        stream: true,
        upstream_request_id: "req_011CSHoEeqs5C35K2UUqR7Fy".into(),
        timing: Timing {
            gateway_delay_ms: 1.25,
            receive_ms: 0.3,
            pre_ms: 0.9,
            connect_ms: 12.5,
            retry_ms: 250.75,
            upstream_first_byte_ms: 480.2,
            upstream_stream_ms: 1520.6,
            post_ms: 0.35,
            relay_max_ms: 0.07,
            settle_ms: 2.1,
            total_ms: 2266.9,
            plugin_ms: 87.5,
        },
        usage: Usage {
            input_tokens: 812,
            cache_read_tokens: 4096,
            cache_write_tokens: 2048,
            cache_write_5m_tokens: 1536,
            cache_write_1h_tokens: 512,
            output_tokens: 350,
            reasoning_tokens: 120,
            characters: u64::MAX,
            server_tool_calls: 2,
        },
        usage_source: "provider".into(),
        // Seventeen significant digits: the hardest case for an exact round trip.
        cost: Some(0.964_055_720_000_000_1),
        currency: "EUR".into(),
        cost_reporting: Some(0.0123),
        reporting_currency: "USD".into(),
        price_version: 41,
        price_variant: "batch".into(),
        price_missing: true,
        dropped_params: vec!["top_k".into(), "logit_bias".into()],
        audit_id: "aud_01JABC".into(),
        intent: "billing".into(),
        intent_confidence: 0.97,
        intent_engine: "cascade:jev".into(),
        plugin_verdict: "would_block".into(),
        plugin_error: "timeout".into(),
        parent_id: "gw_0199a8c0123477bc8def0123456789aa".into(),
    }
}

async fn read_event(app: &TestApp, id: Uuid) -> UsageEvent {
    let response = app.get(&format!("/reports/requests/{id}")).await;
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.expect("a usage event")
}

#[tokio::test]
async fn every_field_of_an_event_survives_storage() {
    let app = TestApp::start().await;
    let event = full_event();

    // Guard the test itself: a field added to the struct must get a non-zero value above,
    // or the comparison below would not notice a column that drops it.
    let mut blank = UsageEvent::new(Uuid::nil(), "");
    blank.timestamp = at("1970-01-01T00:00:00Z");
    let blank = serde_json::to_value(&blank).expect("json");
    for (field, value) in serde_json::to_value(&event).expect("json").as_object().expect("flat object") {
        assert_ne!(value, &blank[field], "full_event() leaves `{field}` at its zero value");
    }

    app.store_usage("round-trip-1", std::slice::from_ref(&event)).await;
    assert_eq!(read_event(&app, event.id).await, event);

    // The same holds for an event that carries nothing but its identity and scope: every
    // optional field at its zero value, and no cost.
    let mut minimal = UsageEvent::new(Uuid::from_u128(77), "gw-1");
    minimal.timestamp = at("2026-10-04T12:34:57Z");
    minimal.organisation_id = "org_acme".into();
    minimal.workspace_id = "ws_prod".into();
    minimal.key_id = "key_01HZX".into();
    app.store_usage("round-trip-2", std::slice::from_ref(&minimal)).await;
    assert_eq!(read_event(&app, minimal.id).await, minimal);

    // Every column of the table is a field of the event and the other way round.
    let columns = app
        .select("SELECT name FROM system.columns WHERE database = currentDatabase() AND table = 'usage_events' ORDER BY position")
        .await;
    let columns: Vec<&str> = columns.iter().map(|row| row["name"].as_str().expect("name")).collect();
    let event_json = serde_json::to_value(&event).expect("json");
    let fields: Vec<&str> = event_json.as_object().expect("flat object").keys().map(String::as_str).collect();
    assert_eq!(columns, fields);
}

#[tokio::test]
async fn costs_and_timings_keep_their_exact_bits() {
    let app = TestApp::start().await;

    // Realistic costs: tokens times a price per million. Most have 16 or 17 digits.
    let mut events = Vec::new();
    for n in 1..=300u32 {
        let mut event = UsageEvent::new(Uuid::from_u128(u128::from(n)), "gw-1");
        event.timestamp = at("2026-10-04T12:00:00Z") + chrono::Duration::milliseconds(i64::from(n));
        event.organisation_id = "org".into();
        event.workspace_id = "ws".into();
        event.key_id = "key".into();
        let tokens = f64::from(n * 7919 % 200_000);
        let price = f64::from(n * 104_729 % 100_000) / 1000.0;
        event.cost = Some(tokens * price / 1e6 + f64::from(n) * 1e-7);
        event.timing.gateway_delay_ms = (n as f32) * 0.37 / 3.0;
        event.timing.total_ms = (n as f32) * 1234.567 / 7.0;
        events.push(event);
    }
    app.store_usage("floats-1", &events).await;

    let page = app.json("/reports/requests?from=2026-10-04T00:00:00Z&to=2026-10-05T00:00:00Z&limit=1000").await;
    let mut stored: Vec<UsageEvent> = serde_json::from_value(page["requests"].clone()).expect("events");
    stored.sort_by_key(|event| event.id);
    assert_eq!(stored.len(), events.len());
    for (sent, got) in events.iter().zip(&stored) {
        assert_eq!(sent.cost.map(f64::to_bits), got.cost.map(f64::to_bits));
        assert_eq!(sent.timing, got.timing);
    }
}

/// Every report over the fixture, with fixed ranges so that two calls are comparable.
async fn every_report(app: &TestApp) -> Vec<(String, Value)> {
    let organisation = "organisation_id=org-a";
    let paths = [
        format!("/reports/overview?to=2026-03-12T10:30:00Z&{organisation}"),
        format!("/reports/usage?{MARCH}&bucket=day&group_by=provider"),
        format!("/reports/usage?{MARCH}&bucket=month&group_by=tag:purpose&{organisation}"),
        format!("/reports/breakdown?{MARCH}&by=model"),
        format!("/reports/breakdown?{MARCH}&by=end_user&{organisation}"),
        format!("/reports/requests?{MARCH}&limit=100"),
        format!("/reports/requests/{}", id(1)),
        "/reports/traces/trace-1".to_owned(),
        format!("/reports/end-users?{MARCH}"),
        format!("/reports/end-users/alice?{MARCH}"),
        format!("/reports/errors?{MARCH}&bucket=day"),
        format!("/reports/delay?{MARCH}&bucket=day&group_by=mode"),
        format!("/reports/accuracy?{MARCH}"),
    ];
    let mut reports = Vec::new();
    for path in paths {
        let report = app.json(&path).await;
        reports.push((path, report));
    }
    reports
}

#[tokio::test]
async fn a_batch_delivered_twice_changes_nothing() {
    let app = app_with_fixture().await;
    let rows = app.stored_rows("usage_events").await;
    assert_eq!(rows, 9);
    let before = every_report(&app).await;

    // The core did not see the 200 and sends the same batch again: ClickHouse drops it by its
    // batch id, so not even the physical row count moves.
    app.store_usage("fixture-1", &fixture()).await;
    assert_eq!(app.stored_rows("usage_events").await, 9);
    assert_eq!(every_report(&app).await, before);

    // The same events arrive in differently cut batches under new ids, as after a crash of the
    // core between two checkpoints. Now the copies are stored, and the reports still do not
    // count them. Background merges are stopped so that the copies are certain to be on disk
    // while the reports run.
    app.command("SYSTEM STOP MERGES usage_events").await;
    let events = fixture();
    app.store_usage("fixture-2a", &events[..4]).await;
    app.store_usage("fixture-2b", &events[4..]).await;
    assert_eq!(app.stored_rows("usage_events").await, 18);
    let after = every_report(&app).await;
    for ((path, was), (_, is)) in before.iter().zip(&after) {
        assert_eq!(was, is, "{path} changed after a second delivery");
    }
}

#[tokio::test]
async fn a_malformed_line_rejects_the_whole_batch_and_names_the_line() {
    let app = TestApp::start().await;
    let events = fixture();

    let cases = [
        // Not JSON at all.
        ("{\"id\": \"0199a8c0-", "EOF"),
        // JSON, but a field has the wrong type.
        ("{\"id\":\"00000000-0000-0000-0000-000000000063\",\"timestamp\":\"yesterday\"}", ""),
        // JSON, but a required field is missing.
        ("{\"timestamp\":\"2026-03-12T10:05:00Z\"}", "missing field"),
    ];
    for (number, (bad_line, expected)) in cases.into_iter().enumerate() {
        let body = format!(
            "{}\n{bad_line}\n{}\n",
            serde_json::to_string(&events[0]).expect("json"),
            serde_json::to_string(&events[1]).expect("json"),
        );
        let response = app.post_batch(USAGE_PATH, &format!("malformed-{number}"), body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let reply: Value = response.json().await.expect("JSON error");
        assert_eq!(reply["error"]["code"], "malformed_line");
        assert_eq!(reply["error"]["line"], 2);
        let message = reply["error"]["message"].as_str().expect("message");
        assert!(message.starts_with("line 2: "), "{message}");
        assert!(message.contains(expected), "{message}");
        if number == 2 {
            // The example in API.md.
            assert_eq!(
                reply,
                json!({ "error": {
                    "code": "malformed_line",
                    "message": "line 2: missing field `id` at line 1 column 36",
                    "line": 2,
                }})
            );
        }
    }

    // Nothing of the three batches was stored, not even their valid lines.
    assert_eq!(app.stored_rows("usage_events").await, 0);

    // A blank line is not a record and does not shift the numbering.
    let body = format!("{}\n\n\nnot json\n", serde_json::to_string(&events[0]).expect("json"));
    let response = app.post_batch(USAGE_PATH, "malformed-blank", body).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let reply: Value = response.json().await.expect("JSON error");
    assert_eq!(reply["error"]["line"], 4);
}

#[tokio::test]
async fn a_field_this_build_does_not_know_is_dropped_not_refused() {
    // A newer core may send a field an older analytics service has no column for.
    let app = TestApp::start().await;
    let mut line = serde_json::to_value(&fixture()[0]).expect("json");
    line["added_in_a_later_release"] = json!({ "anything": [1, 2, 3] });
    let response = app.post_batch(USAGE_PATH, "newer-core-1", format!("{line}\n")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(read_event(&app, id(1)).await, fixture()[0]);
}

#[tokio::test]
async fn the_batch_id_is_required() {
    let app = TestApp::start().await;
    let response =
        app.request(reqwest::Method::POST, USAGE_PATH).body(ndjson(&fixture())).send().await.expect("answer");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let reply: Value = response.json().await.expect("JSON error");
    assert_eq!(reply["error"]["code"], "invalid_batch_id");
    assert_eq!(app.stored_rows("usage_events").await, 0);
}

#[tokio::test]
async fn an_empty_batch_is_accepted_and_stores_nothing() {
    let app = TestApp::start().await;
    let response = app.post_batch(USAGE_PATH, "empty-1", String::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let reply: Value = response.json().await.expect("JSON");
    assert_eq!(reply, json!({ "batch_id": "empty-1", "rows": 0 }));
    assert_eq!(app.stored_rows("usage_events").await, 0);
}

#[tokio::test]
async fn a_clickhouse_failure_is_a_5xx_and_the_retry_succeeds() {
    let app = TestApp::start().await;

    // ClickHouse cannot take the insert.
    app.command("RENAME TABLE usage_events TO usage_events_away").await;
    let response = app.post_batch(USAGE_PATH, "retry-1", ndjson(&fixture())).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let reply: Value = response.json().await.expect("JSON error");
    assert_eq!(reply["error"]["code"], "storage_unavailable");

    // ClickHouse is back; the core sends the same batch again.
    app.command("RENAME TABLE usage_events_away TO usage_events").await;
    app.store_usage("retry-1", &fixture()).await;
    assert_eq!(app.stored_rows("usage_events").await, 9);
}

#[tokio::test]
async fn a_batch_above_the_size_limit_is_refused() {
    let app = TestApp::start().await;
    // The test configuration allows 8 MiB.
    let body = "x".repeat(9 * 1024 * 1024);
    let response = app.post_batch(USAGE_PATH, "large-1", body).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let reply: Value = response.json().await.expect("JSON error");
    assert_eq!(reply["error"]["code"], "batch_too_large");
}

#[tokio::test]
async fn a_compressed_batch_is_refused_rather_than_misread() {
    let app = TestApp::start().await;
    let response = app
        .request(reqwest::Method::POST, USAGE_PATH)
        .header(BATCH_ID_HEADER, "gzip-1")
        .header("content-encoding", "gzip")
        .body(vec![0x1f, 0x8b, 0x08, 0x00])
        .send()
        .await
        .expect("answer");
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}
