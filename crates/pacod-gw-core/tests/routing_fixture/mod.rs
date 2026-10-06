//! A snapshot for measuring routing: one workspace, eight models at two providers, and
//! templates routed in different ways. Shared by the allocation test and the benchmark.

use chrono::Utc;
use pacod_gw_core::crypto::envelope::Kek;
use pacod_gw_core::snapshot::{Snapshot, build};
use pacod_gw_core::store::Row;
use serde_json::{Value, json};

fn row(kind: &str, id: &str, parent: Option<&str>, doc: Value) -> Row {
    Row {
        id: id.into(),
        kind: kind.into(),
        parent: parent.map(Into::into),
        name: None,
        doc,
        version: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn template(id: &str, name: &str, spec: Value) -> Row {
    row(
        "template",
        id,
        Some("ws_1"),
        json!({ "name": name, "current_version": 1, "versions": [
            { "version": 1, "spec": spec, "created_at": "2026-10-01T00:00:00Z" } ] }),
    )
}

/// The key's HMAC in the snapshot.
pub const KEY: [u8; 32] = [1u8; 32];

/// Rules that never match: they make the scan walk past `n` rules.
fn misses(n: usize) -> Vec<Value> {
    (0..n)
        .map(|i| {
            json!({ "name": format!("miss-{i}"), "when": {
            "task": { "in": ["never-a", "never-b"] }, "agent_id": { "prefix": ["nobody-"] } },
            "then": { "tiers": ["small"] } })
        })
        .collect()
}

pub fn snapshot() -> Snapshot {
    let mut rows = vec![
        row("organisation", "org_1", None, json!({ "name": "acme" })),
        row("workspace", "ws_1", Some("org_1"), json!({ "name": "prod", "tag_keys": ["team"] })),
        row("api_key", "key_1", Some("ws_1"), json!({ "name": "k", "secret_hmac": hex::encode(KEY) })),
        row("provider", "prv_a", None, json!({ "name": "a", "family": "openai", "base_url": "http://a.invalid/v1" })),
        row("provider", "prv_b", None, json!({ "name": "b", "family": "anthropic", "base_url": "http://b.invalid" })),
    ];
    let mut pool = Vec::new();
    for i in 0..8 {
        let (prv, tier) = if i % 2 == 0 { ("prv_a", "small") } else { ("prv_b", "large") };
        let id = format!("mdl_{i}");
        let name = format!("x/m{i}");
        rows.push(row(
            "model",
            &id,
            None,
            json!({ "name": name, "provider_id": prv, "upstream_id": format!("m{i}"), "state": "active", "free": true }),
        ));
        rows.push(row("org_grant", &format!("ogr_{i}"), Some("org_1"), json!({ "model_id": id })));
        pool.push(json!({ "model": name, "tier": tier, "weight": i + 1 }));
    }
    let mut ten = misses(9);
    ten.push(json!({ "name": "hit", "when": { "complexity": ["high"], "task": { "in": ["code-review"] },
        "tags": { "team": { "in": ["search"] } } }, "then": { "tiers": ["large", "small"], "algorithm": "weighted" } }));
    let mut sixty_four = misses(63);
    sixty_four.push(ten[9].clone());
    let cel = json!([{ "name": "cel", "when": { "task": { "present": true } },
        "cel": "input_tokens > 1000 && tags[\"team\"] == \"search\"", "then": { "tiers": ["large"] } }]);
    for (id, name, rules) in
        [("rsc_10", "ten", json!(ten)), ("rsc_64", "sixty-four", json!(sixty_four)), ("rsc_cel", "cel", cel)]
    {
        rows.push(row("routing_scheme", id, Some("org_1"), json!({ "name": name, "rules": rules })));
    }
    rows.push(template("tpl_1", "single", json!({ "model": "x/m0" })));
    rows.push(template("tpl_2", "ten", json!({ "models": pool, "routing_scheme": "ten" })));
    rows.push(template("tpl_3", "sixty-four", json!({ "models": pool, "routing_scheme": "sixty-four" })));
    rows.push(template("tpl_4", "wrr", json!({ "models": pool })));
    rows.push(template("tpl_5", "cel", json!({ "models": pool, "routing_scheme": "cel" })));
    let snap = build(1, rows, &[Kek::new("k", [0u8; 32])]);
    assert!(snap.problems.is_empty(), "{:?}", snap.problems);
    snap
}
