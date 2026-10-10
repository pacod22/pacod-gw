//! The token check, and SQL injection through every door a request has: filter values,
//! tag keys, group-by, bucket, cursor, limit and path segments.

use pacod_gw_events::UsageEvent;
use pacod_gw_events::ingest::{INTERNAL_TOKEN_HEADER, USAGE_PATH};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::support::{MARCH, TOKEN, TestApp, app_with_fixture, at, fixture, ndjson};

// ---------------------------------------------------------------------------------------------
// token

#[tokio::test]
async fn every_route_but_the_probes_needs_the_token() {
    let app = app_with_fixture().await;
    let event_path = format!("/reports/requests/{}", Uuid::from_u128(1));
    let routes = [
        (Method::POST, "/ingest/usage"),
        (Method::POST, "/ingest/audit"),
        (Method::GET, "/reports/overview"),
        (Method::GET, "/reports/usage"),
        (Method::GET, "/reports/breakdown?by=model"),
        (Method::GET, "/reports/requests"),
        (Method::GET, event_path.as_str()),
        (Method::GET, "/reports/traces/trace-1"),
        (Method::GET, "/reports/end-users"),
        (Method::GET, "/reports/end-users/alice"),
        (Method::GET, "/reports/errors"),
        (Method::GET, "/reports/delay"),
        (Method::GET, "/reports/accuracy"),
        (Method::GET, "/reports/ranking"),
        (Method::GET, "/reports/audit"),
        (Method::GET, "/reports/audit/aud-1"),
        (Method::DELETE, "/audit?end_user=alice"),
        // A path that does not exist gives nothing away either.
        (Method::GET, "/reports/nothing-here"),
        (Method::GET, "/"),
    ];

    let wrong_tokens: [Option<&str>; 6] = [
        None,
        Some(""),
        Some("wrong-token-wrong-token-wrong-to"),
        // One character short, one too long, one changed at the end.
        Some(&TOKEN[..TOKEN.len() - 1]),
        Some("integration-test-token-01234567890"),
        Some("integration-test-token-012345678X"),
    ];

    for (method, path) in &routes {
        for token in wrong_tokens {
            let mut request = app.http.request(method.clone(), format!("{}{path}", app.base));
            if let Some(token) = token {
                request = request.header(INTERNAL_TOKEN_HEADER, token);
            }
            let response = request.send().await.expect("answer");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{method} {path} with token {token:?}");
            let body: Value = response.json().await.expect("JSON error");
            assert_eq!(body["error"]["code"], "unauthorized");
        }
    }

    // The same requests pass the check with the token (whatever they answer then).
    for (method, path) in &routes {
        let response = app.request(method.clone(), path).send().await.expect("answer");
        assert_ne!(response.status(), StatusCode::UNAUTHORIZED, "{method} {path}");
    }

    // The token is not accepted anywhere but in its header.
    let response =
        app.http.get(format!("{}/reports/overview", app.base)).bearer_auth(TOKEN).send().await.expect("answer");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Without the token nothing was erased or stored by the attempts above.
    assert_eq!(app.stored_rows("usage_events").await, 9);
}

#[tokio::test]
async fn the_probes_need_no_token() {
    let app = TestApp::start().await;
    for path in ["/healthz", "/readyz"] {
        let response = app.http.get(format!("{}{path}", app.base)).send().await.expect("answer");
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
}

#[tokio::test]
async fn an_unknown_route_is_a_404_for_the_core() {
    let app = TestApp::start().await;
    let error = app.error("/reports/nothing-here", StatusCode::NOT_FOUND).await;
    assert_eq!(error["code"], "not_found");
}

// ---------------------------------------------------------------------------------------------
// injection

/// Encodes a value for use in a query string.
fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Encodes a value for use as one path segment, where a `+` is not a space.
fn encode_segment(value: &str) -> String {
    encode(value).replace('+', "%20")
}

const HOSTILE: [&str; 8] = [
    "' OR 1=1 --",
    "x' OR '1'='1",
    "'; DROP TABLE usage_events; --",
    "alice' UNION ALL SELECT * FROM usage_events --",
    "\\' OR 1=1 --",
    "alice\tOR\t1=1",
    "alice\n; DROP TABLE usage_events",
    "{p0:String}",
];

/// The fixture is intact: nothing was dropped, deleted or added.
async fn assert_intact(app: &TestApp) {
    assert_eq!(app.stored_rows("usage_events").await, 9);
    assert_eq!(app.request_numbers(MARCH).await, [9, 2, 7, 1, 6, 3, 4, 5]);
}

#[tokio::test]
async fn a_hostile_filter_value_is_only_ever_a_value() {
    let app = app_with_fixture().await;

    for hostile in HOSTILE {
        let value = encode(hostile);
        for filter in [
            "organisation_id",
            "workspace_id",
            "provider",
            "model",
            "template",
            "key_id",
            "end_user",
            "status",
            "mode",
            "tag.purpose",
            "trace_id",
            "session_id",
        ] {
            // The value matches no event: the list is empty, not everything and not an error.
            let got = app.request_numbers(&format!("{MARCH}&{filter}={value}")).await;
            assert!(got.is_empty(), "{filter}={hostile:?} matched {got:?}");
        }

        // The same through the aggregated reports.
        let report = app.json(&format!("/reports/accuracy?{MARCH}&end_user={value}")).await;
        assert_eq!(report["requests"], 0, "{hostile:?}");
        let report = app.json(&format!("/reports/breakdown?{MARCH}&by=model&provider={value}")).await;
        assert_eq!(report["rows"], json!([]), "{hostile:?}");

        // A hostile tag key is a key no event has.
        let got = app.request_numbers(&format!("{MARCH}&tag.{value}=plan")).await;
        assert!(got.is_empty(), "tag key {hostile:?} matched {got:?}");

        // As a path segment it names an end user, a trace or an audit record that is not there.
        let segment = encode_segment(hostile);
        let report = app.json(&format!("/reports/end-users/{segment}?{MARCH}")).await;
        assert_eq!(report["end_user"], hostile);
        assert_eq!(report["totals"]["requests"], 0);
        app.error(&format!("/reports/traces/{segment}"), StatusCode::NOT_FOUND).await;
        app.error(&format!("/reports/audit/{segment}"), StatusCode::NOT_FOUND).await;
        app.error(&format!("/reports/requests/{segment}"), StatusCode::BAD_REQUEST).await;
    }

    assert_intact(&app).await;
}

#[tokio::test]
async fn a_value_full_of_sql_characters_is_matched_literally() {
    let app = TestApp::start().await;

    // An end user and a tag whose names are everything a careless query builder trips over.
    let odd_user = "o'brien \\ \"x\" ; -- /* */ \t tab \n line {p0:String} %";
    let odd_key = "k'ey\\ \"q\"";
    let odd_value = "v'al\\ue\n2";
    let mut events = Vec::new();
    for (number, end_user) in [(1u128, odd_user), (2, "plain"), (3, "o'brien")] {
        let mut event = UsageEvent::new(Uuid::from_u128(number), "i-1");
        event.timestamp = at("2026-03-12T10:00:00Z") + chrono::Duration::seconds(number as i64);
        event.organisation_id = "org-a".into();
        event.workspace_id = "ws-1".into();
        event.key_id = "key-1".into();
        event.end_user = end_user.into();
        event.status = "ok".into();
        if number == 1 {
            event.tags.insert(odd_key.into(), odd_value.into());
            event.trace_id = odd_user.into();
        }
        events.push(event);
    }
    app.store_usage("odd-1", &events).await;

    // Exactly the one event, by filter value, by tag key and value, and by path segment.
    let by_user = app.request_numbers(&format!("{MARCH}&end_user={}", encode(odd_user))).await;
    assert_eq!(by_user, [1]);
    let by_tag = app.request_numbers(&format!("{MARCH}&tag.{}={}", encode(odd_key), encode(odd_value))).await;
    assert_eq!(by_tag, [1]);
    let by_trace = app.request_numbers(&format!("{MARCH}&trace_id={}", encode(odd_user))).await;
    assert_eq!(by_trace, [1]);

    let report = app.json(&format!("/reports/end-users/{}?{MARCH}", encode_segment(odd_user))).await;
    assert_eq!(report["end_user"], odd_user);
    assert_eq!(report["totals"]["requests"], 1);

    // Grouping by the odd tag key gives its value as a group, verbatim.
    let report = app.json(&format!("/reports/breakdown?{MARCH}&by={}", encode(&format!("tag:{odd_key}")))).await;
    let groups: Vec<&str> =
        report["rows"].as_array().expect("rows").iter().map(|row| row["group"].as_str().expect("group")).collect();
    assert_eq!(groups, ["", odd_value]);

    // And it comes back verbatim in the end-user list and, quoted, in CSV.
    let report = app.json(&format!("/reports/end-users?{MARCH}")).await;
    let mut names: Vec<&str> = report["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["end_user"].as_str().expect("end_user"))
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["o'brien", odd_user, "plain"]);

    let csv = app
        .get(&format!("/reports/requests?{MARCH}&format=csv&end_user={}", encode(odd_user)))
        .await
        .text()
        .await
        .expect("body");
    let quoted = format!(",\"{}\",", odd_user.replace('"', "\"\""));
    assert!(csv.contains(&quoted), "{csv}");
}

#[tokio::test]
async fn group_by_bucket_and_the_other_identifiers_come_from_a_fixed_list() {
    let app = app_with_fixture().await;

    let hostile_identifiers = [
        "provider; DROP TABLE usage_events",
        "provider, cost",
        "toString(organisation_id)",
        "1) FROM usage_events; DROP TABLE usage_events; --",
        "(SELECT max(cost) FROM usage_events)",
        "model_served",
        // A column, but not a grouping: the grouping names are a fixed list.
        "cost",
        "`group`",
        "",
    ];
    for hostile in hostile_identifiers {
        let value = encode(hostile);
        for path in [
            format!("/reports/usage?{MARCH}&group_by={value}"),
            format!("/reports/delay?{MARCH}&group_by={value}"),
            format!("/reports/breakdown?{MARCH}&by={value}"),
            format!("/reports/usage?{MARCH}&bucket={value}"),
            format!("/reports/errors?{MARCH}&bucket={value}"),
            format!("/reports/delay?{MARCH}&bucket={value}"),
            format!("/reports/usage?{MARCH}&format={value}"),
        ] {
            let error = app.error(&path, StatusCode::BAD_REQUEST).await;
            assert_eq!(error["code"], "invalid_parameter", "{path}");
        }
    }

    // `tag:<key>` takes any key, and the key is a bound value: a hostile one is simply a tag
    // nobody has set, so every event falls into the group without a value.
    for hostile in ["x'] OR 1=1 --", "purpose'], cost) --", "a']; DROP TABLE usage_events; --"] {
        let value = encode(&format!("tag:{hostile}"));
        let report = app.json(&format!("/reports/usage?{MARCH}&bucket=month&group_by={value}")).await;
        assert_eq!(report["group_by"], format!("tag:{hostile}"));
        let series = report["series"].as_array().expect("series");
        assert_eq!(series.len(), 1, "{hostile}");
        assert_eq!(series[0]["group"], "");
        assert_eq!(series[0]["points"][0]["requests"], 8);

        let report = app.json(&format!("/reports/breakdown?{MARCH}&by={value}")).await;
        assert_eq!(report["rows"].as_array().expect("rows").len(), 1);
        assert_eq!(report["rows"][0]["requests"], 8);
    }

    // Numbers and cursors are parsed before they are used.
    for path in [
        format!("/reports/requests?{MARCH}&limit={}", encode("1; DROP TABLE usage_events")),
        format!("/reports/requests?{MARCH}&cursor={}", encode("1:x' OR 1=1 --")),
        format!("/reports/requests?{MARCH}&cursor={}", encode("1 OR 1=1:00000000-0000-0000-0000-000000000001")),
        format!("/reports/end-users?{MARCH}&offset={}", encode("0; DROP TABLE usage_events")),
        format!("/reports/requests?{MARCH}&audited={}", encode("true OR 1=1")),
        format!("/reports/usage?from={}", encode("2026-03-01T00:00:00Z' OR '1'='1")),
    ] {
        let error = app.error(&path, StatusCode::BAD_REQUEST).await;
        assert_eq!(error["code"], "invalid_parameter", "{path}");
    }
    // A cursor into the audit list is a time and an id; a hostile id is compared as a string.
    let page = app.json(&format!("/reports/audit?{MARCH}&cursor={}", encode("1773311400000000000:x' OR 1=1 --"))).await;
    assert_eq!(page["records"], json!([]));

    assert_intact(&app).await;
}

#[tokio::test]
async fn hostile_values_cannot_widen_an_erasure() {
    let app = TestApp::start().await;
    let records = [
        crate::support::audit_record("aud-1", 1, "2026-03-12T10:05:00Z"),
        crate::support::audit_record("aud-2", 2, "2026-03-12T10:06:00Z"),
    ];
    app.store_audit("audit-1", &records).await;

    for hostile in HOSTILE {
        for parameter in ["end_user", "session_id"] {
            let response = app
                .request(Method::DELETE, &format!("/audit?{parameter}={}", encode(hostile)))
                .send()
                .await
                .expect("answer");
            assert_eq!(response.status(), StatusCode::OK, "{parameter}={hostile:?}");
            let reply: Value = response.json().await.expect("JSON");
            assert_eq!(reply, json!({ "deleted": 0 }), "{parameter}={hostile:?}");
        }
    }
    assert_eq!(app.stored_rows("message_audit").await, 2);
}

#[tokio::test]
async fn a_hostile_batch_id_is_refused_or_harmless() {
    let app = TestApp::start().await;
    let body = ndjson(&fixture());

    // Control characters cannot be sent in a header at all; spaces are refused by the service.
    let response = app.post_batch(USAGE_PATH, "x' OR 1=1 --", body.clone()).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(app.stored_rows("usage_events").await, 0);

    // Without spaces it is accepted, as a token and nothing else.
    let hostile = "x';DROP/**/TABLE/**/usage_events;--&database=system&query=SELECT%201";
    let response = app.post_batch(USAGE_PATH, hostile, body.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let reply: Value = response.json().await.expect("JSON");
    assert_eq!(reply, json!({ "batch_id": hostile, "rows": 9 }));
    assert_eq!(app.stored_rows("usage_events").await, 9);

    // And it deduplicates like any other id.
    let response = app.post_batch(USAGE_PATH, hostile, body).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(app.stored_rows("usage_events").await, 9);
}
