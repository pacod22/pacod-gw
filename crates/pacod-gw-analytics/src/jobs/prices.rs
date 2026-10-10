//! The price tracker's run (concept section 13.3): for each provider, try its sources in
//! order until one yields rates for its models.
//!
//! Two kinds of source. A **feed** is a machine-readable list and is read here. A **page** is
//! a pricing page: its text is sent to the core, which has an LLM extract the rates through
//! the gateway's own data plane. What is found goes back to the core as candidates; whether a
//! candidate is applied, proposed or ignored is the core's decision.

use std::collections::BTreeMap;

use pacod_gw_events::jobs::{CandidateRates, PriceCandidate};
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
}

/// The rates of one model in a feed: token rates per million tokens, the others per unit, and
/// separate rates for the variants the provider bills apart.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct FeedRates {
    #[serde(flatten)]
    pub rates: CandidateRates,
    #[serde(default)]
    pub variants: BTreeMap<String, CandidateRates>,
}

/// The suffix LiteLLM gives the fields of a variant, and the name the provider's usage reports
/// for it (the service tier), which is the name of the price variant.
const LITELLM_VARIANTS: [(&str, &str); 3] = [("_batches", "batch"), ("_priority", "priority"), ("_flex", "flex")];

/// One model of LiteLLM's list. Its rates are per token, under its own names, in US dollars.
fn litellm_rates(v: &Value) -> FeedRates {
    let per_million = |key: &str| v.get(key).and_then(Value::as_f64).map(|x| (x * 1e12).round() / 1e6);
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
    // A web search, priced per query; the medium search context is the default size.
    rates.per_server_tool_call = v
        .get("search_context_cost_per_query")
        .and_then(|q| q.get("search_context_size_medium"))
        .and_then(Value::as_f64);
    let variants = LITELLM_VARIANTS
        .iter()
        .map(|(suffix, name)| ((*name).to_owned(), token_rates(suffix)))
        .filter(|(_, r)| !r.is_empty())
        .collect();
    FeedRates { rates, variants }
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
fn lookup<'a>(models: &'a BTreeMap<String, FeedRates>, m: &TrackedModel) -> Option<&'a FeedRates> {
    let prefixed = m.provider.as_ref().map(|p| format!("{p}/{}", m.upstream_id));
    models.get(&m.upstream_id).or_else(|| prefixed.as_ref().and_then(|k| models.get(k))).or_else(|| models.get(&m.name))
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
                let r = lookup(&feed, m)?;
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
    let mut out = Vec::new();
    for p in found["prices"].as_array().into_iter().flatten() {
        let Some(m) = models.iter().find(|m| p["model"].as_str() == Some(m.upstream_id.as_str())) else { continue };
        let mut rates =
            FeedRates { rates: serde_json::from_value(p.clone()).unwrap_or_default(), ..Default::default() };
        // The extraction names the variants it found as fixed properties.
        for (name, v) in p["variants"].as_object().into_iter().flatten() {
            let r: CandidateRates = serde_json::from_value(v.clone()).unwrap_or_default();
            if !r.is_empty() {
                rates.variants.insert(name.clone(), r);
            }
        }
        let currency = p["currency"].as_str().unwrap_or("USD").to_ascii_uppercase();
        out.push(candidate(
            m,
            &currency,
            &rates,
            "extracted",
            &source.url,
            p["evidence"].as_str().unwrap_or_default().chars().take(2000).collect(),
        ));
    }
    Ok(out)
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
    fn the_gateways_own_feed_format() {
        let body = br#"{"currency":"EUR","models":{"gpt-x":{"input":2.0,"output":10.0,"cache_read":0.2},"openai/gpt-y":{"input":1,"output":4}}}"#;
        let (currency, feed) = parse_feed("pacod", body).unwrap();
        assert_eq!(currency, "EUR");
        assert_eq!(lookup(&feed, &model("gpt-x", "openai/gpt-x", "openai")).unwrap().rates.cache_read, Some(0.2));
        // Found by the id the repository uses, when the feed is written that way.
        assert_eq!(lookup(&feed, &model("gpt-y-2026", "openai/gpt-y", "openai")).unwrap().rates.output, Some(4.0));
        assert!(lookup(&feed, &model("gpt-z", "openai/gpt-z", "openai")).is_none());
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
        let x = &lookup(&feed, &model("claude-x", "anthropic/claude-x", "anthropic")).unwrap().rates;
        assert_eq!(
            (x.input, x.output, x.cache_read, x.cache_write_5m, x.cache_write_1h),
            (Some(3.0), Some(15.0), Some(0.3), Some(3.75), None)
        );
        // Listed under `provider/id`.
        assert_eq!(lookup(&feed, &model("claude-y", "claude-y-repo", "anthropic")).unwrap().rates.input, Some(1.0));
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

    #[test]
    fn a_page_becomes_text_a_model_can_read() {
        let html = "<html><head><style>td { color: red }</style><script>var price = '<b>9</b>';</script></head>\n<body><h1>Pricing</h1>\n<table><tr><td>gpt-x</td><td>&#36;2.00&nbsp;/ 1M input tokens</td><td>$10.00 / 1M output</td></tr></table><p>R&amp;D &lt;beta&gt;</p></body></html>";
        assert_eq!(page_text(html), "Pricing gpt-x $2.00 / 1M input tokens $10.00 / 1M output R&D <beta>");
        assert_eq!(page_text("plain text"), "plain text");
        assert_eq!(page_text("cut <b"), "cut", "an unfinished tag is dropped");
        assert_eq!(page_text("<script>never closed"), "");
    }
}
