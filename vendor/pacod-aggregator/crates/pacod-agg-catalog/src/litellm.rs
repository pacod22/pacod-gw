//! LiteLLM's list of models (`model_prices_and_context_window.json`): an object of entries by
//! key, rates per token in US dollars under LiteLLM's names. Only the vendors' own APIs are
//! read ([`crate::vendors::litellm_vendor`]); the many resellers and clouds the list also prices
//! are left out, and an entry listed twice (`deepseek-chat`, `deepseek/deepseek-chat`) is read
//! once.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

use crate::ids::normalise_model_id;
use crate::v1::{
    Catalog, Modalities, Model, ModelKind, Parameter, Price, Properties, Rates, Source, SourceKind, Support, Tier,
    Vendor, Window,
};
use crate::vendors;

/// Where LiteLLM keeps the list.
pub const URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// The suffix LiteLLM gives the fields of a variant, and the name the vendor's usage reports for
/// it (the service tier), which is the name of the price variant.
const VARIANTS: [(&str, &str); 3] = [("_batches", "batch"), ("_priority", "priority"), ("_flex", "flex")];

/// Reads the list.
pub fn read(list: &Value) -> Result<Catalog, String> {
    let entries = list.as_object().ok_or("LiteLLM's list is an object of models by key")?;
    let mut seen = HashSet::new();
    let mut vendor_ids = BTreeSet::new();
    let mut models = Vec::new();
    for (key, v) in entries {
        let Some(provider) = v.get("litellm_provider").and_then(Value::as_str) else { continue };
        let Some(vendor) = vendors::litellm_vendor(provider) else { continue };
        let Some(kind) = kind(v.get("mode").and_then(Value::as_str)) else { continue };
        let api_id = api_id(key, provider, vendor);
        if !seen.insert((vendor, normalise_model_id(api_id))) {
            continue;
        }
        let price = price(v);
        let properties = properties(v);
        if price.is_none() && properties.is_empty() {
            continue;
        }
        vendor_ids.insert(vendor);
        let mut parameters = BTreeMap::new();
        if v.get("supports_tool_choice").and_then(Value::as_bool) == Some(true) {
            parameters
                .insert("tool_choice".to_owned(), Parameter { support: Some(Support::Accepted), ..Default::default() });
        }
        models.push(Model {
            vendor: Some(vendor.to_owned()),
            api_id: api_id.to_owned(),
            kind: Some(kind),
            sunset_at: v.get("deprecation_date").and_then(Value::as_str).and_then(date),
            successor: v.get("successor_model").and_then(Value::as_str).map(str::to_owned),
            properties,
            parameters,
            price,
            ..Default::default()
        });
    }
    let vendors = vendor_ids
        .into_iter()
        .map(|id| Vendor {
            id: id.to_owned(),
            display_name: vendors::info(id).map(|i| i.display_name.to_owned()),
            currency: Some("USD".to_owned()),
            ..Default::default()
        })
        .collect();
    Ok(Catalog {
        source: Source { kind: SourceKind::Litellm, url: Some(URL.to_owned()), version: None },
        vendors,
        models,
        ..Default::default()
    })
}

/// The id at the vendor's API: the key without the provider's prefix (`gemini/gemini-2.5-pro`,
/// `groq/openai/gpt-oss-120b`). OpenAI's and Anthropic's keys have none.
fn api_id<'a>(key: &'a str, provider: &str, vendor: &str) -> &'a str {
    match key.split_once('/') {
        Some((first, rest)) if first == provider || first == vendor || provider.starts_with(first) => rest,
        _ => key,
    }
}

fn kind(mode: Option<&str>) -> Option<ModelKind> {
    Some(match mode? {
        "chat" | "responses" | "completion" => ModelKind::Chat,
        "embedding" => ModelKind::Embedding,
        "image_generation" => ModelKind::Image,
        "audio_speech" => ModelKind::Speech,
        "audio_transcription" => ModelKind::Transcription,
        "rerank" => ModelKind::Rerank,
        "realtime" => ModelKind::Realtime,
        // Video, OCR, search, moderation and the rest are not models a gateway serves as such.
        _ => return None,
    })
}

/// A date of the list (`2026-11-30`) as the start of that day in UTC.
fn date(s: &str) -> Option<DateTime<Utc>> {
    let day = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()?;
    Some(day.and_hms_opt(0, 0, 0)?.and_utc())
}

/// A rate per token as one per million tokens; zero, LiteLLM's word for "not charged", is none.
fn per_million(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|x| *x > 0.0).map(|x| (x * 1e12).round() / 1e6)
}

/// The price of one entry: rates, variants, long-context tiers and off-peak hours.
fn price(v: &Value) -> Option<Price> {
    let m = |key: &str| per_million(v.get(key));
    let per_unit = |key: &str| v.get(key).and_then(Value::as_f64).filter(|x| *x > 0.0);
    let token_rates = |suffix: &str| Rates {
        input: m(&format!("input_cost_per_token{suffix}")),
        output: m(&format!("output_cost_per_token{suffix}")),
        cache_read: m(&format!("cache_read_input_token_cost{suffix}")),
        cache_write_5m: m(&format!("cache_creation_input_token_cost{suffix}")),
        cache_write_1h: m(&format!("cache_creation_input_token_cost_above_1hr{suffix}")),
        ..Default::default()
    };
    let mut rates = token_rates("");
    rates.reasoning = m("output_cost_per_reasoning_token");
    rates.per_character = per_unit("input_cost_per_character");
    rates.per_request = per_unit("input_cost_per_request");
    // Audio and image tokens where they are priced apart (realtime, transcription, image
    // models), and the unit prices of images, audio and rerank queries.
    rates.input_audio = m("input_cost_per_audio_token");
    rates.output_audio = m("output_cost_per_audio_token");
    rates.input_image = m("input_cost_per_image_token");
    rates.output_image = m("output_cost_per_image_token");
    rates.per_image = per_unit("output_cost_per_image");
    rates.per_audio_second = per_unit("input_cost_per_second");
    rates.per_search_unit = per_unit("input_cost_per_query");
    // A web search, priced per query; the medium search context is the default size.
    rates.per_server_tool_call = v
        .get("search_context_cost_per_query")
        .and_then(|q| q.get("search_context_size_medium"))
        .and_then(Value::as_f64)
        .filter(|x| *x > 0.0);
    let mut variants: BTreeMap<String, Rates> = VARIANTS
        .iter()
        .map(|(suffix, name)| ((*name).to_owned(), token_rates(suffix)))
        .filter(|(_, r)| !r.is_empty())
        .collect();
    // Long-context rates: `input_cost_per_token_above_200k_tokens`, and per variant with its
    // suffix after that (`…_above_200k_tokens_priority`).
    let mut thresholds: Vec<u64> = v
        .as_object()
        .into_iter()
        .flat_map(|o| o.keys())
        .filter_map(|k| k.strip_prefix("input_cost_per_token_above_"))
        .filter_map(|rest| rest.split_once("k_tokens").and_then(|(n, _)| n.parse::<u64>().ok()))
        .collect();
    thresholds.sort_unstable();
    thresholds.dedup();
    let tiers = thresholds
        .into_iter()
        .map(|k| {
            let above = format!("_above_{k}k_tokens");
            Tier {
                above_input_tokens: k * 1000,
                rates: token_rates(&above),
                variants: VARIANTS
                    .iter()
                    .map(|(suffix, name)| ((*name).to_owned(), token_rates(&format!("{above}{suffix}"))))
                    .filter(|(_, r)| !r.is_empty())
                    .collect(),
            }
        })
        .filter(|t| !t.rates.is_empty() || !t.variants.is_empty())
        .collect();
    // Off-peak prices with the hours they apply in (DeepSeek).
    let mut windows = Vec::new();
    if let Some(off) = v.get("off_peak_pricing").filter(|o| o.is_object()) {
        let off_rates = Rates {
            input: per_million(off.get("input_cost_per_token")),
            output: per_million(off.get("output_cost_per_token")),
            cache_read: per_million(off.get("cache_read_input_token_cost")),
            ..Default::default()
        };
        for w in off["windows"].as_array().into_iter().flatten() {
            let hours = match &w["hours_utc"] {
                Value::String(h) => vec![h.clone()],
                Value::Array(list) => list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
                _ => Vec::new(),
            };
            let weekdays =
                w["weekdays"].as_array().into_iter().flatten().filter_map(Value::as_u64).map(|d| d as u8).collect();
            windows.push(Window { variant: "off_peak".into(), weekdays, hours_utc: hours });
        }
        if !off_rates.is_empty() && !windows.is_empty() {
            variants.insert("off_peak".into(), off_rates);
        } else {
            windows.clear();
        }
    }
    let price = Price { rates, variants, tiers, windows, ..Default::default() };
    // Entries without any rate are not priced here.
    (!price.rates.is_empty()).then_some(price)
}

/// What the list says a model can do.
fn properties(v: &Value) -> Properties {
    let flag = |key: &str| v.get(key).and_then(Value::as_bool);
    let tokens = |key: &str| v.get(key).and_then(Value::as_u64).filter(|n| *n > 0);
    // LiteLLM's modality words, as the catalogue writes them.
    let modalities = |key: &str| -> Option<Vec<String>> {
        let list: Vec<String> = v
            .get(key)?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|m| match m {
                "text" | "image" | "audio" | "video" => Some(m.to_owned()),
                "pdf" | "document" => Some("document".to_owned()),
                _ => None,
            })
            .collect();
        (!list.is_empty()).then_some(list)
    };
    let mut input = modalities("supported_modalities");
    let documents = flag("supports_pdf_input");
    if documents == Some(true)
        && let Some(list) = input.as_mut()
        && !list.iter().any(|m| m == "document")
    {
        list.push("document".into());
    }
    let output = modalities("supported_output_modalities");
    let modalities = (input.is_some() || output.is_some())
        .then(|| Modalities { input: input.unwrap_or_default(), output: output.unwrap_or_default() });
    Properties {
        context_window: tokens("max_input_tokens"),
        max_output_tokens: tokens("max_output_tokens"),
        modalities,
        vision: flag("supports_vision"),
        documents,
        tools: flag("supports_function_calling"),
        structured_output: flag("supports_response_schema"),
        reasoning: flag("supports_reasoning"),
        prompt_cache: flag("supports_prompt_caching"),
        server_tools: (flag("supports_web_search") == Some(true)).then(|| vec!["web_search".to_owned()]),
        ..Default::default()
    }
}
