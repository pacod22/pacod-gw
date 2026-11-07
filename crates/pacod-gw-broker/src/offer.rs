//! What an organisation offers, as a broker announcement (docs/pacod-gw.md in pacod-broker, R1
//! and section 5). Pure: the gateway's views in, an announcement out.

use std::collections::{BTreeMap, HashMap, HashSet};

use pacod_broker_client::Announcement;
use pacod_broker_client::model::LiveStatus;
use serde_json::{Map, Value, json};

use crate::config::Offering;
use crate::gw::{Model, Named, OfferedModel, Price, RoutingTarget, Template};

/// What is offered: a template, called as `@name`, or a model of the repository, called by its
/// name.
#[derive(Debug, Clone)]
pub enum Subject {
    Template(Template),
    Model(String),
}

/// How an offering shows on the broker.
#[derive(Debug, Clone, PartialEq)]
pub struct Terms {
    /// `organisation` (the organisation's own members), `platform` or `public`.
    pub visibility: String,
    /// The resource's name; the template's, or the model's in lower case, when none.
    pub name: Option<String>,
    /// One line for consumers; the template's description, or a sentence, when none.
    pub summary: Option<String>,
}

impl Terms {
    fn of(o: &Offering) -> Self {
        Self { visibility: o.visibility.clone(), name: o.name.clone(), summary: o.summary.clone() }
    }

    /// The terms of a switch (`broker_offer`): no visibility means the organisation's own.
    fn switched(b: &crate::gw::BrokerOffer) -> Self {
        Self {
            visibility: if b.visibility.is_empty() { "organisation".into() } else { b.visibility.clone() },
            name: None,
            summary: (!b.summary.is_empty()).then(|| b.summary.clone()),
        }
    }
}

/// What a call names to reach an offering, for finding the offering a ticketed call went to: a
/// call records its template, or, without one, the model it asked for. A template call asks
/// for `@name`, so the two never mix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Called {
    Template(String),
    Model(String),
}

/// One offering with what the announcement needs about it.
#[derive(Debug, Clone)]
pub struct Offered {
    pub terms: Terms,
    pub subject: Subject,
    /// The workspace whose template it is; none for a template of the organisation, and for a
    /// model.
    pub workspace: Option<Named>,
    /// The workspaces, by id, whose keys reach the offering: a template's own workspace, or each
    /// workspace of the organisation without a template of that name of its own; for a model,
    /// each one that may call it.
    pub reached_by: Vec<String>,
    /// The model whose kind, limits and prices the offering shows: the pool's first, or the
    /// model offered.
    pub model: Option<Model>,
    /// That model's price versions, newest first.
    pub prices: Vec<Price>,
}

impl Offered {
    /// The model names whose breakers make the offering's live state.
    pub fn pool(&self) -> Vec<String> {
        match &self.subject {
            Subject::Template(t) => t.spec.pool(),
            Subject::Model(name) => vec![name.clone()],
        }
    }

    pub fn called(&self) -> Called {
        match &self.subject {
            Subject::Template(t) => Called::Template(t.name.clone()),
            Subject::Model(name) => Called::Model(name.clone()),
        }
    }
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

/// A model's resource key: `model:` and its name in lower case, when that is a broker slug (a-z,
/// 0-9, `- _ . : /`, starting with a letter or digit) of at most 96 characters. The gateway
/// refuses to offer a model whose name is not; the broker would refuse the whole announcement.
pub fn model_key(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let slug = lower.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && lower.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.:/".contains(&b));
    let key = format!("model:{lower}");
    (slug && key.len() <= 96).then_some(key)
}

/// The resource key of an offering: the workspace and the template, both slugs in the gateway,
/// `org:` and the template for the organisation's own, or `model:` and the model's name. A
/// workspace's or template's slug has no colon, so none of the three is another's.
pub fn key(o: &Offered) -> String {
    match (&o.subject, &o.workspace) {
        (Subject::Template(t), Some(ws)) => format!("{}.{}", ws.name, t.name),
        (Subject::Template(t), None) => format!("org:{}", t.name),
        // `choose` offers only a model whose name makes a key.
        (Subject::Model(name), _) => model_key(name).unwrap_or_else(|| format!("model:{}", name.to_ascii_lowercase())),
    }
}

/// What an organisation offers: the file's list, then each template whose switch is on
/// (`broker_offer`), its workspaces' and its own, then each granted model it offers. `own` holds
/// each workspace's templates by workspace id, `shared` the organisation's. Model and prices are
/// left for the caller to read. Only active templates are offered, an organisation's template
/// only while a workspace reaches it, and a model only while a workspace may call it.
pub fn choose(
    org: &Named,
    file: &[Offering],
    workspaces: &[Named],
    own: &HashMap<String, Vec<Template>>,
    shared: &[Template],
    models: &[OfferedModel],
) -> Vec<Offered> {
    let none = Vec::new();
    let own_of = |ws: &Named| own.get(&ws.id).unwrap_or(&none);
    let mut offers: Vec<(Offering, Terms)> = file.iter().map(|o| (o.clone(), Terms::of(o))).collect();
    let switched = workspaces
        .iter()
        .flat_map(|ws| own_of(ws).iter().map(move |t| (Some(ws), t)))
        .chain(shared.iter().map(|t| (None, t)));
    for (ws, t) in switched {
        let Some(b) = &t.spec.broker_offer else { continue };
        // The file's entry for the same template wins: it may name or summarise it.
        if offers.iter().any(|(x, _)| x.names(ws.map(|w| (w.id.as_str(), w.name.as_str())), &t.name)) {
            continue;
        }
        let terms = Terms::switched(b);
        let named = Offering {
            workspace: ws.map(|w| w.name.clone()),
            template: t.name.clone(),
            visibility: terms.visibility.clone(),
            name: None,
            summary: None,
        };
        offers.push((named, terms));
    }
    let mut out = Vec::new();
    for (x, terms) in &offers {
        let (workspace, template, reached_by) = match &x.workspace {
            Some(w) => {
                let Some(ws) = workspaces.iter().find(|ws| ws.name == *w || ws.id == *w) else {
                    tracing::warn!(organisation = %org.name, workspace = %w, "no such workspace");
                    continue;
                };
                let Some(t) = own_of(ws).iter().find(|t| t.name == x.template || t.id == x.template) else {
                    tracing::warn!(organisation = %org.name, workspace = %ws.name, template = %x.template, "no such template");
                    continue;
                };
                (Some(ws.clone()), t, vec![ws.id.clone()])
            }
            None => {
                let Some(t) = shared.iter().find(|t| t.name == x.template || t.id == x.template) else {
                    tracing::warn!(organisation = %org.name, template = %x.template, "the organisation has no such template");
                    continue;
                };
                // A workspace's own template of the same name hides the organisation's there,
                // also while it is disabled: that workspace's calls never reach this one.
                let reached_by: Vec<String> = workspaces
                    .iter()
                    .filter(|ws| !own_of(ws).iter().any(|o| o.name == t.name))
                    .map(|ws| ws.id.clone())
                    .collect();
                if reached_by.is_empty() {
                    tracing::warn!(
                        organisation = %org.name,
                        template = %t.name,
                        "no workspace reaches the organisation's template (each has its own of that name); not offered"
                    );
                    continue;
                }
                (None, t, reached_by)
            }
        };
        if !template.status.is_empty() && template.status != "active" {
            continue;
        }
        out.push(Offered {
            terms: terms.clone(),
            subject: Subject::Template(template.clone()),
            workspace,
            reached_by,
            model: None,
            prices: Vec::new(),
        });
    }
    // Each key once: the broker refuses an announcement that names one twice.
    let mut keys: HashSet<String> = out.iter().map(key).collect();
    for m in models {
        let Some(b) = &m.offer else { continue };
        let Some(k) = model_key(&m.name) else {
            tracing::warn!(organisation = %org.name, model = %m.name, "the model's name is not a broker key; not offered");
            continue;
        };
        if m.reached_by.is_empty() {
            tracing::warn!(organisation = %org.name, model = %m.name, "no workspace may call the model; not offered");
            continue;
        }
        if !keys.insert(k) {
            tracing::warn!(organisation = %org.name, model = %m.name, "another offered model has the same key; not offered");
            continue;
        }
        out.push(Offered {
            terms: Terms::switched(b),
            subject: Subject::Model(m.name.clone()),
            workspace: None,
            reached_by: m.reached_by.clone(),
            model: None,
            prices: Vec::new(),
        });
    }
    out
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
    let (upstream, name, summary, whose, tag) = match &o.subject {
        Subject::Template(t) => {
            let summary = if t.description.is_empty() {
                format!("Template {} of {} on pacod-gw: call model @{}", t.name, org.name, t.name)
            } else {
                t.description.clone()
            };
            let whose = match &o.workspace {
                Some(ws) => format!(
                    "workspace {}, template {} (version {}). Call it as model `@{}` with a key of that workspace.",
                    ws.name, t.name, t.current_version, t.name
                ),
                None => format!(
                    "template {} of the organisation (version {}). Call it as model `@{}` with a key of one of its \
                     workspaces.",
                    t.name, t.current_version, t.name
                ),
            };
            // Callers name the template, not an upstream model: the gateway picks within its pool.
            (format!("@{}", t.name), t.name.clone(), summary, whose, "template")
        }
        Subject::Model(m) => (
            m.clone(),
            m.to_ascii_lowercase(),
            format!("Model {m} of {} on pacod-gw", org.name),
            format!("model {m}. Call it as model `{m}` with a key of one of its workspaces that may use it."),
            "model",
        ),
    };
    let mut model = Map::new();
    model.insert("upstream_id".into(), json!(upstream));
    if let Some(m) = &o.model {
        if let Some(c) = m.context_window {
            model.insert("context_window".into(), json!(c));
        }
        if let Some(c) = m.max_output_tokens {
            model.insert("max_output_tokens".into(), json!(c));
        }
    }
    let visibility = match o.terms.visibility.as_str() {
        "organisation" => format!("organisation:{}", org.id),
        v => v.to_string(),
    };
    let mut r = json!({
        "key": key(o),
        "kind": kind(model_kind),
        "name": o.terms.name.clone().unwrap_or(name),
        "summary": o.terms.summary.clone().unwrap_or(summary),
        "description": format!("Offered by organisation {} through its gateway, {whose}", org.name),
        "tags": ["gateway", tag],
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
            terms: Terms { visibility: "platform".into(), name: None, summary: Some("Contract review".into()) },
            workspace: Some(Named { id: "ws_1".into(), name: "prod".into(), ..Named::default() }),
            reached_by: vec!["ws_1".into()],
            subject: Subject::Template(Template {
                id: "tpl_1".into(),
                name: "legal".into(),
                current_version: 3,
                ..Template::default()
            }),
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
        o.terms.visibility = "organisation".into();
        let org = Named { id: "org_9".into(), name: "acme".into(), ..Named::default() };
        let site = Site { public_url: "https://gw.example".into(), obtain_url: "https://gw.example".into(), ttl_s: 60 };
        let a = announcement(&org, &[o], &site).unwrap();
        assert_eq!(serde_json::to_value(&a.resources[0]).unwrap()["access"]["visibility"], "organisation:org_9");
    }

    #[test]
    fn the_organisations_template_has_a_key_of_its_own() {
        let mut o = offered();
        o.workspace = None;
        let org = Named { id: "org_1".into(), name: "acme".into(), ..Named::default() };
        let site = Site { public_url: "https://gw.example".into(), obtain_url: "https://gw.example".into(), ttl_s: 60 };
        let a = announcement(&org, &[offered(), o], &site).unwrap();
        let v = serde_json::to_value(&a.resources).unwrap();
        assert_eq!((&v[0]["key"], &v[1]["key"]), (&json!("prod.legal"), &json!("org:legal")), "both, side by side");
        assert_eq!(v[1]["model"]["upstream_id"], "@legal");
        let d = v[1]["description"].as_str().unwrap();
        assert!(d.contains("template legal of the organisation") && d.contains("one of its workspaces"), "{d}");
    }

    fn template(id: &str, name: &str, offer: Option<&str>, status: &str) -> Template {
        let mut t = Template { id: id.into(), name: name.into(), status: status.into(), ..Template::default() };
        t.spec.model = "openai/gpt-4o".into();
        t.spec.broker_offer = offer.map(|v| crate::gw::BrokerOffer { visibility: v.into(), summary: String::new() });
        t
    }

    #[test]
    fn an_organisations_template_is_offered_where_its_workspaces_reach_it() {
        let org = Named { id: "org_1".into(), name: "acme".into(), ..Named::default() };
        let ws = |id: &str, name: &str| Named { id: id.into(), name: name.into(), ..Named::default() };
        let workspaces = vec![ws("ws_1", "prod"), ws("ws_2", "lab"), ws("ws_3", "old")];
        let own = HashMap::from([
            // prod offers its own "legal"; lab has no templates; old hides "triage" with a
            // disabled one of its own.
            ("ws_1".to_string(), vec![template("tpl_1", "legal", Some("platform"), "active")]),
            ("ws_3".to_string(), vec![template("tpl_3", "triage", None, "disabled")]),
        ]);
        let shared = vec![
            template("otp_1", "triage", Some(""), "active"),
            template("otp_2", "legal", None, "active"),
            template("otp_3", "draft", Some("public"), "disabled"),
        ];
        let file = vec![Offering {
            workspace: None,
            template: "legal".into(),
            visibility: "public".into(),
            name: None,
            summary: Some("From the file".into()),
        }];
        let got = choose(&org, &file, &workspaces, &own, &shared, &[]);
        let seen: Vec<(String, Vec<String>, String)> =
            got.iter().map(|o| (key(o), o.reached_by.clone(), o.terms.visibility.clone())).collect();
        assert_eq!(
            seen,
            vec![
                // The file's entry: the organisation's "legal", which prod's own hides.
                ("org:legal".to_string(), vec!["ws_2".to_string(), "ws_3".to_string()], "public".to_string()),
                ("prod.legal".to_string(), vec!["ws_1".to_string()], "platform".to_string()),
                // The switch with no visibility means the organisation's own members.
                ("org:triage".to_string(), vec!["ws_1".to_string(), "ws_2".to_string()], "organisation".to_string()),
            ],
            "a disabled template is not offered"
        );
        assert_eq!(got[0].terms.summary.as_deref(), Some("From the file"));

        // Hidden in every workspace: nothing would reach it.
        let own = HashMap::from([
            ("ws_1".to_string(), vec![template("tpl_1", "triage", None, "active")]),
            ("ws_2".to_string(), vec![template("tpl_2", "triage", None, "active")]),
            ("ws_3".to_string(), vec![template("tpl_3", "triage", None, "active")]),
        ]);
        assert!(choose(&org, &[], &workspaces, &own, &shared[..1], &[]).is_empty());
    }

    fn granted(name: &str, offer: Option<&str>, reached_by: &[&str]) -> OfferedModel {
        OfferedModel {
            model_id: format!("mdl_{name}"),
            name: name.into(),
            offer: offer.map(|v| crate::gw::BrokerOffer { visibility: v.into(), summary: String::new() }),
            reached_by: reached_by.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_granted_model_is_offered_by_its_name() {
        let org = Named { id: "org_1".into(), name: "acme".into(), ..Named::default() };
        let models = [
            granted("Qwen/Qwen2.5-72B", Some("public"), &["ws_1", "ws_2"]),
            granted("openai/gpt-4o", None, &["ws_1"]),
            granted("openai/nobody", Some("platform"), &[]),
            granted("_odd/x", Some("platform"), &["ws_1"]),
            // The same key as the first: the broker would refuse both, so one is left out.
            granted("qwen/qwen2.5-72b", Some("platform"), &["ws_1"]),
            granted("openai/gpt-4o-mini", Some(""), &["ws_1"]),
        ];
        let got = choose(&org, &[], &[], &HashMap::new(), &[], &models);
        let seen: Vec<(String, String, Vec<String>)> =
            got.iter().map(|o| (key(o), o.terms.visibility.clone(), o.reached_by.clone())).collect();
        assert_eq!(
            seen,
            vec![
                ("model:qwen/qwen2.5-72b".into(), "public".into(), vec!["ws_1".into(), "ws_2".into()]),
                ("model:openai/gpt-4o-mini".into(), "organisation".into(), vec!["ws_1".into()]),
            ],
            "not offered, called by no workspace, not a broker key, a key taken: each left out"
        );
        assert_eq!(got[0].called(), Called::Model("Qwen/Qwen2.5-72B".into()));
        assert_eq!(got[0].pool(), vec!["Qwen/Qwen2.5-72B".to_string()]);

        // Beside a template, as one announcement the broker takes.
        let mut m = got[0].clone();
        m.model = Some(Model { name: "Qwen/Qwen2.5-72B".into(), kind: "chat".into(), ..Model::default() });
        m.prices = offered().prices;
        let site = Site { public_url: "https://gw.example".into(), obtain_url: "https://gw.example".into(), ttl_s: 60 };
        let a = announcement(&org, &[offered(), m], &site).unwrap();
        let v = serde_json::to_value(&a.resources[1]).unwrap();
        assert_eq!(v["key"], "model:qwen/qwen2.5-72b");
        assert_eq!(v["name"], "qwen/qwen2.5-72b", "a broker name is a slug");
        assert_eq!(v["model"]["upstream_id"], "Qwen/Qwen2.5-72B", "callers name the model as the gateway knows it");
        assert_eq!(v["tags"], json!(["gateway", "model"]));
        assert_eq!(v["access"]["visibility"], "public");
        assert_eq!(v["pricing"]["periods"].as_array().map(Vec::len), Some(2), "the model's own prices");
        let d = v["description"].as_str().unwrap();
        assert!(d.contains("model Qwen/Qwen2.5-72B") && d.contains("workspaces that may use it"), "{d}");
    }

    #[test]
    fn the_model_key_is_the_gateways_rule() {
        assert_eq!(model_key("openai/gpt-4o").as_deref(), Some("model:openai/gpt-4o"));
        assert_eq!(model_key("ollama/llama3:8b").as_deref(), Some("model:ollama/llama3:8b"));
        assert_eq!(model_key("-x"), None);
        assert!(model_key(&"a".repeat(90)).is_some());
        assert_eq!(model_key(&"a".repeat(91)), None, "model: and 91 characters are more than 96");
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
