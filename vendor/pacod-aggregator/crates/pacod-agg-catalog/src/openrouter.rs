//! OpenRouter's list of models (`GET https://openrouter.ai/api/v1/models`). Ids name the model's
//! author (`anthropic/claude-haiku-5.5`), prices are per token as strings in US dollars, and
//! `supported_parameters` says what OpenRouter passes on, not what the vendor's API takes.
//!
//! The author is not always the host: for open-weight models (DeepSeek, GLM, Kimi, Qwen, …) the
//! price is usually a third party's. The reader keeps it; a consumer decides which vendors'
//! prices it trusts.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

use crate::v1::{
    Catalog, Evidence, Modalities, Model, ModelKind, Parameter, Price, Properties, Rates, Source, SourceKind, Support,
    Tier, Vendor,
};
use crate::vendors;

/// Where OpenRouter serves the list.
pub const URL: &str = "https://openrouter.ai/api/v1/models";

/// The request parameters a catalogue names, by OpenRouter's names for them.
const PARAMETERS: &[&str] = &[
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "frequency_penalty",
    "presence_penalty",
    "repetition_penalty",
    "seed",
    "stop",
    "max_tokens",
    "logit_bias",
    "logprobs",
    "top_logprobs",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "response_format",
    "structured_outputs",
    "reasoning",
    "reasoning_effort",
    "verbosity",
    "web_search_options",
];

/// Reads the list.
pub fn read(list: &Value) -> Result<Catalog, String> {
    let entries = list.get("data").and_then(Value::as_array).ok_or("OpenRouter's list has no `data` array")?;
    let mut vendor_ids = BTreeSet::new();
    let mut models = Vec::new();
    for m in entries {
        let Some(id) = m.get("id").and_then(Value::as_str) else { continue };
        // `…:free`, `…:thinking` and the like are OpenRouter's offers of a model, not models.
        if id.contains(':') {
            continue;
        }
        let Some((author, slug)) = id.split_once('/') else { continue };
        // A router (`openrouter/auto`) prices nothing itself and writes -1.
        let Some(price) = price(m, id) else { continue };
        let vendor = vendors::openrouter_vendor(author);
        vendor_ids.insert(vendor.clone());
        let aliases = m
            .get("canonical_slug")
            .and_then(Value::as_str)
            .and_then(|c| c.split_once('/'))
            .map(|(_, s)| s)
            .filter(|s| *s != slug)
            .map(|s| vec![s.to_owned()])
            .unwrap_or_default();
        let supported: BTreeSet<&str> = m
            .get("supported_parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let defaults = m.get("default_parameters").filter(|d| d.is_object());
        let parameters: BTreeMap<String, Parameter> = PARAMETERS
            .iter()
            .filter(|p| supported.contains(**p))
            .map(|p| {
                let default = defaults.and_then(|d| d.get(*p)).filter(|v| !v.is_null()).cloned();
                ((*p).to_owned(), Parameter { support: Some(Support::Accepted), default, ..Default::default() })
            })
            .collect();
        let properties = properties(m, &supported);
        let kind = properties.modalities.as_ref().and_then(|mo| {
            if mo.output.iter().any(|o| o == "text") {
                Some(ModelKind::Chat)
            } else if mo.output.iter().any(|o| o == "image") {
                Some(ModelKind::Image)
            } else {
                None
            }
        });
        models.push(Model {
            vendor: Some(vendor),
            api_id: slug.to_owned(),
            aliases,
            display_name: m.get("name").and_then(Value::as_str).map(display_name),
            kind,
            sunset_at: m.get("expiration_date").and_then(Value::as_str).and_then(date),
            properties,
            parameters,
            price: (!price.is_empty()).then_some(price),
            ..Default::default()
        });
    }
    let vendors = vendor_ids
        .into_iter()
        .map(|id| Vendor {
            display_name: vendors::info(&id).map(|i| i.display_name.to_owned()),
            currency: Some("USD".to_owned()),
            id,
            ..Default::default()
        })
        .collect();
    Ok(Catalog {
        source: Source { kind: SourceKind::Openrouter, url: Some(URL.to_owned()), version: None },
        vendors,
        models,
        ..Default::default()
    })
}

/// `Anthropic: Claude Haiku 5.5` without the author.
fn display_name(name: &str) -> String {
    name.split_once(": ").map(|(_, n)| n).unwrap_or(name).trim().to_owned()
}

fn date(s: &str) -> Option<DateTime<Utc>> {
    let day = NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()?;
    Some(day.and_hms_opt(0, 0, 0)?.and_utc())
}

/// A price string per token or per unit. `None` for a missing or zero price; `Err` for a
/// negative one, which OpenRouter writes for a router whose price depends on where it routes.
fn amount(pricing: &Value, key: &str) -> Result<Option<f64>, ()> {
    let Some(v) = pricing.get(key) else { return Ok(None) };
    let x = match v {
        Value::String(s) => s.trim().parse::<f64>().map_err(|_| ())?,
        Value::Number(n) => n.as_f64().ok_or(())?,
        _ => return Ok(None),
    };
    if x < 0.0 {
        return Err(());
    }
    Ok((x > 0.0).then_some(x))
}

fn rates_of(pricing: &Value) -> Result<Rates, ()> {
    let per_million = |key: &str| amount(pricing, key).map(|v| v.map(|x| (x * 1e12).round() / 1e6));
    Ok(Rates {
        input: per_million("prompt")?,
        output: per_million("completion")?,
        cache_read: per_million("input_cache_read")?,
        cache_write_5m: per_million("input_cache_write")?,
        cache_write_1h: per_million("input_cache_write_1h")?,
        reasoning: per_million("internal_reasoning")?,
        input_audio: per_million("audio")?,
        output_audio: per_million("audio_output")?,
        per_request: amount(pricing, "request")?,
        per_server_tool_call: amount(pricing, "web_search")?,
        ..Default::default()
    })
}

/// The model's price: its rates, and the rates of long prompts (`overrides`).
fn price(m: &Value, id: &str) -> Option<Price> {
    let pricing = m.get("pricing")?;
    let rates = rates_of(pricing).ok()?;
    let mut tiers = Vec::new();
    for o in pricing.get("overrides").and_then(Value::as_array).into_iter().flatten() {
        let Some(above) = o.get("min_prompt_tokens").and_then(Value::as_u64) else { continue };
        let Ok(rates) = rates_of(o) else { return None };
        if !rates.is_empty() {
            tiers.push(Tier { above_input_tokens: above, rates, variants: BTreeMap::new() });
        }
    }
    tiers.sort_by_key(|t| t.above_input_tokens);
    Some(Price {
        rates,
        tiers,
        evidence: Some(Evidence {
            url: Some(format!("https://openrouter.ai/{id}")),
            method: Some("feed".to_owned()),
            ..Default::default()
        }),
        ..Default::default()
    })
}

fn properties(m: &Value, supported: &BTreeSet<&str>) -> Properties {
    let modality = |key: &str| -> Vec<String> {
        m.get("architecture")
            .and_then(|a| a.get(key))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|x| match x {
                "text" | "image" | "audio" | "video" => Some(x.to_owned()),
                "file" => Some("document".to_owned()),
                _ => None,
            })
            .collect()
    };
    let input = modality("input_modalities");
    let output = modality("output_modalities");
    let has = |p: &str| supported.contains(p);
    let tokens = |v: Option<&Value>| v.and_then(Value::as_u64).filter(|n| *n > 0);
    let top = m.get("top_provider");
    Properties {
        context_window: tokens(m.get("context_length")).or_else(|| tokens(top.and_then(|t| t.get("context_length")))),
        max_output_tokens: tokens(top.and_then(|t| t.get("max_completion_tokens"))),
        vision: (!input.is_empty()).then(|| input.iter().any(|i| i == "image")),
        documents: (!input.is_empty()).then(|| input.iter().any(|i| i == "document")),
        modalities: (!input.is_empty() || !output.is_empty()).then_some(Modalities { input, output }),
        tools: (!supported.is_empty()).then(|| has("tools")),
        structured_output: (!supported.is_empty()).then(|| has("structured_outputs")),
        json_mode: (!supported.is_empty()).then(|| has("response_format")),
        reasoning: (!supported.is_empty()).then(|| has("reasoning") || has("include_reasoning")),
        ..Default::default()
    }
}
