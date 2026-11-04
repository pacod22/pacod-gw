use std::collections::HashMap;

use jiff::Timestamp;
use pacod_broker_model::{Label, Reputation};
use serde_json::json;

use super::*;
use crate::fixtures::{entry, provider};

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

/// Three providers: a GPU farm with an inventory and off-peak prices, a cheap provider, and an
/// MCP server private to one organisation.
fn catalogue() -> Index {
    let farm = provider(
        "farm",
        json!({"name": "x", "summary": "GPU farm in Sofia", "inventory": {"visibility": "platform",
            "hardware": [{"id": "h100", "type": "gpu", "vendor": "nvidia", "model": "H100-SXM", "count": 8, "memory_gb": 80},
                         {"id": "ram", "type": "memory", "memory_gb": 2048}],
            "software": [{"id": "vllm", "kind": "serving_engine", "name": "vllm", "version": "0.7.3"},
                         {"id": "cuda", "kind": "driver", "name": "cuda", "version": "12.6"}]}}),
    );
    let cheap = provider("cheap", json!({"name": "x", "summary": "Cheap"}));
    let tools = provider("tools", json!({"name": "x", "summary": "Tools"}));
    let entries = vec![
        entry(
            &farm,
            json!({
                "key": "llama", "kind": "model", "name": "llama-3.3-70b", "summary": "Llama 3.3 70B on H100s in Sofia",
                "tags": ["llm"], "interfaces": [{"protocol": "openai-chat", "url": "https://farm.example/v1"}],
                "model": {"family": "llama", "context_window": 131072, "modalities": {"input": ["text"], "output": ["text"]}},
                "capabilities": {"tools": true, "streaming": true},
                "runs_on": {"hardware": ["h100"], "software": ["vllm", "cuda"]},
                "pricing": {"currency": "EUR", "timezone": "UTC", "periods": [{"valid_from": "2026-01-01T00:00:00Z", "rates": {"input": 0.8, "output": 1.2},
                    "windows": [{"name": "night", "from_time": "22:00", "to_time": "06:00", "rates": {"input": 0.3}}]}]},
                "availability": {"regions": ["bg"], "latency": {"ttft_ms": 300}, "capacity": {"max_concurrency": 16},
                    "schedule": {"timezone": "UTC", "maintenance": [{"from": "2026-11-04T00:00:00Z", "to": "2026-11-04T02:00:00Z"}]}},
                "policy": {"residency": ["eu"], "training_use": false},
                "access": {"visibility": "public"}
            }),
        ),
        entry(
            &cheap,
            json!({
                "key": "llama", "kind": "model", "name": "llama-3.3-70b", "summary": "Llama 3.3 70B, cheap, in the US",
                "tags": ["llm"], "interfaces": [{"protocol": "openai-chat", "url": "https://cheap.example/v1"}],
                "model": {"family": "llama", "context_window": 8192},
                "capabilities": {"streaming": true},
                "pricing": {"currency": "EUR", "periods": [{"valid_from": "2026-01-01T00:00:00Z", "rates": {"input": 0.5, "output": 0.7}}]},
                "availability": {"regions": ["us-east"], "latency": {"ttft_ms": 900}},
                "access": {"visibility": "platform", "min_client_reputation": 0.6, "allow_new_clients": false}
            }),
        ),
        entry(
            &tools,
            json!({
                "key": "weather", "kind": "mcp_server", "name": "weather", "summary": "Weather forecasts as MCP tools",
                "interfaces": [{"protocol": "mcp-streamable-http", "url": "https://tools.example/mcp"}],
                "capabilities": {"mcp_tools": [{"name": "forecast", "description": "Forecast for a city"}]},
                "pricing": {"free": true},
                "access": {"visibility": "organisation:org1"}
            }),
        ),
    ];
    Index::build(7, [farm, cheap, tools], entries)
}

struct Fixed(HashMap<String, Reputation>);

impl Scores for Fixed {
    fn resource(&self, id: &str) -> Option<&Reputation> {
        self.0.get(id)
    }
    fn provider(&self, _: &str) -> Option<&Reputation> {
        None
    }
}

fn run(idx: &Index, q: serde_json::Value, viewer: &Viewer) -> Result<Results, QueryError> {
    let q: Query = serde_json::from_value(q).unwrap();
    idx.discover(&q, &Ctx { now: ts("2026-11-03T12:00:00Z"), viewer, scores: &NoScores })
}

fn names(r: &Results) -> Vec<String> {
    r.hits.iter().map(|h| format!("{}/{}", h.entry.provider.profile.name, h.entry.record.spec.name)).collect()
}

#[test]
fn visibility_follows_the_caller() {
    let idx = catalogue();
    let anon = Viewer::default();
    assert_eq!(run(&idx, json!({}), &anon).unwrap().total, 0);
    let anon_public = Viewer { public_discovery: true, ..Viewer::default() };
    assert_eq!(names(&run(&idx, json!({}), &anon_public).unwrap()), ["farm/llama-3.3-70b"]);
    let user = Viewer { authenticated: true, reputation: Some((0.9, 0.9)), ..Viewer::default() };
    assert_eq!(run(&idx, json!({}), &user).unwrap().total, 2);
    let member = Viewer {
        authenticated: true,
        organisation: Some("org1".into()),
        reputation: Some((0.9, 0.9)),
        ..Viewer::default()
    };
    assert_eq!(run(&idx, json!({}), &member).unwrap().total, 3);
}

#[test]
fn providers_can_keep_resources_from_low_reputation_clients() {
    let idx = catalogue();
    let low = Viewer { authenticated: true, reputation: Some((0.4, 0.9)), ..Viewer::default() };
    assert_eq!(names(&run(&idx, json!({"kind": "model"}), &low).unwrap()), ["farm/llama-3.3-70b"]);
    // New clients are kept out too here: `allow_new_clients: false`.
    let new = Viewer { authenticated: true, reputation: Some((0.9, 0.1)), ..Viewer::default() };
    assert_eq!(run(&idx, json!({"kind": "model"}), &new).unwrap().total, 1);
    // Seeing every visibility (open mode) is not a pass through the threshold; reviewing is.
    let open = Viewer { all: true, authenticated: true, reputation: Some((0.4, 0.9)), ..Viewer::default() };
    assert_eq!(names(&run(&idx, json!({"kind": "model"}), &open).unwrap()), ["farm/llama-3.3-70b"]);
    assert_eq!(run(&idx, json!({"kind": "model"}), &Viewer::everything()).unwrap().total, 2);
}

#[test]
fn prices_are_evaluated_at_the_instant_asked() {
    let idx = catalogue();
    let all = Viewer::everything();
    // At noon the farm costs 0.8 for input, the cheap one 0.5.
    let r = run(&idx, json!({"kind": "model", "sort": "price:input"}), &all).unwrap();
    assert_eq!(names(&r), ["cheap/llama-3.3-70b", "farm/llama-3.3-70b"]);
    assert_eq!(r.hits[0].why.sort_value, Some(0.5));
    // At night the farm's window makes it 0.3.
    let r = run(&idx, json!({"kind": "model", "sort": "price:input", "at": "2026-11-03T23:00:00Z"}), &all).unwrap();
    assert_eq!(names(&r), ["farm/llama-3.3-70b", "cheap/llama-3.3-70b"]);
    assert_eq!(r.hits[0].price.as_ref().unwrap().source, "window:night");
    // A maximum price.
    let r = run(&idx, json!({"kind": "model", "max_price": {"input": 0.6}}), &all).unwrap();
    assert_eq!(names(&r), ["cheap/llama-3.3-70b"]);
}

#[test]
fn spans_use_the_time_weighted_mean_and_full_coverage() {
    let idx = catalogue();
    let all = Viewer::everything();
    // One day at the farm: 8 h at 0.3, 16 h at 0.8, mean 0.633…, but a maintenance window on
    // 4 November breaks full coverage.
    let r = run(&idx, json!({"kind": "model", "provider": idx.entries().find(|e| e.provider.profile.name == "farm").unwrap().record.provider_id,
        "during": {"from": "2026-11-03T00:00:00Z", "to": "2026-11-04T00:00:00Z"}}), &all).unwrap();
    let avg = r.hits[0].price_summary.as_ref().unwrap().rates["input"].avg;
    assert!((avg - (8.0 * 0.3 + 16.0 * 0.8) / 24.0).abs() < 1e-9, "{avg}");
    let r = run(
        &idx,
        json!({"kind": "model", "during": {"from": "2026-11-03T20:00:00Z", "to": "2026-11-04T04:00:00Z"}}),
        &all,
    )
    .unwrap();
    assert_eq!(names(&r), ["cheap/llama-3.3-70b"]);
    let r = run(&idx, json!({"kind": "model", "during": {"from": "2026-11-03T20:00:00Z", "to": "2026-11-04T04:00:00Z"}, "availability": {"coverage": "any"}}), &all).unwrap();
    assert_eq!(r.total, 2);
}

#[test]
fn hardware_and_software_filters_use_the_inventory() {
    let idx = catalogue();
    let all = Viewer::everything();
    let r = run(&idx, json!({"hardware": {"gpu_model": "h100-sxm", "min_gpu_memory_gb": 80, "min_gpus": 4}}), &all)
        .unwrap();
    assert_eq!(names(&r), ["farm/llama-3.3-70b"]);
    assert_eq!(run(&idx, json!({"hardware": {"min_gpus": 16}}), &all).unwrap().total, 0);
    let r = run(
        &idx,
        json!({"software": [{"name": "vllm", "version": ">=0.7"}, {"name": "cuda", "version": "^12"}]}),
        &all,
    )
    .unwrap();
    assert_eq!(r.total, 1);
    assert_eq!(run(&idx, json!({"software": [{"name": "vllm", "version": ">=0.8"}]}), &all).unwrap().total, 0);
    // An anonymous caller cannot match on an inventory only signed-in callers may see.
    let anon = Viewer { public_discovery: true, ..Viewer::default() };
    assert_eq!(run(&idx, json!({"hardware": {"gpu_model": "H100-SXM"}}), &anon).unwrap().total, 0);
}

#[test]
fn text_capability_protocol_and_policy_filters() {
    let idx = catalogue();
    let all = Viewer::everything();
    assert_eq!(names(&run(&idx, json!({"q": "forecast"}), &all).unwrap()), ["tools/weather"]);
    assert_eq!(names(&run(&idx, json!({"q": "sofia llam"}), &all).unwrap()), ["farm/llama-3.3-70b"]);
    assert_eq!(run(&idx, json!({"capabilities": ["tools", "streaming"]}), &all).unwrap().total, 1);
    assert_eq!(run(&idx, json!({"capabilities": ["mcp_tool:forecast"]}), &all).unwrap().total, 1);
    assert_eq!(run(&idx, json!({"protocols": ["mcp-streamable-http", "openai-chat"]}), &all).unwrap().total, 3);
    assert_eq!(run(&idx, json!({"residency": ["eu", "us"], "no_training_use": true}), &all).unwrap().total, 1);
    assert_eq!(run(&idx, json!({"model": {"min_context_window": 100000}}), &all).unwrap().total, 1);
    assert_eq!(run(&idx, json!({"regions": ["us-east"]}), &all).unwrap().total, 1);
}

#[test]
fn ranking_by_reputation_and_latency_stays_within_a_kind() {
    let idx = catalogue();
    let all = Viewer::everything();
    assert!(matches!(run(&idx, json!({"sort": "reputation"}), &all), Err(QueryError::Invalid(_))));
    let r = run(&idx, json!({"kind": "model", "sort": "ttft"}), &all).unwrap();
    assert_eq!(names(&r), ["farm/llama-3.3-70b", "cheap/llama-3.3-70b"]);

    let cheap_id = idx.entries().find(|e| e.provider.profile.name == "cheap").unwrap().record.id.clone();
    let rep = |score| Reputation {
        score,
        confidence: 0.9,
        label: Label::of(score, 0.9),
        components: Default::default(),
        computed_at: ts("2026-11-03T12:00:00Z"),
        frozen: false,
    };
    let scores = Fixed([(cheap_id, rep(0.9))].into());
    let q: Query = serde_json::from_value(
        json!({"kind": "model", "sort": "reputation", "min_reputation": 0.0, "min_confidence": 0.0}),
    )
    .unwrap();
    let r = idx.discover(&q, &Ctx { now: ts("2026-11-03T12:00:00Z"), viewer: &all, scores: &scores }).unwrap();
    assert_eq!(names(&r), ["cheap/llama-3.3-70b", "farm/llama-3.3-70b"]);
    // The default confidence threshold leaves out the farm, which has no evidence yet.
    let q: Query = serde_json::from_value(json!({"kind": "model", "min_reputation": 0.5})).unwrap();
    let r = idx.discover(&q, &Ctx { now: ts("2026-11-03T12:00:00Z"), viewer: &all, scores: &scores }).unwrap();
    assert_eq!(names(&r), ["cheap/llama-3.3-70b"]);
}

#[test]
fn expired_leases_vanish_and_paging_works() {
    let idx = catalogue();
    let all = Viewer::everything();
    let r = run(&idx, json!({"limit": 2}), &all).unwrap();
    assert_eq!((r.hits.len(), r.total, r.next_cursor.as_deref()), (2, 3, Some("2")));
    let r2 = run(&idx, json!({"limit": 2, "cursor": "2"}), &all).unwrap();
    assert_eq!((r2.hits.len(), r2.next_cursor), (1, None));
    idx.entries().next().unwrap().live.expire();
    assert_eq!(run(&idx, json!({}), &all).unwrap().total, 2);
}

#[test]
fn bad_queries_are_refused_with_a_reason() {
    let idx = catalogue();
    let all = Viewer::everything();
    for q in [
        json!({"max_price": {"tokens": 1}}),
        json!({"at": "2026-11-03T00:00:00Z", "during": {"from": "2026-11-03T00:00:00Z", "to": "2026-11-04T00:00:00Z"}}),
        json!({"during": {"from": "2026-11-03T00:00:00Z", "to": "2027-11-04T00:00:00Z"}}),
        json!({"software": [{"name": "x", "version": "not a range"}]}),
        json!({"cursor": "abc"}),
    ] {
        assert!(run(&idx, q.clone(), &all).is_err(), "{q}");
    }
    assert!(serde_json::from_value::<Query>(json!({"sort": "price:tokens"})).is_err());
}

#[test]
fn the_currency_filter_keeps_comparisons_honest() {
    let idx = catalogue();
    let all = Viewer::everything();
    // Every priced resource of the fixture is in EUR; the MCP server is free.
    assert_eq!(run(&idx, json!({"currency": "EUR"}), &all).unwrap().total, 3);
    assert_eq!(names(&run(&idx, json!({"currency": "USD"}), &all).unwrap()), ["tools/weather"]);
}
