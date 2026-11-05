//! Machine-readable price lists (feeds), read without a browser or a model: LiteLLM's public
//! list and the gateway's own format. A feed is read into the same shape a page's reading has,
//! for the provider's models only: a feed proposes no models, since it holds every model the
//! provider ever priced.

use std::collections::BTreeMap;

use pacod_gw_events::extraction::{ModelProperties, PageExtraction, PageModel, normalise_model_id};
use pacod_gw_events::jobs::{CandidateRates, CandidateTier, CandidateWindow, PageTask, PriceCandidate};
use serde::Deserialize;
use serde_json::{Value, json};

/// The rates of one model in a feed: token rates per million tokens, the others per unit, the
/// variants the provider bills apart, and what the feed says the model can do.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct FeedRates {
    #[serde(flatten)]
    pub rates: CandidateRates,
    #[serde(default)]
    pub variants: BTreeMap<String, CandidateRates>,
    /// Rates for long prompts.
    #[serde(default)]
    pub tiers: Vec<CandidateTier>,
    /// Hours in which a variant applies by the clock.
    #[serde(default)]
    pub windows: Vec<CandidateWindow>,
    #[serde(default)]
    pub properties: ModelProperties,
}

/// The suffix LiteLLM gives the fields of a variant, and the name the provider's usage reports
/// for it (the service tier), which is the name of the price variant.
const LITELLM_VARIANTS: [(&str, &str); 3] = [("_batches", "batch"), ("_priority", "priority"), ("_flex", "flex")];

/// A rate per token as one per million tokens; zero, LiteLLM's word for "not charged", is none.
fn per_million(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|x| *x > 0.0).map(|x| (x * 1e12).round() / 1e6)
}

/// One model of LiteLLM's list. Its rates are per token, under its own names, in US dollars.
pub fn litellm_rates(v: &Value) -> FeedRates {
    let m = |key: &str| per_million(v.get(key));
    let per_unit = |key: &str| v.get(key).and_then(Value::as_f64);
    let token_rates = |suffix: &str| CandidateRates {
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
    rates.per_image = per_unit("output_cost_per_image").filter(|x| *x > 0.0);
    rates.per_audio_second = per_unit("input_cost_per_second").filter(|x| *x > 0.0);
    rates.per_search_unit = per_unit("input_cost_per_query").filter(|x| *x > 0.0);
    // A web search, priced per query; the medium search context is the default size.
    rates.per_server_tool_call = v
        .get("search_context_cost_per_query")
        .and_then(|q| q.get("search_context_size_medium"))
        .and_then(Value::as_f64);
    let mut variants: BTreeMap<String, CandidateRates> = LITELLM_VARIANTS
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
            CandidateTier {
                above_input_tokens: k * 1000,
                rates: token_rates(&above),
                variants: LITELLM_VARIANTS
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
        let off_rates = CandidateRates {
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
            windows.push(CandidateWindow { variant: "off_peak".into(), weekdays, hours_utc: hours });
        }
        if !off_rates.is_empty() && !windows.is_empty() {
            variants.insert("off_peak".into(), off_rates);
        } else {
            windows.clear();
        }
    }
    FeedRates { rates, variants, tiers, windows, properties: litellm_properties(v) }
}

/// What LiteLLM's list says a model can do.
pub fn litellm_properties(v: &Value) -> ModelProperties {
    let flag = |key: &str| v.get(key).and_then(Value::as_bool);
    let tokens = |key: &str| v.get(key).and_then(Value::as_u64).filter(|n| *n > 0);
    // LiteLLM's modality words, as the gateway writes them.
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
    if flag("supports_pdf_input") == Some(true)
        && let Some(list) = input.as_mut()
        && !list.iter().any(|m| m == "document")
    {
        list.push("document".into());
    }
    ModelProperties {
        context_window: tokens("max_input_tokens"),
        max_output_tokens: tokens("max_output_tokens"),
        input_modalities: input,
        output_modalities: modalities("supported_output_modalities"),
        vision: flag("supports_vision"),
        tools: flag("supports_function_calling"),
        structured_output: flag("supports_response_schema"),
        reasons: flag("supports_reasoning"),
    }
}

/// A feed in the gateway's own format.
#[derive(Debug, Deserialize)]
struct PacodFeed {
    #[serde(default = "usd")]
    currency: String,
    models: BTreeMap<String, FeedRates>,
}

fn usd() -> String {
    "USD".into()
}

/// Reads a feed into rates by model id. Returns the currency with them.
pub fn parse_feed(format: &str, body: &[u8]) -> anyhow::Result<(String, BTreeMap<String, FeedRates>)> {
    if format == "litellm" {
        let raw: BTreeMap<String, Value> = serde_json::from_slice(body)?;
        let models = raw
            .into_iter()
            .map(|(id, v)| (id, litellm_rates(&v)))
            // Entries without any rate this gateway knows are not models it can price.
            .filter(|(_, r)| !r.rates.is_empty())
            .collect();
        return Ok((usd(), models));
    }
    let feed: PacodFeed = serde_json::from_slice(body)?;
    Ok((feed.currency, feed.models))
}

/// The entry of a feed for a provider's model: under the prefix the administrator named, by the
/// provider's id alone, or under the provider's name. The named prefix comes first: an id alone
/// may be another provider's model of the same name.
fn lookup<'a>(
    models: &'a BTreeMap<String, FeedRates>,
    id: &str,
    key_prefix: Option<&str>,
    provider: &str,
) -> Option<&'a FeedRates> {
    // Gemini names models `models/<id>`; the feeds use the id.
    let id = id.trim_start_matches("models/");
    let under = |prefix: &str| models.get(&format!("{prefix}/{id}"));
    key_prefix.and_then(under).or_else(|| models.get(id)).or_else(|| under(provider))
}

/// What a feed says about the provider's models, as a page reading.
pub fn read(task: &PageTask, body: &[u8]) -> anyhow::Result<PageExtraction> {
    let (currency, feed) = parse_feed(&task.format, body)?;
    let mut out = PageExtraction::default();
    for id in &task.known_models {
        let Some(r) = lookup(&feed, id, task.key_prefix.as_deref(), &task.provider) else { continue };
        let named: serde_json::Map<String, Value> =
            r.rates.named().into_iter().filter_map(|(k, v)| Some((k.to_owned(), json!(v?)))).collect();
        let price = (!r.rates.is_empty()).then(|| PriceCandidate {
            model_id: id.clone(),
            currency: currency.clone(),
            rates: r.rates.clone(),
            variants: r.variants.clone(),
            tiers: r.tiers.clone(),
            windows: r.windows.clone(),
            source: "feed".into(),
            evidence_url: task.url.clone(),
            evidence_text: format!("{id}: {}", Value::Object(named)),
        });
        out.models.push(PageModel {
            model: normalise_model_id(id),
            display_name: None,
            // A feed names every model the provider ever priced: none of them is proposed.
            api_id_stated: false,
            price,
            properties: r.properties.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(format: &str, models: &[&str], key_prefix: Option<&str>, provider: &str) -> PageTask {
        PageTask {
            page_id: "ppg_1".into(),
            provider_id: "prv_1".into(),
            provider: provider.into(),
            url: "https://prices.example/list.json".into(),
            kind: "feed".into(),
            format: format.into(),
            key_prefix: key_prefix.map(str::to_owned),
            known_models: models.iter().map(|m| (*m).to_owned()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_gateways_own_feed_format() {
        let body = br#"{"currency":"EUR","models":{"gpt-x":{"input":2.0,"output":10.0,"cache_read":0.2,
            "properties":{"context_window":128000}},"openai/gpt-y":{"input":1,"output":4}}}"#;
        let found = read(&task("pacod", &["gpt-x", "gpt-y", "gpt-z"], None, "openai"), body).unwrap();
        assert_eq!(found.models.len(), 2, "{found:?}");
        let x = &found.models[0];
        let price = x.price.as_ref().unwrap();
        assert_eq!(
            (price.currency.as_str(), price.rates.cache_read, price.source.as_str()),
            ("EUR", Some(0.2), "feed")
        );
        assert_eq!(x.properties.context_window, Some(128_000));
        assert!(!x.api_id_stated, "a feed proposes nothing");
        assert_eq!(found.models[1].price.as_ref().unwrap().rates.output, Some(4.0), "under the provider's name");
        // The currency defaults to US dollars; a feed without models is an error, not empty.
        assert_eq!(parse_feed("", br#"{"models":{}}"#).unwrap().0, "USD");
        assert!(parse_feed("pacod", b"{}").is_err());
        assert!(parse_feed("pacod", b"<html>").is_err());
    }

    /// Entries copied from LiteLLM's published list on 2026-10-10 and 2026-10-11.
    #[test]
    fn the_litellm_list_gives_every_rate_variant_and_property() {
        let (_, feed) = parse_feed("litellm", include_bytes!("testdata/litellm-sample.json")).unwrap();
        let get = |id: &str| feed.get(id).unwrap_or_else(|| panic!("{id} is missing"));
        let gpt = get("gpt-4o");
        assert_eq!((gpt.rates.input, gpt.rates.output, gpt.rates.cache_read), (Some(2.5), Some(10.0), Some(1.25)));
        assert_eq!((gpt.variants["batch"].input, gpt.variants["priority"].cache_read), (Some(1.25), Some(2.125)));
        assert_eq!(get("o3").variants.keys().collect::<Vec<_>>(), ["batch", "flex", "priority"]);
        let claude = get("claude-sonnet-4-5");
        assert_eq!((claude.rates.cache_write_5m, claude.rates.cache_write_1h), (Some(3.75), Some(6.0)));
        assert_eq!(claude.rates.per_server_tool_call, Some(0.01), "a web search");
        let p = &claude.properties;
        assert_eq!((p.context_window, p.max_output_tokens), (Some(200_000), Some(64_000)));
        assert_eq!(
            (p.vision, p.tools, p.structured_output, p.reasons),
            (Some(true), Some(true), Some(true), Some(true))
        );
        let tts = get("tts-1");
        assert_eq!((tts.rates.per_character, tts.rates.input), (Some(1.5e-5), None));
        // Long-context tiers and off-peak hours.
        let pro = get("gemini/gemini-3.1-pro-preview");
        assert_eq!((pro.tiers[0].above_input_tokens, pro.tiers[0].rates.input), (200_000, Some(4.0)));
        let ds = get("deepseek/deepseek-flash");
        assert_eq!(ds.windows[0].hours_utc, ["00:00-01:00", "04:00-06:00", "10:00-00:00"]);
        assert_eq!(ds.rates.cache_write_5m, None, "a zero rate is no rate");
    }

    #[test]
    fn the_named_prefix_comes_before_an_id_alone() {
        let body = include_bytes!("testdata/litellm-sample.json");
        // GLM is listed as `zai/…`, Kimi as `moonshot/…`: the page names the prefix.
        assert!(read(&task("litellm", &["glm-4.7"], None, "glm"), body).unwrap().models.is_empty());
        let glm = read(&task("litellm", &["glm-4.7"], Some("zai"), "glm"), body).unwrap();
        assert_eq!(glm.models[0].price.as_ref().unwrap().rates.input, Some(0.6));
        let pro = read(&task("litellm", &["models/gemini-3.1-pro-preview"], Some("gemini"), "gemini"), body).unwrap();
        assert_eq!(pro.models[0].model, "gemini-3.1-pro-preview");
        // An id that is also listed bare is taken under the prefix the page names.
        let feed = br#"{"x-1": {"input_cost_per_token": 1e-06}, "acme/x-1": {"input_cost_per_token": 2e-06}}"#;
        let found = read(&task("litellm", &["x-1"], Some("acme"), "acme"), feed).unwrap();
        assert_eq!(found.models[0].price.as_ref().unwrap().rates.input, Some(2.0));
    }

    #[test]
    fn audio_image_and_unit_rates_and_modalities_are_read() {
        let r = litellm_rates(&json!({
            "input_cost_per_token": 4e-6, "input_cost_per_audio_token": 3.2e-5, "output_cost_per_image_token": 4e-5,
            "supported_modalities": ["text", "image", "pdf", "smell"], "supported_output_modalities": ["text"],
        }));
        assert_eq!((r.rates.input_audio, r.rates.output_image), (Some(32.0), Some(40.0)));
        assert_eq!(
            r.properties.input_modalities.as_deref(),
            Some(&["text".into(), "image".into(), "document".into()][..])
        );
        assert_eq!(litellm_rates(&json!({ "input_cost_per_second": 0.0001 })).rates.per_audio_second, Some(0.0001));
        assert_eq!(litellm_rates(&json!({ "output_cost_per_image": 0.0 })).rates.per_image, None);
    }
}
