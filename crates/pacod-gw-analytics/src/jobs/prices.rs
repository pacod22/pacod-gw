//! The price tracker's run (concept section 13.3): for each provider, try its sources in
//! order until one yields rates for its models.
//!
//! Two kinds of source. A **feed** is a machine-readable list and is read here. A **page** is
//! a pricing page: its text is sent to the core, which has an LLM extract the rates through
//! the gateway's own data plane. What is found goes back to the core as candidates; whether a
//! candidate is applied, proposed or ignored is the core's decision.

use std::collections::BTreeMap;

use pacod_gw_events::jobs::{CandidateRates, CandidateTier, CandidateWindow, PriceCandidate};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Runner;

/// The largest document a source may be.
const MAX_SOURCE_BYTES: usize = 16 << 20;

/// A model the tracker looks after, as the core describes it.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackedModel {
    pub model_id: String,
    pub name: String,
    pub upstream_id: String,
    pub provider_id: String,
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub provider_id: String,
    /// `feed` or `page`.
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub format: String,
    /// The prefix the feed files this provider's models under, where it is not the gateway's
    /// name for the provider: LiteLLM lists GLM as `zai/…` and Kimi as `moonshot/…`.
    #[serde(default)]
    pub key_prefix: Option<String>,
}

/// The rates of one model in a feed: token rates per million tokens, the others per unit, and
/// separate rates for the variants the provider bills apart.
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
}

/// The suffix LiteLLM gives the fields of a variant, and the name the provider's usage reports
/// for it (the service tier), which is the name of the price variant.
const LITELLM_VARIANTS: [(&str, &str); 3] = [("_batches", "batch"), ("_priority", "priority"), ("_flex", "flex")];

/// One model of LiteLLM's list. Its rates are per token, under its own names, in US dollars.
fn litellm_rates(v: &Value) -> FeedRates {
    // A rate of zero is LiteLLM's way of saying the thing is not charged: left out here.
    let per_million =
        |key: &str| v.get(key).and_then(Value::as_f64).filter(|x| *x > 0.0).map(|x| (x * 1e12).round() / 1e6);
    let per_unit = |key: &str| v.get(key).and_then(Value::as_f64);
    let token_rates = |suffix: &str| CandidateRates {
        input: per_million(&format!("input_cost_per_token{suffix}")),
        output: per_million(&format!("output_cost_per_token{suffix}")),
        cache_read: per_million(&format!("cache_read_input_token_cost{suffix}")),
        cache_write_5m: per_million(&format!("cache_creation_input_token_cost{suffix}")),
        cache_write_1h: per_million(&format!("cache_creation_input_token_cost_above_1hr{suffix}")),
        ..Default::default()
    };
    let mut rates = token_rates("");
    rates.reasoning = per_million("output_cost_per_reasoning_token");
    rates.per_character = per_unit("input_cost_per_character");
    rates.per_request = per_unit("input_cost_per_request");
    // Audio and image tokens where they are priced apart (realtime, transcription, image
    // models), and the unit prices of images, audio and rerank queries.
    rates.input_audio = per_million("input_cost_per_audio_token");
    rates.output_audio = per_million("output_cost_per_audio_token");
    rates.input_image = per_million("input_cost_per_image_token");
    rates.output_image = per_million("output_cost_per_image_token");
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
            input: off
                .get("input_cost_per_token")
                .and_then(Value::as_f64)
                .filter(|x| *x > 0.0)
                .map(|x| (x * 1e12).round() / 1e6),
            output: off
                .get("output_cost_per_token")
                .and_then(Value::as_f64)
                .filter(|x| *x > 0.0)
                .map(|x| (x * 1e12).round() / 1e6),
            cache_read: off
                .get("cache_read_input_token_cost")
                .and_then(Value::as_f64)
                .filter(|x| *x > 0.0)
                .map(|x| (x * 1e12).round() / 1e6),
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
    FeedRates { rates, variants, tiers, windows }
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
        // LiteLLM's list: rates per token, under its own field names, in US dollars.
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

/// The entry of a feed for a model: by the provider's id, by `provider/id`, or by the id the
/// repository gives the model.
fn lookup<'a>(
    models: &'a BTreeMap<String, FeedRates>,
    m: &TrackedModel,
    key_prefix: Option<&str>,
) -> Option<&'a FeedRates> {
    // Gemini names models `models/<id>`; the feeds use the id.
    let id = m.upstream_id.trim_start_matches("models/");
    let under = |prefix: &str| models.get(&format!("{prefix}/{id}"));
    models
        .get(id)
        .or_else(|| key_prefix.and_then(under))
        .or_else(|| m.provider.as_deref().and_then(under))
        .or_else(|| models.get(&m.name))
}

/// The text of an HTML page: tags, scripts and styles removed, white space collapsed. Good
/// enough for a model to read prices from; this is not an HTML parser.
pub fn page_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 4);
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let tail = &rest[open..];
        let lower = tail.get(..8).unwrap_or(tail).to_ascii_lowercase();
        // The content of scripts and styles is not text of the page.
        let skip_to = if lower.starts_with("<script") {
            Some("</script>")
        } else if lower.starts_with("<style") {
            Some("</style>")
        } else {
            None
        };
        let end = match skip_to {
            Some(close) => tail.to_ascii_lowercase().find(close).map(|i| i + close.len()),
            None => tail.find('>').map(|i| i + 1),
        };
        match end {
            Some(e) => {
                out.push(' ');
                rest = &tail[e..];
            }
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#36;", "$");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn candidate(
    m: &TrackedModel,
    currency: &str,
    r: &FeedRates,
    source: &str,
    url: &str,
    evidence: String,
) -> PriceCandidate {
    PriceCandidate {
        model_id: m.model_id.clone(),
        currency: currency.to_owned(),
        rates: r.rates.clone(),
        variants: r.variants.clone(),
        tiers: r.tiers.clone(),
        windows: r.windows.clone(),
        source: source.to_owned(),
        evidence_url: url.to_owned(),
        evidence_text: evidence,
    }
}

/// What one source yields for a provider's models.
async fn try_source(
    runner: &Runner,
    run_id: &str,
    source: &Source,
    models: &[&TrackedModel],
) -> anyhow::Result<Vec<PriceCandidate>> {
    let body = runner.fetch(&source.url, MAX_SOURCE_BYTES).await?;
    if source.kind == "feed" {
        let (currency, feed) = parse_feed(&source.format, &body)?;
        return Ok(models
            .iter()
            .filter_map(|m| {
                let r = lookup(&feed, m, source.key_prefix.as_deref())?;
                let named: serde_json::Map<String, Value> =
                    r.rates.named().into_iter().filter_map(|(k, v)| Some((k.to_owned(), json!(v?)))).collect();
                let evidence = format!("{}: {}", m.upstream_id, Value::Object(named));
                Some(candidate(m, &currency, r, "feed", &source.url, evidence))
            })
            .collect());
    }
    // A page: the core has an LLM read it. This side never sees a provider key.
    let text = page_text(&String::from_utf8_lossy(&body));
    let ids: Vec<&str> = models.iter().map(|m| m.upstream_id.as_str()).collect();
    let found: Value =
        runner.post(&format!("/{run_id}/extract"), &json!({ "url": source.url, "text": text, "models": ids })).await?;
    Ok(candidates_from_extraction(&found, models, &source.url))
}

/// The candidates in what the extraction model found on a page: rates, variants, long-context
/// tiers, and off-peak rates with their hours converted to UTC windows. A part that cannot be
/// read is left out and said so in the evidence; the rest of the price stays.
pub fn candidates_from_extraction(found: &Value, models: &[&TrackedModel], url: &str) -> Vec<PriceCandidate> {
    use pacod_gw_events::hours::{StatedHours, canonical_tiers, off_peak_windows};
    let mut out = Vec::new();
    for p in found["prices"].as_array().into_iter().flatten() {
        // Pages write model names their own way (`GLM-4.7` for `glm-4.7`, seen live).
        let named = p["model"].as_str().unwrap_or_default();
        let Some(m) = models.iter().find(|m| m.upstream_id.eq_ignore_ascii_case(named.trim())) else { continue };
        let mut rates =
            FeedRates { rates: serde_json::from_value(p.clone()).unwrap_or_default(), ..Default::default() };
        // The extraction names the variants it found as fixed properties.
        for (name, v) in p["variants"].as_object().into_iter().flatten() {
            let r: CandidateRates = serde_json::from_value(v.clone()).unwrap_or_default();
            if !r.is_empty() {
                rates.variants.insert(name.clone(), r);
            }
        }
        let mut notes = Vec::new();
        let tiers: Vec<CandidateTier> = p["tiers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                Some(CandidateTier {
                    above_input_tokens: t["above_input_tokens"].as_u64()?,
                    rates: serde_json::from_value(t.clone()).ok()?,
                    variants: BTreeMap::new(),
                })
            })
            .collect();
        rates.tiers = canonical_tiers(&tiers);
        if rates.tiers.len() < p["tiers"].as_array().map_or(0, Vec::len) {
            notes.push("a long-context tier without a threshold or rates was left out");
        }
        if let Some(off) = p["off_peak"].as_object() {
            let off_rates: CandidateRates = serde_json::from_value(Value::Object(off.clone())).unwrap_or_default();
            let hours = |key: &str| off.get(key).and_then(|v| serde_json::from_value::<StatedHours>(v.clone()).ok());
            match off_peak_windows(hours("peak").as_ref(), hours("off_peak").as_ref()) {
                Some(windows) if !off_rates.is_empty() => {
                    rates.variants.insert("off_peak".into(), off_rates);
                    rates.windows = windows;
                }
                Some(_) => notes.push("off-peak hours without off-peak rates were left out"),
                None if !off_rates.is_empty() => notes.push("off-peak rates without readable hours were left out"),
                None => {}
            }
        }
        let currency = p["currency"].as_str().unwrap_or("USD").to_ascii_uppercase();
        let mut evidence: String = p["evidence"].as_str().unwrap_or_default().chars().take(2000).collect();
        for note in notes {
            evidence.push_str(&format!(" [{note}]"));
        }
        out.push(candidate(m, &currency, &rates, "extracted", url, evidence));
    }
    out
}

/// Runs the price job on the work the core handed out. Returns the candidates and, per
/// source, what came of it.
pub async fn run(runner: &Runner, run_id: &str, work: &Value) -> anyhow::Result<Value> {
    let models: Vec<TrackedModel> = serde_json::from_value(work["models"].clone())?;
    let sources: Vec<Source> = serde_json::from_value(work["config"]["sources"].clone()).unwrap_or_default();
    let mut candidates: Vec<PriceCandidate> = Vec::new();
    let mut report = Vec::new();

    let mut providers: Vec<&str> = models.iter().map(|m| m.provider_id.as_str()).collect();
    providers.sort_unstable();
    providers.dedup();
    for provider in providers {
        let of_provider: Vec<&TrackedModel> = models.iter().filter(|m| m.provider_id == provider).collect();
        // The sources of a provider are tried in the order they are configured, until one
        // has a price for at least one of its models.
        for source in sources.iter().filter(|s| s.provider_id == provider) {
            match try_source(runner, run_id, source, &of_provider).await {
                Ok(found) => {
                    report.push(json!({ "provider_id": provider, "url": source.url, "kind": source.kind, "ok": true, "found": found.len() }));
                    if !found.is_empty() {
                        candidates.extend(found);
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(url = %source.url, error = %e, "price source failed");
                    report.push(json!({ "provider_id": provider, "url": source.url, "kind": source.kind, "ok": false, "message": format!("{e:#}") }));
                }
            }
        }
    }
    Ok(json!({ "candidates": candidates, "sources": report }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(upstream: &str, name: &str, provider: &str) -> TrackedModel {
        TrackedModel {
            model_id: format!("mdl_{upstream}"),
            name: name.into(),
            upstream_id: upstream.into(),
            provider_id: "prv_1".into(),
            provider: Some(provider.into()),
        }
    }

    #[test]
    fn an_extraction_gives_tiers_and_off_peak_windows_in_utc() {
        let flash = model("deepseek-flash", "deepseek/flash", "deepseek");
        let glm = model("glm-4.7", "glm/4.7", "glm-cn");
        // What a model reads off DeepSeek's page when it states the peak hours in Beijing time,
        // and off bigmodel.cn's input-length tiers.
        let found = json!({ "prices": [
            { "model": "deepseek-flash", "currency": "usd", "input": 0.3, "output": 1.2, "evidence": "Off-peak 50% off",
              "off_peak": { "input": 0.15, "output": 0.6,
                            "peak": { "weekdays": [1, 2, 3, 4, 5], "hours": ["09:00-12:00", "14:00-18:00"], "utc_offset": "+08:00" } } },
            { "model": "glm-4.7", "currency": "CNY", "input": 2.0, "output": 8.0, "evidence": "0-32K, 32K-200K",
              "tiers": [{ "above_input_tokens": 32000, "input": 4.0, "output": 16.0 }, { "input": 9.0 }] }
        ] });
        let c = candidates_from_extraction(&found, &[&flash, &glm], "https://x");
        assert_eq!(c.len(), 2);
        // The page's spelling of a model's name does not matter.
        let upper = json!({ "prices": [{ "model": "GLM-4.7", "currency": "CNY", "input": 2.0, "evidence": "e" }] });
        assert_eq!(candidates_from_extraction(&upper, &[&glm], "https://x").len(), 1);
        let ds = &c[0];
        assert_eq!(ds.currency, "USD");
        assert_eq!((ds.variants["off_peak"].input, ds.variants["off_peak"].output), (Some(0.15), Some(0.6)));
        let hours: Vec<(Vec<u8>, Vec<String>)> =
            ds.windows.iter().map(|w| (w.weekdays.clone(), w.hours_utc.clone())).collect();
        assert_eq!(
            hours,
            [
                (vec![1, 2, 3, 4, 5], vec!["00:00-01:00".to_owned(), "04:00-06:00".into(), "10:00-00:00".into()]),
                (vec![6, 7], vec!["00:00-00:00".to_owned()])
            ],
            "the same windows as LiteLLM's feed"
        );
        let tiers: Vec<(u64, Option<f64>)> = c[1].tiers.iter().map(|t| (t.above_input_tokens, t.rates.input)).collect();
        assert_eq!(tiers, [(32_000, Some(4.0))]);
        assert!(
            c[1].evidence_text.contains("left out"),
            "the tier without a threshold is noted: {}",
            c[1].evidence_text
        );
    }

    #[test]
    fn unreadable_hours_leave_the_off_peak_rates_out_not_the_price() {
        let flash = model("deepseek-flash", "deepseek/flash", "deepseek");
        let found = json!({ "prices": [{ "model": "deepseek-flash", "currency": "USD", "input": 0.3, "evidence": "e",
            "off_peak": { "input": 0.15, "peak": { "weekdays": [1], "hours": ["evenings"], "utc_offset": "Beijing" } } }] });
        let c = candidates_from_extraction(&found, &[&flash], "https://x");
        assert_eq!(c[0].rates.input, Some(0.3));
        assert!(c[0].windows.is_empty() && !c[0].variants.contains_key("off_peak"));
        assert!(c[0].evidence_text.contains("without readable hours"), "{}", c[0].evidence_text);
    }

    #[test]
    fn the_gateways_own_feed_format() {
        let body = br#"{"currency":"EUR","models":{"gpt-x":{"input":2.0,"output":10.0,"cache_read":0.2},"openai/gpt-y":{"input":1,"output":4}}}"#;
        let (currency, feed) = parse_feed("pacod", body).unwrap();
        assert_eq!(currency, "EUR");
        assert_eq!(lookup(&feed, &model("gpt-x", "openai/gpt-x", "openai"), None).unwrap().rates.cache_read, Some(0.2));
        // Found by the id the repository uses, when the feed is written that way.
        assert_eq!(
            lookup(&feed, &model("gpt-y-2026", "openai/gpt-y", "openai"), None).unwrap().rates.output,
            Some(4.0)
        );
        assert!(lookup(&feed, &model("gpt-z", "openai/gpt-z", "openai"), None).is_none());
        // The currency defaults to US dollars; a feed without models is an error, not empty.
        assert_eq!(parse_feed("", br#"{"models":{}}"#).unwrap().0, "USD");
        assert!(parse_feed("pacod", b"{}").is_err());
        assert!(parse_feed("pacod", b"<html>").is_err());
    }

    #[test]
    fn the_litellm_list_is_per_token() {
        let body = br#"{
            "sample_spec": {"max_tokens": "LEGACY"},
            "claude-x": {"input_cost_per_token": 3e-06, "output_cost_per_token": 1.5e-05, "cache_read_input_token_cost": 3e-07, "cache_creation_input_token_cost": 3.75e-06},
            "anthropic/claude-y": {"input_cost_per_token": 1e-06, "output_cost_per_token": 5e-06}
        }"#;
        let (currency, feed) = parse_feed("litellm", body).unwrap();
        assert_eq!((currency.as_str(), feed.len()), ("USD", 2), "entries without a price are not models");
        let x = &lookup(&feed, &model("claude-x", "anthropic/claude-x", "anthropic"), None).unwrap().rates;
        assert_eq!(
            (x.input, x.output, x.cache_read, x.cache_write_5m, x.cache_write_1h),
            (Some(3.0), Some(15.0), Some(0.3), Some(3.75), None)
        );
        // Listed under `provider/id`.
        assert_eq!(
            lookup(&feed, &model("claude-y", "claude-y-repo", "anthropic"), None).unwrap().rates.input,
            Some(1.0)
        );
    }

    /// Four entries copied from LiteLLM's published list on 2026-10-10.
    #[test]
    fn the_litellm_list_gives_every_rate_and_variant() {
        let (_, feed) = parse_feed("litellm", include_bytes!("testdata/litellm-sample.json")).unwrap();
        let get = |id: &str| feed.get(id).unwrap_or_else(|| panic!("{id} is missing"));
        let gpt = get("gpt-4o");
        assert_eq!((gpt.rates.input, gpt.rates.output, gpt.rates.cache_read), (Some(2.5), Some(10.0), Some(1.25)));
        let batch = &gpt.variants["batch"];
        assert_eq!((batch.input, batch.output), (Some(1.25), Some(5.0)));
        let priority = &gpt.variants["priority"];
        assert_eq!((priority.input, priority.output, priority.cache_read), (Some(4.25), Some(17.0), Some(2.125)));
        let o3 = get("o3");
        assert_eq!(o3.variants.keys().collect::<Vec<_>>(), ["batch", "flex", "priority"]);
        assert_eq!(o3.variants["flex"].input, Some(1.0));
        let claude = get("claude-sonnet-4-5");
        assert_eq!((claude.rates.cache_write_5m, claude.rates.cache_write_1h), (Some(3.75), Some(6.0)));
        assert_eq!(claude.rates.per_server_tool_call, Some(0.01), "a web search");
        assert_eq!(claude.variants["batch"].cache_read, Some(0.15));
        // Speech is priced per character, which LiteLLM lists as it is.
        let tts = get("tts-1");
        assert_eq!((tts.rates.per_character, tts.rates.input), (Some(1.5e-5), None));
        assert!(tts.variants.is_empty());
    }

    /// LiteLLM's keys for audio and image tokens and for unit prices. The figures are
    /// illustrative; the keys are the ones its list uses.
    #[test]
    fn audio_image_and_unit_rates_are_read() {
        let rates = litellm_rates(&serde_json::json!({
            "input_cost_per_token": 4e-6,
            "output_cost_per_token": 1.6e-5,
            "input_cost_per_audio_token": 3.2e-5,
            "output_cost_per_audio_token": 6.4e-5,
            "input_cost_per_image_token": 1e-5,
            "output_cost_per_image_token": 4e-5,
        }))
        .rates;
        assert_eq!((rates.input_audio, rates.output_audio), (Some(32.0), Some(64.0)));
        assert_eq!((rates.input_image, rates.output_image), (Some(10.0), Some(40.0)));
        let whisper =
            litellm_rates(&serde_json::json!({ "input_cost_per_second": 0.0001, "output_cost_per_second": 0.0 }));
        assert_eq!(whisper.rates.per_audio_second, Some(0.0001));
        let dalle = litellm_rates(&serde_json::json!({ "output_cost_per_image": 0.04 }));
        assert_eq!(dalle.rates.per_image, Some(0.04));
        let rerank = litellm_rates(&serde_json::json!({ "input_cost_per_query": 0.002 }));
        assert_eq!(rerank.rates.per_search_unit, Some(0.002));
        // Zero means not charged, as for every other rate.
        let free = litellm_rates(&serde_json::json!({ "output_cost_per_image": 0.0 }));
        assert_eq!(free.rates.per_image, None);
    }

    /// Entries copied from LiteLLM's published list on 2026-10-11.
    #[test]
    fn long_context_off_peak_and_other_prefixes_are_read() {
        let (_, feed) = parse_feed("litellm", include_bytes!("testdata/litellm-sample.json")).unwrap();
        let pro = lookup(&feed, &model("models/gemini-3.1-pro-preview", "gemini/pro", "gemini"), None).unwrap();
        assert_eq!((pro.rates.input, pro.rates.output), (Some(2.0), Some(12.0)));
        let above = &pro.tiers[0];
        assert_eq!(above.above_input_tokens, 200_000);
        assert_eq!((above.rates.input, above.rates.output, above.rates.cache_read), (Some(4.0), Some(18.0), Some(0.4)));
        assert_eq!(above.variants["priority"].input, Some(7.2));
        assert!(!above.variants.contains_key("flex"), "flex has no long-context rates there");
        let ds = lookup(&feed, &model("deepseek-flash", "deepseek/flash", "deepseek"), None).unwrap();
        assert_eq!((ds.variants["off_peak"].input, ds.variants["off_peak"].output), (Some(0.15), Some(0.6)));
        assert_eq!(ds.windows.len(), 2);
        assert_eq!(ds.windows[0].weekdays, [1, 2, 3, 4, 5]);
        assert_eq!(ds.windows[0].hours_utc, ["00:00-01:00", "04:00-06:00", "10:00-00:00"]);
        assert_eq!(ds.windows[1].hours_utc, ["00:00-00:00"]);
        assert_eq!(ds.rates.cache_write_5m, None, "a zero rate is no rate");
        // GLM is listed as `zai/…`, Kimi as `moonshot/…`: the source names the prefix.
        assert!(lookup(&feed, &model("glm-4.7", "glm/4.7", "glm"), None).is_none());
        let glm = lookup(&feed, &model("glm-4.7", "glm/4.7", "glm"), Some("zai")).unwrap();
        assert_eq!((glm.rates.input, glm.rates.cache_read), (Some(0.6), Some(0.11)));
        assert_eq!(
            lookup(&feed, &model("kimi-k3", "kimi/k3", "kimi"), Some("moonshot")).unwrap().rates.output,
            Some(15.0)
        );
    }

    #[test]
    fn a_page_becomes_text_a_model_can_read() {
        let html = "<html><head><style>td { color: red }</style><script>var price = '<b>9</b>';</script></head>\n<body><h1>Pricing</h1>\n<table><tr><td>gpt-x</td><td>&#36;2.00&nbsp;/ 1M input tokens</td><td>$10.00 / 1M output</td></tr></table><p>R&amp;D &lt;beta&gt;</p></body></html>";
        assert_eq!(page_text(html), "Pricing gpt-x $2.00 / 1M input tokens $10.00 / 1M output R&D <beta>");
        assert_eq!(page_text("plain text"), "plain text");
        assert_eq!(page_text("cut <b"), "cut", "an unfinished tag is dropped");
        assert_eq!(page_text("<script>never closed"), "");
    }
}
