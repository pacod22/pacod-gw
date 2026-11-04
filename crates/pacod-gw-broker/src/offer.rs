//! What an organisation offers, as a broker announcement (docs/pacod-gw.md in pacod-broker, R1
//! and section 5). Pure: the gateway's views in, an announcement out.

use std::collections::BTreeMap;

use pacod_broker_client::Announcement;
use pacod_broker_client::model::LiveStatus;
use serde_json::{Map, Value, json};

use crate::config::Offering;
use crate::gw::{Model, Named, Price, RoutingTarget, Template};

/// One offered template with what the announcement needs about it.
#[derive(Debug, Clone)]
pub struct Offered {
    pub offering: Offering,
    pub workspace: Named,
    pub template: Template,
    /// The pool's first model, whose kind, limits and prices the offering shows.
    pub model: Option<Model>,
    /// That model's price versions, newest first.
    pub prices: Vec<Price>,
}

/// Where announcements point, and the lease.
#[derive(Debug, Clone)]
pub struct Site {
    pub public_url: String,
    pub obtain_url: String,
    pub ttl_s: u64,
}

/// The broker's provider name of an organisation's offerings.
pub fn provider_name(org: &Named) -> String {
    format!("gw-{}", org.name)
}

/// The resource key of an offering: the workspace and the template, both slugs in the gateway.
pub fn key(o: &Offered) -> String {
    format!("{}.{}", o.workspace.name, o.template.name)
}

pub fn announcement(org: &Named, offered: &[Offered], site: &Site) -> anyhow::Result<Announcement> {
    let display = if org.display_name.is_empty() { &org.name } else { &org.display_name };
    let resources: Vec<Value> = offered.iter().map(|o| resource(org, o, site)).collect();
    let a = json!({
        "ttl_s": site.ttl_s,
        "provider": {
            "name": provider_name(org),
            "summary": format!("{display}: models and templates through pacod-gw"),
            "website": site.public_url,
        },
        "resources": resources,
    });
    let a: Announcement = serde_json::from_value(a)?;
    // The broker's default limit on descriptions; a broker set lower refuses with its reason.
    if let Err(p) = a.validate(16 * 1024) {
        anyhow::bail!("{}: the announcement does not validate: {p}", org.name);
    }
    Ok(a)
}

fn kind(model_kind: &str) -> String {
    match model_kind {
        "" | "chat" => "model".into(),
        "embedding" => "embedding".into(),
        other => format!("other:{other}"),
    }
}

fn resource(org: &Named, o: &Offered, site: &Site) -> Value {
    let t = &o.template;
    let model_kind = o.model.as_ref().map_or("chat", |m| m.kind.as_str());
    let auth = json!({ "scheme": "bearer", "obtain_url": site.obtain_url });
    let mut interfaces =
        vec![json!({ "protocol": "pacod-gw", "url": format!("{}/gw/v1", site.public_url), "auth": auth })];
    if matches!(model_kind, "" | "chat") {
        interfaces.push(json!({ "protocol": "openai-chat", "url": format!("{}/v1", site.public_url), "auth": auth }));
        interfaces
            .push(json!({ "protocol": "anthropic-messages", "url": format!("{}/v1", site.public_url), "auth": auth }));
        interfaces.push(json!({ "protocol": "gemini", "url": format!("{}/v1beta", site.public_url), "auth": auth }));
    }
    let summary = o.offering.summary.clone().unwrap_or_else(|| {
        if t.description.is_empty() {
            format!("Template {} of {} on pacod-gw: call model @{}", t.name, org.name, t.name)
        } else {
            t.description.clone()
        }
    });
    let mut model = Map::new();
    // Callers name the template, not an upstream model: the gateway picks within its pool.
    model.insert("upstream_id".into(), json!(format!("@{}", t.name)));
    if let Some(m) = &o.model {
        if let Some(c) = m.context_window {
            model.insert("context_window".into(), json!(c));
        }
        if let Some(c) = m.max_output_tokens {
            model.insert("max_output_tokens".into(), json!(c));
        }
    }
    let visibility = match o.offering.visibility.as_str() {
        "organisation" => format!("organisation:{}", org.id),
        v => v.to_string(),
    };
    let mut r = json!({
        "key": key(o),
        "kind": kind(model_kind),
        "name": o.offering.name.clone().unwrap_or_else(|| t.name.clone()),
        "summary": summary,
        "description": format!(
            "Offered by organisation {} through its gateway, workspace {}, template {} (version {}). \
             Call it as model `@{}` with a key of that workspace.",
            org.name, o.workspace.name, t.name, t.current_version, t.name
        ),
        "tags": ["gateway", "template"],
        "interfaces": interfaces,
        "model": model,
        "access": { "visibility": visibility },
    });
    if let Some(p) = pricing(&o.prices) {
        r["pricing"] = p;
    }
    r
}

/// The broker's names for gw's rates. gw rates the broker has no field for (the one-hour cache
/// write, audio and image tokens, characters, search units) are left out.
const RATES: [(&str, &str); 9] = [
    ("input", "input"),
    ("output", "output"),
    ("cache_read", "cache_read"),
    ("cache_write_5m", "cache_write"),
    ("reasoning", "reasoning"),
    ("per_request", "per_request"),
    ("per_server_tool_call", "per_call"),
    ("per_image", "per_image"),
    ("per_audio_second", "per_audio_second"),
];

fn rates(from: &Map<String, Value>) -> Option<Value> {
    let mut out = Map::new();
    for (gw, broker) in RATES {
        if let Some(v) = from.get(gw).and_then(Value::as_f64) {
            out.insert(broker.into(), json!(v));
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// A gw price version as a broker period. Every version that was in force is a period of its
/// own: the broker refuses an announcement that changes or drops a period that has started, so
/// the history must read the same every time it is announced.
fn period(p: &Price) -> Option<Value> {
    let base = rates(&p.rest)?;
    let variants = p.rest.get("variants").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut windows = Vec::new();
    for w in p.rest.get("windows").and_then(Value::as_array).into_iter().flatten() {
        let variant = w["variant"].as_str().unwrap_or_default();
        let Some(r) = variants.get(variant).and_then(Value::as_object).and_then(rates) else { continue };
        let days: Vec<&str> = w["weekdays"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d.as_u64())
            .filter(|d| (1..=7).contains(d))
            .map(|d| WEEKDAYS[d as usize - 1])
            .collect();
        let spans: Vec<&str> = w["hours_utc"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        for (i, span) in spans.iter().enumerate() {
            let Some((from, to)) = span.split_once('-') else { continue };
            let to = if to == "00:00" { "24:00" } else { to };
            let slug = variant.replace('_', "-");
            let name = if spans.len() == 1 { slug } else { format!("{slug}-{}", i + 1) };
            let mut win = json!({ "name": name, "from_time": from, "to_time": to, "rates": r });
            if !days.is_empty() {
                win["weekdays"] = json!(days);
            }
            windows.push(win);
        }
    }
    let tiers: Vec<Value> = p
        .rest
        .get("tiers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let above = t["above_input_tokens"].as_u64()?;
            let r = rates(t.as_object()?)?;
            Some(json!({ "above": above, "unit": "input_tokens", "rates": r }))
        })
        .collect();
    let mut out = json!({ "valid_from": p.effective_from.to_rfc3339(), "rates": base });
    if !windows.is_empty() {
        out["windows"] = json!(windows);
    }
    if !tiers.is_empty() {
        out["tiers"] = json!(tiers);
    }
    Some(out)
}

/// The pricing of an offering: the versions that were or are in force, oldest first, in the
/// currency of the newest. A currency change starts over, since the broker cannot change a
/// resource's currency while a period is in force.
pub fn pricing(prices: &[Price]) -> Option<Value> {
    let mut kept: Vec<&Price> =
        prices.iter().filter(|p| matches!(p.status.as_str(), "" | "active" | "superseded")).collect();
    kept.sort_by(|a, b| a.effective_from.cmp(&b.effective_from).then(a.version.cmp(&b.version)));
    let currency = kept.last()?.currency.clone();
    let mut by_start: BTreeMap<String, Value> = BTreeMap::new();
    for p in kept.iter().filter(|p| p.currency == currency) {
        if let Some(per) = period(p) {
            // Two versions from the same instant: the later version wins.
            by_start.insert(p.effective_from.to_rfc3339(), per);
        }
    }
    if by_start.is_empty() {
        return None;
    }
    Some(json!({ "currency": currency, "timezone": "UTC", "periods": by_start.into_values().collect::<Vec<_>>() }))
}

/// The live state of an offering from the gateway's routing view: degraded when every pool
/// model the gateway has used has its breaker open; calls in flight; the best time to first byte.
pub fn live(pool: &[String], targets: &[RoutingTarget]) -> (Option<LiveStatus>, Option<u32>, Option<f64>) {
    let mine: Vec<&RoutingTarget> = targets.iter().filter(|t| pool.contains(&t.model)).collect();
    if mine.is_empty() {
        return (Some(LiveStatus::Active), None, None);
    }
    let all_open = mine.iter().all(|t| t.breaker == "open");
    let in_flight = mine.iter().map(|t| t.in_flight).sum();
    let ttft =
        mine.iter().filter_map(|t| t.first_byte_ms).fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.min(v))));
    (Some(if all_open { LiveStatus::Degraded } else { LiveStatus::Active }), Some(in_flight), ttft)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(v: Value) -> Price {
        serde_json::from_value(v).unwrap()
    }

    fn offered() -> Offered {
        Offered {
            offering: Offering {
                workspace: "prod".into(),
                template: "legal".into(),
                visibility: "platform".into(),
                name: None,
                summary: Some("Contract review".into()),
            },
            workspace: Named { id: "ws_1".into(), name: "prod".into(), ..Named::default() },
            template: Template { id: "tpl_1".into(), name: "legal".into(), current_version: 3, ..Template::default() },
            model: Some(Model {
                name: "openai/gpt-4o".into(),
                kind: "chat".into(),
                context_window: Some(128_000),
                ..Model::default()
            }),
            prices: vec![
                price(
                    json!({"version": 2, "currency": "EUR", "effective_from": "2026-06-01T00:00:00Z", "status": "active",
                    "input": 2.0, "output": 8.0, "cache_write_5m": 2.5, "per_character": 0.1,
                    "variants": {"off_peak": {"input": 1.0}},
                    "windows": [{"variant": "off_peak", "weekdays": [6, 7], "hours_utc": ["00:00-00:00"]}],
                    "tiers": [{"above_input_tokens": 200000, "input": 4.0}]}),
                ),
                price(
                    json!({"version": 1, "currency": "EUR", "effective_from": "2026-01-01T00:00:00Z", "status": "superseded", "input": 2.5, "output": 10.0}),
                ),
                price(
                    json!({"version": 3, "currency": "EUR", "effective_from": "2026-12-01T00:00:00Z", "status": "proposed", "input": 1.0}),
                ),
            ],
        }
    }

    #[test]
    fn an_offering_becomes_a_resource_that_validates() {
        let org = Named { id: "org_1".into(), name: "acme".into(), ..Named::default() };
        let site =
            Site { public_url: "https://gw.example".into(), obtain_url: "https://console.example".into(), ttl_s: 60 };
        let a = announcement(&org, &[offered()], &site).unwrap();
        assert_eq!(a.provider.name, "gw-acme");
        let v = serde_json::to_value(&a.resources[0]).unwrap();
        assert_eq!(v["key"], "prod.legal");
        assert_eq!(v["kind"], "model");
        assert_eq!(v["model"]["upstream_id"], "@legal");
        assert_eq!(v["access"]["visibility"], "platform");
        assert_eq!(v["interfaces"][0]["protocol"], "pacod-gw");
        assert_eq!(v["interfaces"][1]["url"], "https://gw.example/v1");
        assert_eq!(v["interfaces"][0]["auth"]["obtain_url"], "https://console.example");
    }

    #[test]
    fn prices_keep_their_history_and_gws_names_map() {
        let p = pricing(&offered().prices).unwrap();
        assert_eq!(p["currency"], "EUR");
        let periods = p["periods"].as_array().unwrap();
        assert_eq!(periods.len(), 2, "the superseded and the active version; never the proposed one");
        assert_eq!(periods[0]["valid_from"], "2026-01-01T00:00:00+00:00");
        assert_eq!(periods[0]["rates"]["input"], 2.5);
        let now = &periods[1];
        assert_eq!(
            now["rates"],
            json!({"input": 2.0, "output": 8.0, "cache_write": 2.5}),
            "per_character has no broker field"
        );
        assert_eq!(
            now["windows"][0],
            json!({"name": "off-peak", "weekdays": ["sat", "sun"], "from_time": "00:00", "to_time": "24:00", "rates": {"input": 1.0}})
        );
        assert_eq!(now["tiers"][0], json!({"above": 200000, "unit": "input_tokens", "rates": {"input": 4.0}}));
    }

    #[test]
    fn visibility_organisation_names_the_gateways_organisation() {
        let mut o = offered();
        o.offering.visibility = "organisation".into();
        let org = Named { id: "org_9".into(), name: "acme".into(), ..Named::default() };
        let site = Site { public_url: "https://gw.example".into(), obtain_url: "https://gw.example".into(), ttl_s: 60 };
        let a = announcement(&org, &[o], &site).unwrap();
        assert_eq!(serde_json::to_value(&a.resources[0]).unwrap()["access"]["visibility"], "organisation:org_9");
    }

    #[test]
    fn live_state_follows_the_breakers() {
        let pool = vec!["a".to_string(), "b".to_string()];
        let t = |model: &str, breaker: &str, in_flight, ttfb| RoutingTarget {
            model: model.into(),
            breaker: breaker.into(),
            in_flight,
            first_byte_ms: ttfb,
        };
        let (s, n, ttft) =
            live(&pool, &[t("a", "open", 1, Some(400.0)), t("b", "closed", 2, Some(250.0)), t("c", "open", 9, None)]);
        assert_eq!((s, n, ttft), (Some(LiveStatus::Active), Some(3), Some(250.0)));
        let (s, ..) = live(&pool, &[t("a", "open", 0, None), t("b", "open", 0, None)]);
        assert_eq!(s, Some(LiveStatus::Degraded));
    }
}
