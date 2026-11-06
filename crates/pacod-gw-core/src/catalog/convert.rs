//! A catalogue entry as the gateway's own objects: the price candidate the price job decides
//! on, the facts it writes to a model, and for the lite gateway a model's kind, capabilities and
//! price. The catalogue names rates, tiers and windows as the gateway does, so a price converts
//! field by field.

use pacod_agg_catalog as cat;
use pacod_gw_events::jobs::{CandidateRates, CandidateTier, CandidateWindow, PriceCandidate};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::model::{Capabilities, ModelKind, ParamRange, ParamSupport, PriceTier, PriceWindow, Rates};

/// The modalities a model can name. Anything else is a misreading.
pub const MODALITIES: &[&str] = &["text", "image", "document", "audio", "video", "embedding", "ranking"];

/// One shape as another with the same field names.
fn same<T: DeserializeOwned>(v: &impl serde::Serialize) -> T {
    serde_json::from_value(serde_json::to_value(v).expect("serialisable")).expect("the same fields")
}

pub fn rates(r: &cat::Rates) -> Rates {
    same(r)
}

pub fn tiers(p: &cat::Price) -> Vec<PriceTier> {
    p.tiers.iter().map(same).collect()
}

pub fn windows(p: &cat::Price) -> Vec<PriceWindow> {
    p.windows.iter().map(same).collect()
}

/// The entry's price as a candidate for the model `model_id`. `source` is the candidate's
/// source (`feed` for a sync or the models file, `import` for an upload).
pub fn candidate(model_id: &str, p: &cat::Price, source: &str, fallback_url: &str) -> PriceCandidate {
    let rates: CandidateRates = same(&p.rates);
    let evidence = p.evidence.as_ref();
    let evidence_text = evidence.and_then(|e| e.text.clone()).unwrap_or_else(|| {
        let named: serde_json::Map<String, Value> =
            rates.named().into_iter().filter_map(|(k, v)| Some((k.to_owned(), json!(v?)))).collect();
        Value::Object(named).to_string()
    });
    PriceCandidate {
        model_id: model_id.to_owned(),
        currency: p.currency.clone(),
        rates,
        variants: p.variants.iter().map(|(k, r)| (k.clone(), same(r))).collect(),
        tiers: p.tiers.iter().map(same::<CandidateTier>).collect(),
        windows: p.windows.iter().map(same::<CandidateWindow>).collect(),
        source: source.to_owned(),
        evidence_url: evidence.and_then(|e| e.url.clone()).unwrap_or_else(|| fallback_url.to_owned()),
        evidence_text,
        checked_at: p.checked_at,
    }
}

/// What the entry states about a model, by the field names `model_fact` and
/// `write_model_fields` take. Unknown values are left out.
pub fn facts(m: &cat::Model) -> Vec<(&'static str, Value)> {
    let p = &m.properties;
    let mut out = Vec::new();
    if let Some(n) = p.context_window {
        out.push(("context_window", json!(n)));
    }
    if let Some(n) = p.max_output_tokens {
        out.push(("max_output_tokens", json!(n)));
    }
    for (field, v) in [
        ("capabilities.vision", p.vision),
        ("capabilities.tools", p.tools),
        ("capabilities.structured_output", p.structured_output),
        ("capabilities.json_mode", p.json_mode),
    ] {
        if let Some(b) = v {
            out.push((field, json!(b)));
        }
    }
    if let Some(list) = p.reasoning_efforts.as_ref().filter(|l| !l.is_empty()) {
        out.push(("capabilities.reasoning_efforts", json!(list)));
    }
    if let Some(e) = p.default_effort.as_ref().filter(|e| !e.is_empty()) {
        out.push(("capabilities.default_effort", json!(e)));
    }
    if let Some(mo) = &p.modalities {
        for (field, list) in [("modalities.input", &mo.input), ("modalities.output", &mo.output)] {
            if !list.is_empty() {
                out.push((field, json!(list)));
            }
        }
    }
    out
}

/// Whether a stated fact can be true of a model at all. A misreading (a price read as a
/// context window, a modality the gateway does not know) is left out.
pub fn plausible(field: &str, value: &Value, context_window: Option<u64>) -> bool {
    match field {
        "context_window" => value.as_u64().is_some_and(|n| (1_000..=100_000_000).contains(&n)),
        "max_output_tokens" => {
            value.as_u64().is_some_and(|n| (1..=10_000_000).contains(&n) && context_window.is_none_or(|w| n <= w))
        }
        "modalities.input" | "modalities.output" => value
            .as_array()
            .is_some_and(|l| !l.is_empty() && l.iter().all(|m| m.as_str().is_some_and(|m| MODALITIES.contains(&m)))),
        "capabilities.reasoning_efforts" => value
            .as_array()
            .is_some_and(|l| !l.is_empty() && l.len() <= 8 && l.iter().all(|e| e.as_str().is_some_and(effort_word))),
        "capabilities.default_effort" => value.as_str().is_some_and(effort_word),
        _ => value.is_boolean(),
    }
}

fn effort_word(e: &str) -> bool {
    !e.is_empty() && e.len() <= 16 && e.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// The gateway's kind of model for an entry's kind.
pub fn kind(m: &cat::Model) -> Option<ModelKind> {
    Some(match m.kind? {
        cat::ModelKind::Chat => ModelKind::Chat,
        cat::ModelKind::Embedding => ModelKind::Embedding,
        cat::ModelKind::Image => ModelKind::Image,
        cat::ModelKind::Speech => ModelKind::Speech,
        cat::ModelKind::Transcription => ModelKind::Transcription,
        cat::ModelKind::Rerank => ModelKind::Rerank,
        cat::ModelKind::Realtime => ModelKind::Realtime,
        cat::ModelKind::Other => return None,
    })
}

fn support(p: &cat::Parameter) -> Option<ParamSupport> {
    Some(match p.support? {
        cat::Support::Accepted => ParamSupport::Accepted,
        cat::Support::Rejected => ParamSupport::Rejected,
        cat::Support::DefaultOnly => ParamSupport::DefaultOnly,
        cat::Support::Other => return None,
    })
}

fn range(p: &cat::Parameter) -> Option<ParamRange> {
    let r = ParamRange { min: p.min, max: p.max, default: p.default.as_ref().and_then(Value::as_f64) };
    (r.min.is_some() || r.max.is_some() || r.default.is_some()).then_some(r)
}

/// The capabilities a file states for a model, laid over `base` (the preset the gateway would
/// guess): what it can do, and how it treats the sampling parameters and `tool_choice`. Only
/// what the entry states changes. The lite gateway takes a models file this way; the full
/// gateway writes facts one by one ([`facts`]).
pub fn capabilities(m: &cat::Model, mut base: Capabilities) -> Capabilities {
    let p = &m.properties;
    let set = |slot: &mut bool, v: Option<bool>| {
        if let Some(v) = v {
            *slot = v;
        }
    };
    set(&mut base.vision, p.vision);
    set(&mut base.documents, p.documents);
    set(&mut base.tools, p.tools);
    set(&mut base.structured_output, p.structured_output);
    set(&mut base.json_mode, p.json_mode);
    if let Some(tools) = &p.server_tools {
        base.server_tools = !tools.is_empty();
    }
    if let Some(list) = p.reasoning_efforts.as_ref().filter(|l| !l.is_empty()) {
        base.reasoning_efforts = list.clone();
    }
    if let Some(e) = p.default_effort.as_ref().filter(|e| !e.is_empty()) {
        base.default_effort = Some(e.clone());
    }
    let param = |name: &str| m.parameters.get(name);
    for (name, slot) in [("temperature", &mut base.temperature), ("top_p", &mut base.top_p), ("top_k", &mut base.top_k)]
    {
        if let Some(s) = param(name).and_then(support) {
            *slot = s;
        }
    }
    // The gateway treats both penalties as one; a file that refuses either refuses them.
    let penalties: Vec<ParamSupport> =
        ["frequency_penalty", "presence_penalty"].iter().filter_map(|n| param(n).and_then(support)).collect();
    if let Some(worst) = penalties.iter().copied().max_by_key(|s| match s {
        ParamSupport::Accepted => 0,
        ParamSupport::DefaultOnly => 1,
        ParamSupport::Rejected => 2,
    }) {
        base.penalties = worst;
    }
    let ranges = &mut base.sampling;
    for (name, slot) in [
        ("temperature", &mut ranges.temperature),
        ("top_p", &mut ranges.top_p),
        ("top_k", &mut ranges.top_k),
        ("frequency_penalty", &mut ranges.frequency_penalty),
        ("presence_penalty", &mut ranges.presence_penalty),
    ] {
        if let Some(r) = param(name).and_then(range) {
            *slot = Some(r);
        }
    }
    if let Some(s) = param("tool_choice").and_then(support) {
        let forced = s == ParamSupport::Accepted;
        base.tool_choice_forced = forced;
        base.tool_choice_none = forced;
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> cat::Model {
        serde_json::from_value(json!({
            "vendor": "openai", "api_id": "gpt-x", "kind": "chat",
            "properties": { "context_window": 400000, "max_output_tokens": 128000, "vision": true, "tools": true,
                            "reasoning_efforts": ["low", "high"], "default_effort": "low",
                            "modalities": { "input": ["text", "image"], "output": ["text"] } },
            "parameters": { "temperature": { "support": "default_only", "default": 1 },
                            "top_p": { "support": "rejected" },
                            "presence_penalty": { "support": "accepted", "min": -2, "max": 2 } },
            "price": { "currency": "USD", "rates": { "input": 2.0, "output": 10.0 },
                       "variants": { "batch": { "input": 1.0 } },
                       "tiers": [{ "above_input_tokens": 200000, "input": 4.0 }],
                       "windows": [{ "variant": "batch", "weekdays": [6, 7], "hours_utc": ["00:00-00:00"] }],
                       "checked_at": "2026-11-01T00:00:00Z" }
        }))
        .unwrap()
    }

    #[test]
    fn a_price_converts_field_by_field() {
        let e = entry();
        let p = e.price.as_ref().unwrap();
        let c = candidate("mod_1", p, "feed", "https://example.com/catalog");
        assert_eq!((c.rates.input, c.rates.output, c.variants["batch"].input), (Some(2.0), Some(10.0), Some(1.0)));
        assert_eq!((c.tiers[0].above_input_tokens, c.tiers[0].rates.input), (200_000, Some(4.0)));
        assert_eq!(c.windows[0].weekdays, [6, 7]);
        assert_eq!(c.evidence_url, "https://example.com/catalog");
        assert_eq!(c.checked_at.map(|t| t.to_rfc3339()).as_deref(), Some("2026-11-01T00:00:00+00:00"));
        assert_eq!(rates(&p.rates).output, Some(10.0));
        assert_eq!(tiers(p)[0].rates.input, Some(4.0));
        assert_eq!(windows(p)[0].variant, "batch");
    }

    #[test]
    fn facts_are_named_as_the_model_names_them_and_misreadings_are_left_out() {
        let names: Vec<&str> = facts(&entry()).into_iter().map(|(f, _)| f).collect();
        assert_eq!(
            names,
            [
                "context_window",
                "max_output_tokens",
                "capabilities.vision",
                "capabilities.tools",
                "capabilities.reasoning_efforts",
                "capabilities.default_effort",
                "modalities.input",
                "modalities.output"
            ]
        );
        assert!(plausible("context_window", &json!(400_000), None));
        assert!(!plausible("context_window", &json!(12), None));
        assert!(!plausible("max_output_tokens", &json!(500_000), Some(400_000)));
        assert!(!plausible("modalities.input", &json!(["text", "smell"]), None));
        assert!(!plausible("capabilities.default_effort", &json!("Very High!"), None));
    }

    #[test]
    fn capabilities_change_only_where_the_entry_says_so() {
        let base = Capabilities::default();
        let c = capabilities(&entry(), base.clone());
        assert!(c.vision && c.tools);
        assert_eq!(c.documents, base.documents, "not stated, so the preset's");
        assert_eq!((c.temperature, c.top_p), (ParamSupport::DefaultOnly, ParamSupport::Rejected));
        assert_eq!(c.sampling.temperature.as_ref().and_then(|r| r.default), Some(1.0));
        assert_eq!(c.sampling.presence_penalty.as_ref().and_then(|r| r.max), Some(2.0));
        assert_eq!(c.reasoning_efforts, ["low", "high"]);
        assert_eq!(kind(&entry()), Some(ModelKind::Chat));
    }
}
