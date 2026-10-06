//! API.md is the contract the core's pass-through and the UI are built from. This test keeps
//! its examples true: every request line in the file is sent to a service that holds the data
//! set the file describes, and the answer must be the block that follows the line.
//!
//! A request line is a line that consists of nothing but `` `GET <path>` `` or
//! `` `DELETE <path>` ``. Its example is the fenced block that starts two lines below.

use reqwest::Method;
use serde_json::Value;

use crate::support::{TestApp, audit_record, fixture};

const API_MD: &str = include_str!("../../API.md");

#[derive(Debug)]
struct Example {
    method: Method,
    path: String,
    /// `json` or `csv`.
    language: String,
    body: String,
}

fn examples() -> Vec<Example> {
    let lines: Vec<&str> = API_MD.lines().collect();
    let mut examples = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some(request) = line.strip_prefix('`').and_then(|rest| rest.strip_suffix('`')) else {
            continue;
        };
        let (method, path) = match request.split_once(' ') {
            Some(("GET", path)) => (Method::GET, path),
            Some(("DELETE", path)) => (Method::DELETE, path),
            _ => continue,
        };
        assert!(!path.contains(' ') && !path.contains('`'), "line {}: not a plain request line: {line}", index + 1);
        assert_eq!(
            lines.get(index + 1).copied(),
            Some(""),
            "line {}: a request line is followed by a blank line and its example",
            index + 1
        );
        let language = lines
            .get(index + 2)
            .and_then(|fence| fence.strip_prefix("```"))
            .unwrap_or_else(|| panic!("line {}: the request has no example block", index + 1));
        let body: Vec<&str> = lines[index + 3..].iter().take_while(|line| **line != "```").copied().collect();
        examples.push(Example { method, path: path.to_owned(), language: language.to_owned(), body: body.join("\n") });
    }
    examples
}

/// Equal, except that an object with the key `…` stands for a complete usage event and is
/// compared by its `id` only.
fn assert_matches(expected: &Value, actual: &Value, at: &str) {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) if expected.contains_key("…") => {
            assert_eq!(expected["id"], actual["id"], "{at}.id");
            assert!(actual.contains_key("gateway_delay_ms"), "{at}: not an event");
        }
        (Value::Object(expected), Value::Object(actual)) => {
            let expected_keys: Vec<&String> = expected.keys().collect();
            let actual_keys: Vec<&String> = actual.keys().collect();
            assert_eq!(expected_keys, actual_keys, "{at}: fields and their order");
            for (key, value) in expected {
                assert_matches(value, &actual[key], &format!("{at}.{key}"));
            }
        }
        (Value::Array(expected), Value::Array(actual)) => {
            assert_eq!(expected.len(), actual.len(), "{at}: length");
            for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
                assert_matches(expected, actual, &format!("{at}[{index}]"));
            }
        }
        _ => assert_eq!(expected, actual, "{at}"),
    }
}

#[tokio::test]
async fn every_example_in_api_md_is_what_the_service_answers() {
    let app = TestApp::start().await;
    app.store_usage("doc-usage-1", &fixture()).await;
    app.store_audit(
        "doc-audit-1",
        &[audit_record("aud-1", 1, "2026-03-12T10:05:00Z"), audit_record("aud-2", 2, "2026-03-12T10:20:00Z")],
    )
    .await;

    let examples = examples();
    // Every report and both audit reads have an example, plus the CSV forms and one error.
    for route in [
        "/reports/overview?",
        "/reports/usage?",
        "/reports/breakdown?",
        "/reports/requests?",
        "/reports/traces/",
        "/reports/end-users?",
        "/reports/end-users/",
        "/reports/errors?",
        "/reports/delay?",
        "/reports/accuracy?",
        "/reports/ranking?",
        "/reports/audit?",
        "/reports/audit/",
        "/audit?",
    ] {
        assert!(examples.iter().any(|example| example.path.starts_with(route)), "API.md has no example for {route}");
    }
    assert_eq!(examples.iter().filter(|example| example.language == "csv").count(), 2);

    // In document order: the erasure is the last example that touches the audit records.
    for example in &examples {
        let at = format!("{} {}", example.method, example.path);
        let response = app.request(example.method.clone(), &example.path).send().await.expect("answer");
        let text = response.text().await.expect("body");
        match example.language.as_str() {
            "json" => {
                let expected: Value = serde_json::from_str(&example.body)
                    .unwrap_or_else(|error| panic!("{at}: the example is not JSON: {error}"));
                let actual: Value = serde_json::from_str(&text)
                    .unwrap_or_else(|error| panic!("{at}: the answer is not JSON: {error}: {text}"));
                assert_matches(&expected, &actual, &at);
            }
            "csv" => assert_eq!(text.replace("\r\n", "\n"), format!("{}\n", example.body), "{at}"),
            other => panic!("{at}: unexpected example language `{other}`"),
        }
    }
}
