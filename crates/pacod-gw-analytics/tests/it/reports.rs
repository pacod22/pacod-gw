//! Every report on the fixed data set of `support::fixture`, with the numbers worked out by
//! hand. The table in the documentation of `fixture` is the reference.

use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::support::{MARCH, app_with_fixture, id, numbers, totals, with};

const ORG_A: &str = "organisation_id=org-a";
const FROM: &str = "2026-03-01T00:00:00Z";
const TO: &str = "2026-03-13T00:00:00Z";

/// One point of a usage series: the counters, the cost and the two delay percentiles.
fn point(bucket: &str, counts: [u64; 7], cost: f64, p50: f64, p99: f64) -> Value {
    with(
        with(json!({ "bucket": bucket }), totals(counts, cost)),
        json!({ "gateway_delay_p50_ms": p50, "gateway_delay_p99_ms": p99 }),
    )
}

/// One row of a breakdown.
fn group(name: &str, counts: [u64; 7], cost: f64) -> Value {
    with(json!({ "group": name }), totals(counts, cost))
}

// ---------------------------------------------------------------------------------------------
// overview

#[tokio::test]
async fn overview_covers_the_hour_day_week_and_month_that_contain_to() {
    let app = app_with_fixture().await;

    let report = app.json(&format!("/reports/overview?to=2026-03-12T10:30:00Z&{ORG_A}")).await;
    assert_eq!(
        report,
        json!({
            "to": "2026-03-12T10:30:00Z",
            "periods": {
                // 1, 2 and 9
                "hour": with(
                    json!({ "from": "2026-03-12T10:00:00Z" }),
                    totals([3, 2, 120, 50, 10, 5, 0], 0.5),
                ),
                // plus 3 and 6
                "day": with(
                    json!({ "from": "2026-03-12T00:00:00Z" }),
                    totals([5, 2, 720, 350, 10, 5, 40], 1.5),
                ),
                // plus 4; the week starts on Monday the 9th
                "week": with(
                    json!({ "from": "2026-03-09T00:00:00Z" }),
                    totals([6, 3, 1020, 350, 10, 5, 40], 1.75),
                ),
                // plus 5; event 8 is in February
                "month": with(
                    json!({ "from": "2026-03-01T00:00:00Z" }),
                    totals([7, 3, 2020, 850, 110, 55, 240], 3.75),
                ),
            },
        })
    );

    // Without a scope the other organisation's event 7 is part of the hour.
    let report = app.json("/reports/overview?to=2026-03-12T10:30:00Z").await;
    assert_eq!(
        report["periods"]["hour"],
        with(json!({ "from": "2026-03-12T10:00:00Z" }), totals([4, 2, 127, 53, 10, 5, 0], 4.5),)
    );

    // The other filters apply too.
    let report = app.json(&format!("/reports/overview?to=2026-03-12T10:30:00Z&{ORG_A}&provider=openai")).await;
    assert_eq!(
        report["periods"]["month"],
        with(json!({ "from": "2026-03-01T00:00:00Z" }), totals([3, 1, 900, 300, 0, 0, 40], 1.25),)
    );

    // By default the periods end now, long after the fixture.
    let report = app.json("/reports/overview").await;
    for period in ["hour", "day", "week", "month"] {
        assert_eq!(report["periods"][period]["requests"], 0, "{period}");
        assert_eq!(report["periods"][period]["cost"], 0.0, "{period}");
    }
}

// ---------------------------------------------------------------------------------------------
// usage

#[tokio::test]
async fn usage_is_a_time_series_per_group() {
    let app = app_with_fixture().await;

    let report = app.json(&format!("/reports/usage?{MARCH}&{ORG_A}&bucket=day&group_by=provider")).await;
    assert_eq!(
        report,
        json!({
            "from": FROM,
            "to": TO,
            "bucket": "day",
            "group_by": "provider",
            "series": [
                // 9, which was rejected before a provider was chosen
                { "group": "", "points": [
                    point("2026-03-12T00:00:00Z", [1, 1, 0, 0, 0, 0, 0], 0.0, 0.5, 0.5),
                ]},
                // 5, then 1 and 2 (delays 2 and 4: the median of two is the upper one)
                { "group": "anthropic", "points": [
                    point("2026-03-03T00:00:00Z", [1, 0, 1000, 500, 100, 50, 200], 2.0, 3.0, 3.0),
                    point("2026-03-12T00:00:00Z", [2, 1, 120, 50, 10, 5, 0], 0.5, 4.0, 4.0),
                ]},
                // 4, then 3 and 6 (delays 1 and 5)
                { "group": "openai", "points": [
                    point("2026-03-10T00:00:00Z", [1, 1, 300, 0, 0, 0, 0], 0.25, 8.0, 8.0),
                    point("2026-03-12T00:00:00Z", [2, 0, 600, 300, 0, 0, 40], 1.0, 5.0, 5.0),
                ]},
            ],
        })
    );
}

#[tokio::test]
async fn usage_buckets_by_hour_week_and_month() {
    let app = app_with_fixture().await;

    // Month, not grouped: February holds event 8, March the seven others of org-a.
    // March delays sorted: 0.5 1 2 3 4 5 8; median 3, 99th percentile 8.
    let report = app
        .json(&format!("/reports/usage?from=2026-02-01T00:00:00Z&to=2026-04-01T00:00:00Z&{ORG_A}&bucket=month"))
        .await;
    assert_eq!(report["group_by"], "none");
    assert_eq!(
        report["series"],
        json!([{ "group": null, "points": [
            point("2026-02-01T00:00:00Z", [1, 0, 10000, 5000, 0, 0, 0], 8.0, 6.0, 6.0),
            point("2026-03-01T00:00:00Z", [7, 3, 2020, 850, 110, 55, 240], 3.75, 3.0, 8.0),
        ]}])
    );

    // Week: Monday the 2nd holds 5, Monday the 9th the six others.
    // Their delays sorted: 0.5 1 2 4 5 8; median (position 3) 4, 99th percentile 8.
    let report = app.json(&format!("/reports/usage?{MARCH}&{ORG_A}&bucket=week&group_by=none")).await;
    assert_eq!(
        report["series"],
        json!([{ "group": null, "points": [
            point("2026-03-02T00:00:00Z", [1, 0, 1000, 500, 100, 50, 200], 2.0, 3.0, 3.0),
            point("2026-03-09T00:00:00Z", [6, 3, 1020, 350, 10, 5, 40], 1.75, 4.0, 8.0),
        ]}])
    );

    // Hour is the default bucket: 3 at eight, 6 at nine, and 1, 2 and 9 at ten.
    let report = app.json(&format!("/reports/usage?from=2026-03-12T00:00:00Z&to=2026-03-13T00:00:00Z&{ORG_A}")).await;
    assert_eq!(report["bucket"], "hour");
    assert_eq!(
        report["series"],
        json!([{ "group": null, "points": [
            point("2026-03-12T08:00:00Z", [1, 0, 200, 100, 0, 0, 40], 1.0, 1.0, 1.0),
            point("2026-03-12T09:00:00Z", [1, 0, 400, 200, 0, 0, 0], 0.0, 5.0, 5.0),
            point("2026-03-12T10:00:00Z", [3, 2, 120, 50, 10, 5, 0], 0.5, 2.0, 4.0),
        ]}])
    );
}

#[tokio::test]
async fn usage_groups_by_every_allowed_column_and_by_tag() {
    let app = app_with_fixture().await;

    // The group names of each grouping, in order.
    let expected: [(&str, &[&str]); 12] = [
        ("provider", &["", "anthropic", "openai"]),
        ("model", &["", "claude-haiku", "claude-sonnet", "gpt-mini"]),
        ("template", &["", "planner", "writer"]),
        ("key_id", &["key-1", "key-2", "key-3"]),
        ("end_user", &["", "alice", "bob", "carol"]),
        ("status", &["error", "ok", "rejected", "timeout"]),
        ("mode", &["managed", "proxy"]),
        ("agent_id", &["", "chat-bot", "planner-bot", "writer-bot"]),
        ("user_id", &["", "u-1", "u-2"]),
        ("task", &["", "chat", "plan", "write"]),
        ("complexity", &["", "high", "low", "medium"]),
        ("tag:purpose", &["", "chat", "plan", "write"]),
    ];
    for (group_by, groups) in expected {
        let report = app.json(&format!("/reports/usage?{MARCH}&{ORG_A}&bucket=month&group_by={group_by}")).await;
        assert_eq!(report["group_by"], group_by);
        let names: Vec<&str> = report["series"]
            .as_array()
            .expect("series")
            .iter()
            .map(|series| series["group"].as_str().expect("group"))
            .collect();
        assert_eq!(names, groups, "group_by={group_by}");

        // Whatever the grouping, the groups add up to the seven March events of org-a.
        let requests: u64 = report["series"]
            .as_array()
            .expect("series")
            .iter()
            .flat_map(|series| series["points"].as_array().expect("points"))
            .map(|point| point["requests"].as_u64().expect("requests"))
            .sum();
        assert_eq!(requests, 7, "group_by={group_by}");
    }

    // By tag, in full: no purpose on 4, 6 and 9 (delays 8, 5, 0.5).
    let report = app.json(&format!("/reports/usage?{MARCH}&{ORG_A}&bucket=month&group_by=tag:purpose")).await;
    let month = "2026-03-01T00:00:00Z";
    assert_eq!(
        report["series"],
        json!([
            { "group": "", "points": [point(month, [3, 2, 700, 200, 0, 0, 0], 0.25, 5.0, 8.0)] },
            { "group": "chat", "points": [point(month, [1, 0, 200, 100, 0, 0, 40], 1.0, 1.0, 1.0)] },
            { "group": "plan", "points": [point(month, [2, 1, 120, 50, 10, 5, 0], 0.5, 4.0, 4.0)] },
            { "group": "write", "points": [point(month, [1, 0, 1000, 500, 100, 50, 200], 2.0, 3.0, 3.0)] },
        ])
    );
}

#[tokio::test]
async fn usage_as_csv() {
    let app = app_with_fixture().await;
    let response = app.get(&format!("/reports/usage?{MARCH}&{ORG_A}&bucket=day&group_by=provider&format=csv")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/csv; charset=utf-8");
    assert_eq!(response.headers()["content-disposition"], "attachment; filename=\"usage.csv\"");
    assert_eq!(
        response.text().await.expect("body"),
        "bucket,group,requests,errors,input_tokens,output_tokens,cache_read_tokens,\
         cache_write_tokens,reasoning_tokens,cost,gateway_delay_p50_ms,gateway_delay_p99_ms\r\n\
         2026-03-12T00:00:00Z,,1,1,0,0,0,0,0,0.0,0.5,0.5\r\n\
         2026-03-03T00:00:00Z,anthropic,1,0,1000,500,100,50,200,2.0,3.0,3.0\r\n\
         2026-03-12T00:00:00Z,anthropic,2,1,120,50,10,5,0,0.5,4.0,4.0\r\n\
         2026-03-10T00:00:00Z,openai,1,1,300,0,0,0,0,0.25,8.0,8.0\r\n\
         2026-03-12T00:00:00Z,openai,2,0,600,300,0,0,40,1.0,5.0,5.0\r\n"
    );
}

// ---------------------------------------------------------------------------------------------
// breakdown

#[tokio::test]
async fn breakdown_lists_groups_by_cost() {
    let app = app_with_fixture().await;

    let report = app.json(&format!("/reports/breakdown?{MARCH}&{ORG_A}&by=model")).await;
    assert_eq!(
        report,
        json!({
            "from": FROM,
            "to": TO,
            "by": "model",
            "limit": 50,
            "rows": [
                group("claude-sonnet", [1, 0, 1000, 500, 100, 50, 200], 2.0),
                group("gpt-mini", [3, 1, 900, 300, 0, 0, 40], 1.25),
                group("claude-haiku", [2, 1, 120, 50, 10, 5, 0], 0.5),
                group("", [1, 1, 0, 0, 0, 0, 0], 0.0),
            ],
        })
    );

    // `limit` keeps the most expensive groups.
    let report = app.json(&format!("/reports/breakdown?{MARCH}&{ORG_A}&by=model&limit=2")).await;
    assert_eq!(report["limit"], 2);
    assert_eq!(
        report["rows"],
        json!([
            group("claude-sonnet", [1, 0, 1000, 500, 100, 50, 200], 2.0),
            group("gpt-mini", [3, 1, 900, 300, 0, 0, 40], 1.25),
        ])
    );

    // Equal cost: more requests first, then by name.
    let report = app.json(&format!("/reports/breakdown?{MARCH}&{ORG_A}&by=end_user")).await;
    assert_eq!(
        report["rows"],
        json!([
            group("alice", [3, 1, 1120, 550, 110, 55, 200], 2.5),
            group("bob", [2, 1, 500, 100, 0, 0, 40], 1.25),
            group("", [1, 1, 0, 0, 0, 0, 0], 0.0),
            group("carol", [1, 0, 400, 200, 0, 0, 0], 0.0),
        ])
    );

    let report = app.json(&format!("/reports/breakdown?{MARCH}&{ORG_A}&by=error_type")).await;
    assert_eq!(
        report["rows"],
        json!([
            group("", [4, 0, 1700, 850, 110, 55, 240], 3.5),
            group("upstream_timeout", [1, 1, 300, 0, 0, 0, 0], 0.25),
            group("quota_exceeded", [1, 1, 0, 0, 0, 0, 0], 0.0),
            group("rate_limited", [1, 1, 20, 0, 0, 0, 0], 0.0),
        ])
    );

    // The caller's attribution: 6 and 9 carry none.
    let report = app.json(&format!("/reports/breakdown?{MARCH}&{ORG_A}&by=agent_id")).await;
    assert_eq!(
        report["rows"],
        json!([
            group("writer-bot", [1, 0, 1000, 500, 100, 50, 200], 2.0),
            group("chat-bot", [2, 1, 500, 100, 0, 0, 40], 1.25),
            group("planner-bot", [2, 1, 120, 50, 10, 5, 0], 0.5),
            group("", [2, 1, 400, 200, 0, 0, 0], 0.0),
        ])
    );

    let report = app.json(&format!("/reports/breakdown?{MARCH}&{ORG_A}&by=complexity")).await;
    assert_eq!(
        report["rows"],
        json!([
            group("medium", [2, 1, 1300, 500, 100, 50, 200], 2.25),
            group("low", [1, 0, 200, 100, 0, 0, 40], 1.0),
            group("high", [2, 1, 120, 50, 10, 5, 0], 0.5),
            group("", [2, 1, 400, 200, 0, 0, 0], 0.0),
        ])
    );

    let report = app.json(&format!("/reports/breakdown?{MARCH}&{ORG_A}&by=tag:purpose")).await;
    assert_eq!(report["by"], "tag:purpose");
    assert_eq!(
        report["rows"],
        json!([
            group("write", [1, 0, 1000, 500, 100, 50, 200], 2.0),
            group("chat", [1, 0, 200, 100, 0, 0, 40], 1.0),
            group("plan", [2, 1, 120, 50, 10, 5, 0], 0.5),
            group("", [3, 2, 700, 200, 0, 0, 0], 0.25),
        ])
    );

    // The remaining groupings, across both organisations: event 7 adds 4.0 to anthropic.
    let report = app.json(&format!("/reports/breakdown?{MARCH}&by=provider")).await;
    assert_eq!(
        report["rows"],
        json!([
            group("anthropic", [4, 1, 1127, 553, 110, 55, 200], 6.5),
            group("openai", [3, 1, 900, 300, 0, 0, 40], 1.25),
            group("", [1, 1, 0, 0, 0, 0, 0], 0.0),
        ])
    );
    for (by, first) in [("template", ""), ("key_id", "key-9")] {
        let report = app.json(&format!("/reports/breakdown?{MARCH}&by={by}")).await;
        assert_eq!(report["rows"][0]["group"], first, "by={by}");
    }
}

#[tokio::test]
async fn breakdown_as_csv() {
    let app = app_with_fixture().await;
    let response = app.get(&format!("/reports/breakdown?{MARCH}&{ORG_A}&by=model&limit=2&format=csv")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-disposition"], "attachment; filename=\"breakdown.csv\"");
    assert_eq!(
        response.text().await.expect("body"),
        "group,requests,errors,input_tokens,output_tokens,cache_read_tokens,\
         cache_write_tokens,reasoning_tokens,cost\r\n\
         claude-sonnet,1,0,1000,500,100,50,200,2.0\r\n\
         gpt-mini,3,1,900,300,0,0,40,1.25\r\n"
    );
}

// ---------------------------------------------------------------------------------------------
// requests

#[tokio::test]
async fn requests_are_listed_newest_first_and_paged_by_cursor() {
    let app = app_with_fixture().await;

    let first = app.json(&format!("/reports/requests?{MARCH}&{ORG_A}&limit=3")).await;
    assert_eq!(first["from"], FROM);
    assert_eq!(first["to"], TO);
    assert_eq!(first["limit"], 3);
    assert_eq!(numbers(&first["requests"]), [9, 2, 1]);
    let cursor = first["next_cursor"].as_str().expect("a second page");

    let second = app.json(&format!("/reports/requests?{MARCH}&{ORG_A}&limit=3&cursor={cursor}")).await;
    assert_eq!(numbers(&second["requests"]), [6, 3, 4]);
    let cursor = second["next_cursor"].as_str().expect("a third page");

    let third = app.json(&format!("/reports/requests?{MARCH}&{ORG_A}&limit=3&cursor={cursor}")).await;
    assert_eq!(numbers(&third["requests"]), [5]);
    assert_eq!(third["next_cursor"], Value::Null);

    // A page that ends exactly at the last event has no next page.
    let all = app.json(&format!("/reports/requests?{MARCH}&{ORG_A}&limit=7")).await;
    assert_eq!(numbers(&all["requests"]), [9, 2, 1, 6, 3, 4, 5]);
    assert_eq!(all["next_cursor"], Value::Null);

    // An entry is the complete event.
    let event = &first["requests"][2];
    assert_eq!(event["id"], id(1).to_string());
    assert_eq!(event["timestamp"], "2026-03-12T10:05:00Z");
    assert_eq!(event["model_served"], "claude-haiku");
    assert_eq!(event["tags"], json!({ "purpose": "plan" }));
    assert_eq!(event["gateway_delay_ms"], 2.0);
    assert_eq!(event["cost"], 0.5);
    assert_eq!(event["audit_id"], "aud-1");
}

#[tokio::test]
async fn requests_are_found_by_trace_session_and_audit() {
    let app = app_with_fixture().await;
    let list = |query: &'static str| {
        let app = &app;
        async move { app.request_numbers(&format!("{MARCH}&{ORG_A}&{query}")).await }
    };
    assert_eq!(list("trace_id=trace-1").await, [2, 1]);
    assert_eq!(list("trace_id=trace-2").await, [3]);
    assert_eq!(list("trace_id=unknown").await, [0u128; 0]);
    assert_eq!(list("session_id=sess-1").await, [2, 1]);
    assert_eq!(list("audited=true").await, [1]);
    assert_eq!(list("audited=false").await, [9, 2, 6, 3, 4, 5]);
    assert_eq!(list("audited=true&trace_id=trace-2").await, [0u128; 0]);
}

#[tokio::test]
async fn requests_as_csv() {
    let app = app_with_fixture().await;
    let response = app.get(&format!("/reports/requests?{MARCH}&{ORG_A}&limit=2&format=csv")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-disposition"], "attachment; filename=\"requests.csv\"");
    let cursor = response.headers()["x-next-cursor"].to_str().expect("cursor").to_owned();
    let body = response.text().await.expect("body");
    let lines: Vec<&str> = body.split("\r\n").collect();
    // Header, events 9 and 2, and the empty piece after the last line break.
    assert_eq!(lines.len(), 4);
    assert!(lines[0].starts_with(
        "id,timestamp,instance_id,organisation_id,workspace_id,key_id,end_user,session_id,\
         trace_id,tags,agent_id,user_id,task,complexity,mode,interface,provider,model_requested,\
         model_served,model_reported,template,"
    ));
    assert!(lines[0].ends_with(
        ",price_missing,dropped_params,audit_id,intent,intent_confidence,intent_engine,plugin_verdict,plugin_error,parent_id,\
         cache,cache_saved_cost"
    ));
    assert!(
        lines[1]
            .starts_with("00000000-0000-0000-0000-000000000009,2026-03-12T10:25:00Z,i-1,org-a,ws-1,key-1,,,,{},,,,,")
    );
    assert!(lines[2].starts_with(
        "00000000-0000-0000-0000-000000000002,2026-03-12T10:20:00Z,i-1,org-a,ws-1,key-1,alice,\
         sess-1,trace-1,\"{\"\"purpose\"\":\"\"plan\"\"}\",planner-bot,u-1,plan,high,managed,"
    ));
    // One column per field of the event.
    let fields = serde_json::to_value(pacod_gw_events::UsageEvent::new(id(0), ""))
        .expect("json")
        .as_object()
        .expect("flat object")
        .len();
    assert_eq!(lines[0].split(',').count(), fields);

    // The cursor in the header continues the listing.
    let rest = app.request_numbers(&format!("{MARCH}&{ORG_A}&cursor={cursor}")).await;
    assert_eq!(rest, [1, 6, 3, 4, 5]);
}

#[tokio::test]
async fn one_request_is_found_by_id_whatever_its_age() {
    let app = app_with_fixture().await;

    // Event 8 is older than the default range of the lists; a lookup by id has no range.
    let event = app.json(&format!("/reports/requests/{}", id(8))).await;
    assert_eq!(event["timestamp"], "2026-02-20T12:00:00Z");
    assert_eq!(event["input_tokens"], 10000);

    // A range may be given, and then applies.
    let path = format!("/reports/requests/{}?{MARCH}", id(8));
    assert_eq!(app.error(&path, StatusCode::NOT_FOUND).await["code"], "not_found");

    let unknown = format!("/reports/requests/{}", id(404));
    assert_eq!(app.error(&unknown, StatusCode::NOT_FOUND).await["code"], "not_found");
    assert_eq!(app.error("/reports/requests/not-a-uuid", StatusCode::BAD_REQUEST).await["code"], "invalid_id");
}

// ---------------------------------------------------------------------------------------------
// traces

#[tokio::test]
async fn a_trace_lists_its_calls_in_time_order_with_totals() {
    let app = app_with_fixture().await;

    let trace = app.json("/reports/traces/trace-1").await;
    assert_eq!(trace["trace_id"], "trace-1");
    // 1 and 2: delays 2 and 4.
    assert_eq!(
        trace["totals"],
        with(
            totals([2, 1, 120, 50, 10, 5, 0], 0.5),
            json!({
                "gateway_delay_ms": 6.0,
                "first_request_at": "2026-03-12T10:05:00Z",
                "last_request_at": "2026-03-12T10:20:00Z",
            }),
        )
    );
    assert_eq!(numbers(&trace["requests"]), [1, 2]);

    assert_eq!(app.error("/reports/traces/unknown", StatusCode::NOT_FOUND).await["code"], "not_found");
}

// ---------------------------------------------------------------------------------------------
// end users

#[tokio::test]
async fn end_users_are_listed_with_their_consumption() {
    let app = app_with_fixture().await;

    let report = app.json(&format!("/reports/end-users?{MARCH}&{ORG_A}")).await;
    let end_user = |name: &str, counts: [u64; 7], cost: f64, first: &str, last: &str| {
        with(
            with(json!({ "end_user": name }), totals(counts, cost)),
            json!({ "first_request_at": first, "last_request_at": last }),
        )
    };
    let alice = end_user("alice", [3, 1, 1120, 550, 110, 55, 200], 2.5, "2026-03-03T12:00:00Z", "2026-03-12T10:20:00Z");
    let bob = end_user("bob", [2, 1, 500, 100, 0, 0, 40], 1.25, "2026-03-10T15:00:00Z", "2026-03-12T08:00:00Z");
    let carol = end_user("carol", [1, 0, 400, 200, 0, 0, 0], 0.0, "2026-03-12T09:00:00Z", "2026-03-12T09:00:00Z");
    // Event 9 has no end user and is not a row.
    assert_eq!(
        report,
        json!({
            "from": FROM,
            "to": TO,
            "limit": 100,
            "offset": 0,
            "rows": [alice, bob.clone(), carol],
        })
    );

    let report = app.json(&format!("/reports/end-users?{MARCH}&{ORG_A}&limit=1&offset=1")).await;
    assert_eq!(report["rows"], json!([bob]));
}

#[tokio::test]
async fn one_end_user_is_split_by_model_with_recent_calls() {
    let app = app_with_fixture().await;

    let report = app.json(&format!("/reports/end-users/alice?{MARCH}&{ORG_A}")).await;
    assert_eq!(report["from"], FROM);
    assert_eq!(report["to"], TO);
    assert_eq!(report["end_user"], "alice");
    assert_eq!(report["totals"], totals([3, 1, 1120, 550, 110, 55, 200], 2.5));
    assert_eq!(
        report["by_model"],
        json!([
            with(json!({ "model": "claude-sonnet" }), totals([1, 0, 1000, 500, 100, 50, 200], 2.0),),
            with(json!({ "model": "claude-haiku" }), totals([2, 1, 120, 50, 10, 5, 0], 0.5),),
        ])
    );
    assert_eq!(numbers(&report["recent_requests"]), [2, 1, 5]);

    let report = app.json(&format!("/reports/end-users/alice?{MARCH}&{ORG_A}&limit=2")).await;
    assert_eq!(numbers(&report["recent_requests"]), [2, 1]);

    // An end user without events in the range is all zeros, not an error.
    let report = app.json(&format!("/reports/end-users/nobody?{MARCH}")).await;
    assert_eq!(report["totals"], totals([0, 0, 0, 0, 0, 0, 0], 0.0));
    assert_eq!(report["by_model"], json!([]));
    assert_eq!(report["recent_requests"], json!([]));
}

// ---------------------------------------------------------------------------------------------
// errors

#[tokio::test]
async fn errors_are_reported_by_type_by_provider_and_model_and_over_time() {
    let app = app_with_fixture().await;

    let report = app.json(&format!("/reports/errors?{MARCH}&{ORG_A}&bucket=day")).await;
    assert_eq!(numbers(&report["recent_requests"]), [9, 2, 4]);

    let mut report = report;
    report.as_object_mut().expect("object").remove("recent_requests");
    assert_eq!(
        report,
        json!({
            "from": FROM,
            "to": TO,
            "bucket": "day",
            // The failures are 2 (error), 4 (timeout) and 9 (rejected).
            "totals": { "requests": 7, "errors": 3, "error_rate": 3.0 / 7.0 },
            "by_type": [
                { "error_type": "quota_exceeded", "errors": 1, "last_seen_at": "2026-03-12T10:25:00Z" },
                { "error_type": "rate_limited", "errors": 1, "last_seen_at": "2026-03-12T10:20:00Z" },
                { "error_type": "upstream_timeout", "errors": 1, "last_seen_at": "2026-03-10T15:00:00Z" },
            ],
            "by_provider_model": [
                { "provider": "", "model": "", "requests": 1, "errors": 1, "error_rate": 1.0 },
                { "provider": "anthropic", "model": "claude-haiku", "requests": 2, "errors": 1, "error_rate": 0.5 },
                { "provider": "openai", "model": "gpt-mini", "requests": 3, "errors": 1, "error_rate": 1.0 / 3.0 },
            ],
            "over_time": [
                { "bucket": "2026-03-03T00:00:00Z", "requests": 1, "errors": 0 },
                { "bucket": "2026-03-10T00:00:00Z", "requests": 1, "errors": 1 },
                { "bucket": "2026-03-12T00:00:00Z", "requests": 5, "errors": 2 },
            ],
        })
    );

    let report = app.json(&format!("/reports/errors?{MARCH}&{ORG_A}&limit=1")).await;
    assert_eq!(report["bucket"], "hour");
    assert_eq!(numbers(&report["recent_requests"]), [9]);

    // No events at all: zero, not a division by zero.
    let report = app.json(&format!("/reports/errors?{MARCH}&organisation_id=nobody")).await;
    assert_eq!(report["totals"], json!({ "requests": 0, "errors": 0, "error_rate": 0.0 }));
    assert_eq!(report["by_type"], json!([]));
}

// ---------------------------------------------------------------------------------------------
// delay

#[tokio::test]
async fn delay_reports_percentiles_stage_averages_and_the_slowest_calls() {
    let app = app_with_fixture().await;

    let report = app.json(&format!("/reports/delay?{MARCH}&{ORG_A}&bucket=day&group_by=mode&limit=3")).await;
    assert_eq!(numbers(&report["slowest_requests"]), [4, 6, 2]);

    let delays = |requests: u64, p50: f64, p95: f64, p99: f64, max: f64| {
        json!({
            "requests": requests,
            "gateway_delay_p50_ms": p50,
            "gateway_delay_p95_ms": p95,
            "gateway_delay_p99_ms": p99,
            "gateway_delay_max_ms": max,
        })
    };
    let at = |bucket: &str, figures: Value| with(json!({ "bucket": bucket }), figures);

    let mut report = report;
    report.as_object_mut().expect("object").remove("slowest_requests");
    // The seven delays sum to 23.5.
    let mean = 23.5 / 7.0;
    assert_eq!(
        report,
        json!({
            "from": FROM,
            "to": TO,
            "bucket": "day",
            "group_by": "mode",
            // Sorted: 0.5 1 2 3 4 5 8.
            "summary": delays(7, 3.0, 8.0, 8.0, 8.0),
            "stage_averages": {
                "gateway_delay_ms": mean,
                "receive_ms": 1.0,
                "pre_ms": mean,
                "connect_ms": 0.0,
                "retry_ms": 0.0,
                "upstream_first_byte_ms": 0.0,
                "upstream_stream_ms": 0.0,
                "post_ms": 0.0,
                "relay_max_ms": 0.0,
                "settle_ms": 0.0,
                "total_ms": 250.0,
            },
            "series": [
                // 5, then 9, 1 and 2 (0.5 2 4)
                { "group": "managed", "points": [
                    at("2026-03-03T00:00:00Z", delays(1, 3.0, 3.0, 3.0, 3.0)),
                    at("2026-03-12T00:00:00Z", delays(3, 2.0, 4.0, 4.0, 4.0)),
                ]},
                // 4, then 3 and 6 (1 5)
                { "group": "proxy", "points": [
                    at("2026-03-10T00:00:00Z", delays(1, 8.0, 8.0, 8.0, 8.0)),
                    at("2026-03-12T00:00:00Z", delays(2, 5.0, 5.0, 5.0, 5.0)),
                ]},
            ],
        })
    );
}

#[tokio::test]
async fn delay_groups_by_every_allowed_column() {
    let app = app_with_fixture().await;
    let expected: [(&str, &[&str]); 10] = [
        ("mode", &["managed", "proxy"]),
        ("provider", &["", "anthropic", "openai"]),
        ("model", &["", "claude-haiku", "claude-sonnet", "gpt-mini"]),
        ("template", &["", "planner", "writer"]),
        ("workspace_id", &["ws-1", "ws-2"]),
        ("instance_id", &["i-1", "i-2"]),
        ("agent_id", &["", "chat-bot", "planner-bot", "writer-bot"]),
        ("user_id", &["", "u-1", "u-2"]),
        ("task", &["", "chat", "plan", "write"]),
        ("complexity", &["", "high", "low", "medium"]),
    ];
    for (group_by, groups) in expected {
        let report = app.json(&format!("/reports/delay?{MARCH}&{ORG_A}&bucket=month&group_by={group_by}")).await;
        let names: Vec<&str> = report["series"]
            .as_array()
            .expect("series")
            .iter()
            .map(|series| series["group"].as_str().expect("group"))
            .collect();
        assert_eq!(names, groups, "group_by={group_by}");
    }

    // Not grouped, by default; by default the twenty slowest calls.
    let report = app.json(&format!("/reports/delay?{MARCH}&{ORG_A}")).await;
    assert_eq!(report["group_by"], "none");
    assert_eq!(report["bucket"], "hour");
    assert_eq!(report["series"][0]["group"], Value::Null);
    assert_eq!(numbers(&report["slowest_requests"]), [4, 6, 2, 5, 1, 3, 9]);

    // Any stage's percentiles over time: in the fixture the stage before the provider is the
    // whole gateway delay, and every call takes 250 ms in total.
    let report = app.json(&format!("/reports/delay?{MARCH}&{ORG_A}&bucket=month&stage=pre")).await;
    let point = &report["series"][0]["points"][0];
    assert_eq!(report["stage"], "pre");
    assert_eq!(
        (point["stage_p50_ms"].clone(), point["stage_p99_ms"].clone()),
        (point["gateway_delay_p50_ms"].clone(), point["gateway_delay_p99_ms"].clone())
    );
    let report = app.json(&format!("/reports/delay?{MARCH}&{ORG_A}&bucket=month&stage=total")).await;
    assert_eq!(report["series"][0]["points"][0]["stage_p99_ms"], 250.0);
    assert_eq!(
        app.error(&format!("/reports/delay?{MARCH}&stage=gateway"), StatusCode::BAD_REQUEST).await["code"],
        "invalid_parameter"
    );

    // Tags are not a grouping of this report.
    let path = format!("/reports/delay?{MARCH}&group_by=tag:purpose");
    assert_eq!(app.error(&path, StatusCode::BAD_REQUEST).await["code"], "invalid_parameter");

    // Without events there is nothing to measure.
    let report = app.json(&format!("/reports/delay?{MARCH}&organisation_id=nobody")).await;
    assert_eq!(
        report["summary"],
        json!({
            "requests": 0,
            "gateway_delay_p50_ms": null,
            "gateway_delay_p95_ms": null,
            "gateway_delay_p99_ms": null,
            "gateway_delay_max_ms": null,
        })
    );
    assert_eq!(report["stage_averages"]["pre_ms"], Value::Null);
    assert_eq!(report["series"], json!([]));
    assert_eq!(report["slowest_requests"], json!([]));
}

// ---------------------------------------------------------------------------------------------
// accuracy

#[tokio::test]
async fn accuracy_reports_the_cost_that_does_not_rest_on_provider_usage() {
    let app = app_with_fixture().await;

    let report = app.json(&format!("/reports/accuracy?{MARCH}&{ORG_A}")).await;
    assert_eq!(
        report,
        json!({
            "from": FROM,
            "to": TO,
            "requests": 7,
            "total_cost": 3.75,
            // 2 and 9 (usage missing, no cost) and 4 (estimated, 0.25)
            "non_provider_requests": 3,
            "non_provider_cost": 0.25,
            "non_provider_share": 0.25 / 3.75,
            // 6
            "price_missing_requests": 1,
        })
    );

    // No cost at all: a share of zero.
    let report = app.json(&format!("/reports/accuracy?{MARCH}&{ORG_A}&end_user=carol")).await;
    assert_eq!(report["total_cost"], 0.0);
    assert_eq!(report["non_provider_share"], 0.0);
    assert_eq!(report["price_missing_requests"], 1);
}

// ---------------------------------------------------------------------------------------------
// scope and filters

#[tokio::test]
async fn scope_filters_restrict_every_report() {
    let app = app_with_fixture().await;

    // The request list shows exactly which events a scope lets through.
    let list = |scope: &'static str| {
        let app = &app;
        async move { app.request_numbers(&format!("{MARCH}&{scope}")).await }
    };
    assert_eq!(list("").await, [9, 2, 7, 1, 6, 3, 4, 5]);
    assert_eq!(list("organisation_id=org-a").await, [9, 2, 1, 6, 3, 4, 5]);
    assert_eq!(list("organisation_id=org-b").await, [7]);
    assert_eq!(list("organisation_id=org-a&workspace_id=ws-1").await, [9, 2, 1, 3, 4, 5]);
    assert_eq!(list("organisation_id=org-a&workspace_id=ws-2").await, [6]);
    // A workspace of another organisation: nothing.
    assert_eq!(list("organisation_id=org-b&workspace_id=ws-1").await, [0u128; 0]);
    assert_eq!(list("organisation_id=unknown").await, [0u128; 0]);
    // A scope given several times matches any of its values.
    assert_eq!(list("workspace_id=ws-2&workspace_id=ws-9").await, [7, 6]);

    // Lookups by id are scoped too: event 7 and trace-9 belong to org-b.
    let request = format!("/reports/requests/{}", id(7));
    app.json(&request).await;
    app.json(&format!("{request}?organisation_id=org-b&workspace_id=ws-9")).await;
    app.error(&format!("{request}?organisation_id=org-a"), StatusCode::NOT_FOUND).await;
    app.error(&format!("{request}?organisation_id=org-b&workspace_id=ws-1"), StatusCode::NOT_FOUND).await;
    app.json("/reports/traces/trace-9?organisation_id=org-b").await;
    app.error("/reports/traces/trace-9?organisation_id=org-a", StatusCode::NOT_FOUND).await;

    // Every aggregated report, scoped to org-b, sees event 7 and nothing else.
    let org_b = "organisation_id=org-b";
    let seven = totals([1, 0, 7, 3, 0, 0, 0], 4.0);

    let report = app.json(&format!("/reports/overview?to=2026-03-12T10:30:00Z&{org_b}")).await;
    assert_eq!(report["periods"]["month"], with(json!({ "from": "2026-03-01T00:00:00Z" }), seven.clone()));

    let report = app.json(&format!("/reports/usage?{MARCH}&{org_b}&bucket=month")).await;
    assert_eq!(
        report["series"],
        json!([{ "group": null, "points": [
            point("2026-03-01T00:00:00Z", [1, 0, 7, 3, 0, 0, 0], 4.0, 100.0, 100.0),
        ]}])
    );

    let report = app.json(&format!("/reports/breakdown?{MARCH}&{org_b}&by=provider")).await;
    assert_eq!(report["rows"], json!([with(json!({ "group": "anthropic" }), seven.clone())]));

    let report = app.json(&format!("/reports/end-users?{MARCH}&{org_b}")).await;
    assert_eq!(report["rows"].as_array().expect("rows").len(), 1);
    assert_eq!(report["rows"][0]["end_user"], "zed");

    // alice exists, but not in org-b.
    let report = app.json(&format!("/reports/end-users/alice?{MARCH}&{org_b}")).await;
    assert_eq!(report["totals"]["requests"], 0);
    assert_eq!(report["recent_requests"], json!([]));

    let report = app.json(&format!("/reports/errors?{MARCH}&{org_b}")).await;
    assert_eq!(report["totals"], json!({ "requests": 1, "errors": 0, "error_rate": 0.0 }));
    assert_eq!(report["recent_requests"], json!([]));

    let report = app.json(&format!("/reports/delay?{MARCH}&{org_b}")).await;
    assert_eq!(report["summary"]["requests"], 1);
    assert_eq!(report["summary"]["gateway_delay_max_ms"], 100.0);
    assert_eq!(numbers(&report["slowest_requests"]), [7]);

    let report = app.json(&format!("/reports/accuracy?{MARCH}&{org_b}")).await;
    assert_eq!(report["requests"], 1);
    assert_eq!(report["total_cost"], 4.0);
}

#[tokio::test]
async fn each_filter_restricts() {
    let app = app_with_fixture().await;
    let cases: [(&str, &[u128]); 22] = [
        ("provider=openai", &[6, 3, 4]),
        ("provider=", &[9]),
        ("model=claude-haiku", &[2, 1]),
        ("model=gpt-mini&model=claude-sonnet", &[6, 3, 4, 5]),
        ("template=planner", &[2, 1]),
        ("key_id=key-2", &[3, 4]),
        ("end_user=bob", &[3, 4]),
        ("status=ok", &[1, 6, 3, 5]),
        ("status=error&status=timeout", &[2, 4]),
        ("mode=proxy", &[6, 3, 4]),
        ("tag.purpose=plan", &[2, 1]),
        ("tag.purpose=plan&tag.purpose=chat", &[2, 1, 3]),
        ("tag.purpose=", &[9, 6, 4]),
        ("tag.unknown=x", &[]),
        ("provider=anthropic&status=ok", &[1, 5]),
        ("provider=anthropic&tag.purpose=write&mode=managed", &[5]),
        ("agent_id=chat-bot", &[3, 4]),
        ("user_id=u-1", &[2, 1, 5]),
        ("task=plan&task=write", &[2, 1, 5]),
        ("complexity=medium", &[4, 5]),
        ("complexity=", &[9, 6]),
        ("agent_id=planner-bot&complexity=high", &[2, 1]),
    ];
    for (filter, expected) in cases {
        let got = app.request_numbers(&format!("{MARCH}&{ORG_A}&{filter}")).await;
        assert_eq!(got, expected, "{filter}");

        // The aggregated reports apply the same filter.
        let report = app.json(&format!("/reports/accuracy?{MARCH}&{ORG_A}&{filter}")).await;
        assert_eq!(report["requests"], expected.len(), "{filter}");
    }

    // The time range is half-open: `from` is inside, `to` is outside.
    let exact = app.request_numbers("from=2026-03-12T08:00:00Z&to=2026-03-12T09:00:00Z").await;
    assert_eq!(exact, [3]);

    // Without a range a list covers the last 24 hours, which hold none of the fixture.
    assert_eq!(app.request_numbers("").await, [0u128; 0]);
}

#[tokio::test]
async fn invalid_parameters_are_refused_with_a_reason() {
    let app = app_with_fixture().await;
    let cases = [
        // A misspelt scope must not silently widen the report.
        ("/reports/usage?organization_id=org-a", "unknown_parameter"),
        ("/reports/overview?from=2026-03-01T00:00:00Z", "unknown_parameter"),
        ("/reports/accuracy?bucket=day", "unknown_parameter"),
        ("/reports/usage?tag.=x", "invalid_parameter"),
        ("/reports/usage?bucket=minute", "invalid_parameter"),
        ("/reports/usage?bucket=hour&bucket=day", "invalid_parameter"),
        ("/reports/usage?group_by=id", "invalid_parameter"),
        ("/reports/usage?from=yesterday", "invalid_parameter"),
        ("/reports/usage?from=2026-03-02T00:00:00Z&to=2026-03-01T00:00:00Z", "invalid_parameter"),
        ("/reports/usage?format=xml", "invalid_parameter"),
        ("/reports/breakdown", "invalid_parameter"),
        ("/reports/breakdown?by=none", "invalid_parameter"),
        ("/reports/breakdown?by=status", "invalid_parameter"),
        ("/reports/breakdown?by=model&limit=0", "invalid_parameter"),
        ("/reports/breakdown?by=model&limit=1001", "invalid_parameter"),
        ("/reports/requests?audited=maybe", "invalid_parameter"),
        ("/reports/requests?cursor=nonsense", "invalid_parameter"),
        ("/reports/requests?limit=ten", "invalid_parameter"),
        ("/reports/end-users?offset=-1", "invalid_parameter"),
        ("/reports/requests/00000000-0000-0000-0000-000000000001?provider=openai", "unknown_parameter"),
        ("/reports/traces/trace-1?status=ok", "unknown_parameter"),
        // A path segment that is not UTF-8 once decoded.
        ("/reports/end-users/%FF", "invalid_path"),
        ("/reports/traces/%FF", "invalid_path"),
        ("/reports/audit/%FF", "invalid_path"),
        ("/reports/requests/%FF", "invalid_path"),
    ];
    for (path, code) in cases {
        let error = app.error(path, StatusCode::BAD_REQUEST).await;
        assert_eq!(error["code"], code, "{path}: {error}");
        assert!(!error["message"].as_str().expect("message").is_empty(), "{path}");
    }
}

// ---------------------------------------------------------------------------------------------
// currencies

#[tokio::test]
async fn costs_are_added_up_in_the_reporting_currency() {
    let app = crate::support::TestApp::start().await;
    let event = |n: u128, cost: f64, currency: &str, converted: Option<f64>| {
        let mut e = pacod_gw_events::UsageEvent::new(id(n), "gw-1");
        e.timestamp = crate::support::at("2026-03-12T10:05:00Z");
        e.organisation_id = "org-x".into();
        e.workspace_id = "ws-x".into();
        e.status = "ok".into();
        e.usage_source = "provider".into();
        e.cost = Some(cost);
        e.currency = currency.into();
        e.cost_reporting = converted;
        e.reporting_currency = if converted.is_some() { "USD".into() } else { String::new() };
        e
    };
    // A price in euros converted at 1.1, one in dollars, and an event from before conversion.
    app.store_usage(
        "currencies",
        &[event(1, 1.0, "EUR", Some(1.1)), event(2, 2.0, "USD", Some(2.0)), event(3, 0.5, "USD", None)],
    )
    .await;
    let r = app.json("/reports/overview?to=2026-03-12T10:30:00Z&organisation_id=org-x").await;
    let cost = r["periods"]["hour"]["cost"].as_f64().unwrap();
    assert!((cost - 3.6).abs() < 1e-9, "{r}");
    let r = app.json("/reports/breakdown?from=2026-03-12T00:00:00Z&to=2026-03-13T00:00:00Z&organisation_id=org-x&by=workspace_id").await;
    let cost = r["rows"][0]["cost"].as_f64().unwrap_or_default();
    assert!((cost - 3.6).abs() < 1e-9, "{r}");
}

#[tokio::test]
async fn every_report_has_a_csv_form() {
    let app = app_with_fixture().await;
    let csv = |path: String, file: &'static str| {
        let app = &app;
        async move {
            let response = app.get(&path).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(response.headers()["content-type"], "text/csv; charset=utf-8");
            assert_eq!(response.headers()["content-disposition"], format!("attachment; filename=\"{file}\""));
            response
                .text()
                .await
                .expect("body")
                .split("\r\n")
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        }
    };

    let lines = csv(format!("/reports/overview?to=2026-03-12T10:30:00Z&{ORG_A}&format=csv"), "overview.csv").await;
    assert_eq!(
        lines[0],
        "period,from,to,requests,errors,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,reasoning_tokens,cost"
    );
    assert_eq!(lines.len(), 5);
    assert!(lines[4].starts_with("month,2026-03-01T00:00:00Z,2026-03-12T10:30:00Z,7,3,"), "{lines:?}");

    let lines = csv(format!("/reports/traces/trace-1?{ORG_A}&format=csv"), "trace.csv").await;
    assert_eq!(lines.len(), 3, "a header and the two calls of the trace");
    assert!(lines[0].starts_with("id,timestamp,"));

    let lines = csv(format!("/reports/end-users?{MARCH}&{ORG_A}&format=csv"), "end-users.csv").await;
    assert!(
        lines[0].starts_with("end_user,requests,errors,") && lines[0].ends_with(",first_request_at,last_request_at")
    );
    assert!(lines.len() > 1);

    let lines = csv(format!("/reports/errors?{MARCH}&{ORG_A}&format=csv"), "errors.csv").await;
    assert_eq!(lines[0], "provider,model,requests,errors,error_rate");
    assert!(lines.len() > 1);

    let lines = csv(format!("/reports/delay?{MARCH}&{ORG_A}&bucket=month&stage=pre&format=csv"), "delay.csv").await;
    assert_eq!(
        lines[0],
        "bucket,group,requests,gateway_delay_p50_ms,gateway_delay_p95_ms,gateway_delay_p99_ms,gateway_delay_max_ms,stage_p50_ms,stage_p95_ms,stage_p99_ms"
    );
    assert_eq!(lines.len(), 2);

    let lines = csv(format!("/reports/accuracy?{MARCH}&{ORG_A}&format=csv"), "accuracy.csv").await;
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("from,to,requests,total_cost,"));
}

#[tokio::test]
async fn reports_filter_and_group_by_what_plugins_found() {
    let app = crate::support::TestApp::start().await;
    let mut events = crate::support::fixture();
    for (n, e) in events.iter_mut().enumerate() {
        match n % 3 {
            0 => {
                e.intent = "billing".into();
                e.intent_engine = "jev".into();
                e.plugin_verdict = "allow".into();
            }
            1 => {
                e.intent = "legal_advice".into();
                e.intent_engine = "cascade:llm".into();
                e.plugin_verdict = "would_block".into();
            }
            _ => {}
        }
    }
    let with_intent = events.iter().filter(|e| e.organisation_id == "org-a" && e.intent == "billing").count() as u64;
    app.store_usage("plugins-1", &events).await;

    let report = app.json(&format!("/reports/breakdown?{MARCH}&{ORG_A}&by=intent")).await;
    let groups: Vec<&str> = report["rows"].as_array().unwrap().iter().filter_map(|r| r["group"].as_str()).collect();
    assert!(groups.contains(&"billing") && groups.contains(&"legal_advice") && groups.contains(&""), "{groups:?}");
    let billing = report["rows"].as_array().unwrap().iter().find(|r| r["group"] == "billing").unwrap();
    assert_eq!(billing["requests"], with_intent);

    let report = app.json(&format!("/reports/breakdown?{MARCH}&{ORG_A}&by=plugin_verdict&intent=legal_advice")).await;
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{report}");
    assert_eq!(rows[0]["group"], "would_block");

    let report = app.json(&format!("/reports/usage?{MARCH}&{ORG_A}&group_by=intent_engine&bucket=month")).await;
    assert!(report.to_string().contains("cascade:llm"), "{report}");
}
