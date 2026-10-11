//! Small helpers the dialects share: reading JSON leniently, cache markers, usage objects.

use pacod_gw_events::Usage;
use serde_json::{Map, Value, json};

use crate::canonical::{Block, Cache, ImageSource, Profile};
use crate::error::DialectError;

/// Parses a request body that must be a JSON object.
pub(crate) fn object(body: &[u8]) -> Result<Map<String, Value>, DialectError> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(DialectError::invalid("the request body must be a JSON object")),
        Err(e) => Err(DialectError::invalid(format!("the request body is not valid JSON: {e}"))),
    }
}

pub(crate) fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Takes a number that may have been written as an integer or a float.
pub(crate) fn take_f64(top: &mut Map<String, Value>, key: &str) -> Option<f64> {
    match top.shift_remove(key) {
        Some(Value::Null) | None => None,
        Some(v) => v.as_f64(),
    }
}

pub(crate) fn take_u64(top: &mut Map<String, Value>, key: &str) -> Option<u64> {
    match top.shift_remove(key) {
        Some(Value::Null) | None => None,
        Some(v) => v.as_u64(),
    }
}

pub(crate) fn take_bool(top: &mut Map<String, Value>, key: &str) -> Option<bool> {
    top.shift_remove(key).and_then(|v| v.as_bool())
}

pub(crate) fn take_string(top: &mut Map<String, Value>, key: &str) -> Option<String> {
    match top.shift_remove(key) {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

/// `stop` on the OpenAI dialect is a string or a list; elsewhere a list.
pub(crate) fn strings(v: Option<Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s],
        Some(Value::Array(a)) => a.into_iter().filter_map(|x| x.as_str().map(str::to_owned)).collect(),
        _ => Vec::new(),
    }
}

/// A prompt-cache marker in Anthropic's form (`{"type":"ephemeral","ttl":"1h"}`), which the
/// OpenAI dialect borrows.
pub(crate) fn cache_from(marker: Option<&Value>) -> Option<Cache> {
    let m = marker?.as_object()?;
    Some(Cache { ttl: m.get("ttl").and_then(Value::as_str).map(str::to_owned) })
}

pub(crate) fn cache_to(cache: &Cache) -> Value {
    match &cache.ttl {
        Some(ttl) => json!({ "type": "ephemeral", "ttl": ttl }),
        None => json!({ "type": "ephemeral" }),
    }
}

/// `data:image/png;base64,AAAA` → (`image/png`, `AAAA`).
pub(crate) fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let media = meta.strip_suffix(";base64")?;
    Some((media.to_owned(), data.to_owned()))
}

pub(crate) fn image_from_url(url: &str) -> ImageSource {
    match parse_data_url(url) {
        Some((media_type, data)) => ImageSource::Base64 { media_type, data },
        None => ImageSource::Url { url: url.to_owned() },
    }
}

pub(crate) fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
        ImageSource::Url { url } => url.clone(),
    }
}

/// The text of a list of blocks, joined. Blocks that are not text are skipped.
pub(crate) fn joined_text(blocks: &[Block], separator: &str) -> String {
    let mut out = String::new();
    for b in blocks {
        if let Block::Text { text, .. } = b {
            if !out.is_empty() && !text.is_empty() {
                out.push_str(separator);
            }
            out.push_str(text);
        }
    }
    out
}

/// Tool arguments as a JSON value: the model's text parsed, `{}` when it is empty.
pub(crate) fn arguments_value(arguments: &str) -> Result<Value, serde_json::Error> {
    if arguments.trim().is_empty() { Ok(json!({})) } else { serde_json::from_str(arguments) }
}

/// Tool arguments as JSON text, from an object or from text.
pub(crate) fn arguments_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

// --- usage -------------------------------------------------------------------------------------

/// Usage in OpenAI's shape. `prompt_tokens` includes cached tokens there (concept section 8.4).
pub(crate) fn usage_openai(u: &Usage, cost: Option<&Value>) -> Value {
    let prompt = u.prompt_total();
    let mut out = Map::new();
    out.insert("prompt_tokens".into(), json!(prompt));
    out.insert("completion_tokens".into(), json!(u.output_tokens));
    out.insert("total_tokens".into(), json!(prompt + u.output_tokens));
    out.insert("prompt_tokens_details".into(), json!({ "cached_tokens": u.cache_read_tokens }));
    out.insert("completion_tokens_details".into(), json!({ "reasoning_tokens": u.reasoning_tokens }));
    if u.cache_write_tokens > 0 {
        out.insert("cache_creation_input_tokens".into(), json!(u.cache_write_tokens));
    }
    if let Some(c) = cost {
        out.insert("cost".into(), c.clone());
    }
    Value::Object(out)
}

/// The first of `paths` (dotted, for example `prompt_tokens_details.cached_tokens`) that
/// names a number in `v`.
pub fn first_count(v: &Value, paths: &[String]) -> Option<u64> {
    paths.iter().find_map(|path| path.split('.').try_fold(v, |v, key| v.get(key)).and_then(Value::as_u64))
}

/// The usage object of an OpenAI-shaped answer or stream chunk: the top-level `usage`, or
/// where a provider puts it in the choice (Kimi's stream: `choices[0].usage`).
pub fn openai_usage_value(v: &Value) -> Option<&Value> {
    v.get("usage").filter(|u| u.is_object()).or_else(|| v.pointer("/choices/0/usage").filter(|u| u.is_object()))
}

/// Reads an OpenAI-shaped usage object, with cached, written and reasoning tokens where the
/// provider reports them (dotted paths, tried in order). `prompt_tokens` includes the cached
/// and the written tokens; the neutral input does not (concept section 8.4).
pub fn usage_from_openai(u: &Value, cached_at: &[String], reasoning_at: &[String], written_at: &[String]) -> Usage {
    let n = |key: &str| u.get(key).and_then(Value::as_u64).unwrap_or(0);
    let cached = first_count(u, cached_at).unwrap_or(0);
    let written = first_count(u, written_at).unwrap_or(0);
    Usage {
        input_tokens: n("prompt_tokens").saturating_sub(cached).saturating_sub(written),
        cache_read_tokens: cached,
        cache_write_tokens: written,
        output_tokens: n("completion_tokens"),
        reasoning_tokens: first_count(u, reasoning_at).unwrap_or(0),
        ..Default::default()
    }
}

/// For a provider that leaves its reasoning out of `completion_tokens`: Gemini's compatible
/// endpoint counts it only in `total_tokens`, xAI reports it in
/// `completion_tokens_details.reasoning_tokens` (and in the total). The reasoning is the larger
/// of what is reported and what the total holds beyond prompt and completion, and it is billed
/// as output on top of `completion_tokens`. Called once on usage just read.
pub fn add_hidden_reasoning(usage: &mut Usage, u: &Value) {
    let n = |key: &str| u.get(key).and_then(Value::as_u64).unwrap_or(0);
    let shown = n("prompt_tokens") + n("completion_tokens");
    let hidden = n("total_tokens").saturating_sub(shown);
    let reasoning = usage.reasoning_tokens.max(hidden);
    usage.reasoning_tokens = reasoning;
    usage.output_tokens += reasoning;
}

/// Reads OpenAI-shaped usage where the profile says the provider reports its parts. `None`
/// when the value is not a usage object.
pub(crate) fn read_usage_openai(v: Option<&Value>, profile: &Profile) -> Option<Usage> {
    let v = v.filter(|v| v.is_object())?;
    let mut usage =
        usage_from_openai(v, &profile.cached_tokens_at, &profile.reasoning_tokens_at, &profile.cache_write_tokens_at);
    if profile.reasoning_outside_completion {
        add_hidden_reasoning(&mut usage, v);
    }
    Some(usage)
}

/// Usage in Anthropic's shape. `input_tokens` excludes cached tokens there.
pub(crate) fn usage_anthropic(u: &Usage) -> Value {
    json!({
        "input_tokens": u.input_tokens,
        "cache_creation_input_tokens": u.cache_write_tokens,
        "cache_read_input_tokens": u.cache_read_tokens,
        "output_tokens": u.output_tokens,
    })
}

/// Merges Anthropic-shaped usage into `into`: a field that is present replaces the value
/// (the input side arrives in `message_start`, the output side in `message_delta`).
/// Returns whether there was a usage object.
pub(crate) fn merge_usage_anthropic(v: Option<&Value>, into: &mut Usage) -> bool {
    let Some(u) = v.and_then(Value::as_object) else { return false };
    let n = |key: &str| u.get(key).and_then(Value::as_u64);
    if let Some(x) = n("input_tokens") {
        into.input_tokens = x;
    }
    if let Some(x) = n("output_tokens") {
        into.output_tokens = x;
    }
    if let Some(x) = n("cache_read_input_tokens") {
        into.cache_read_tokens = x;
    }
    if let Some(x) = n("cache_creation_input_tokens") {
        into.cache_write_tokens = x;
    }
    if let Some(c) = u.get("cache_creation").and_then(Value::as_object) {
        into.cache_write_5m_tokens = c.get("ephemeral_5m_input_tokens").and_then(Value::as_u64).unwrap_or(0);
        into.cache_write_1h_tokens = c.get("ephemeral_1h_input_tokens").and_then(Value::as_u64).unwrap_or(0);
    }
    if let Some(s) = u.get("server_tool_use").and_then(Value::as_object) {
        let calls = |key: &str| s.get(key).and_then(Value::as_u64).unwrap_or(0);
        into.server_tool_calls = (calls("web_search_requests") + calls("web_fetch_requests")) as u32;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_urls() {
        assert_eq!(parse_data_url("data:image/png;base64,AAAA"), Some(("image/png".into(), "AAAA".into())));
        assert_eq!(parse_data_url("https://example.test/a.png"), None);
        assert_eq!(parse_data_url("data:text/plain,hello"), None, "only base64 data is taken apart");
        let src = image_from_url("data:image/jpeg;base64,QUJD");
        assert_eq!(image_url(&src), "data:image/jpeg;base64,QUJD");
        assert_eq!(image_url(&image_from_url("https://x.test/i.png")), "https://x.test/i.png");
    }

    #[test]
    fn the_two_usage_conventions() {
        // 812 fresh, 6100 read from the cache, 40 written to it, 420 out.
        let u = Usage {
            input_tokens: 812,
            cache_read_tokens: 6100,
            cache_write_tokens: 40,
            output_tokens: 420,
            reasoning_tokens: 100,
            ..Default::default()
        };
        let o = usage_openai(&u, Some(&json!({ "amount": 0.1 })));
        assert_eq!(o["prompt_tokens"], 6952, "OpenAI's prompt_tokens includes cached tokens");
        assert_eq!(o["prompt_tokens_details"]["cached_tokens"], 6100);
        assert_eq!(o["completion_tokens_details"]["reasoning_tokens"], 100);
        assert_eq!(o["total_tokens"], 7372);
        assert_eq!(o["cost"]["amount"], 0.1);
        let back = read_usage_openai(Some(&o), &Profile::default()).unwrap();
        // Cache writes are not part of OpenAI's vocabulary, so they come back as input.
        assert_eq!((back.input_tokens, back.cache_read_tokens, back.output_tokens), (852, 6100, 420));

        let a = usage_anthropic(&u);
        assert_eq!(a["input_tokens"], 812, "Anthropic's input_tokens excludes them");
        let mut round = Usage::default();
        assert!(merge_usage_anthropic(Some(&a), &mut round));
        assert_eq!((round.input_tokens, round.cache_read_tokens, round.cache_write_tokens), (812, 6100, 40));
        // A later event that names only the output side leaves the input side alone.
        assert!(merge_usage_anthropic(Some(&json!({ "output_tokens": 500 })), &mut round));
        assert_eq!((round.input_tokens, round.output_tokens), (812, 500));
        assert!(!merge_usage_anthropic(Some(&json!(null)), &mut round));
        assert_eq!(read_usage_openai(Some(&json!(null)), &Profile::default()), None);
        // DeepSeek's place by default; a provider's own place where its profile names one.
        let ds = json!({ "prompt_tokens": 10, "prompt_cache_hit_tokens": 4, "completion_tokens": 2 });
        assert_eq!(read_usage_openai(Some(&ds), &Profile::default()).unwrap().cache_read_tokens, 4);
        let own = json!({ "prompt_tokens": 10, "cache": { "hits": 6 }, "completion_tokens": 2, "thinking": 1 });
        let profile = Profile {
            cached_tokens_at: vec!["cache.hits".into()],
            reasoning_tokens_at: vec!["thinking".into()],
            ..Profile::default()
        };
        let u = read_usage_openai(Some(&own), &profile).unwrap();
        assert_eq!((u.input_tokens, u.cache_read_tokens, u.reasoning_tokens), (4, 6, 1));
    }

    #[test]
    fn tool_arguments() {
        assert_eq!(arguments_value("").unwrap(), json!({}));
        assert_eq!(arguments_value("{\"a\":1}").unwrap(), json!({ "a": 1 }));
        assert!(arguments_value("{\"a\":").is_err());
        assert_eq!(arguments_text(Some(&json!({ "a": 1 }))), "{\"a\":1}");
        assert_eq!(arguments_text(Some(&json!("{\"a\": 1}"))), "{\"a\": 1}", "text is kept exactly");
        assert_eq!(arguments_text(None), "");
    }

    #[test]
    fn reasoning_outside_the_completion_is_billed_however_it_is_reported() {
        let read = |u: Value| {
            let mut usage = usage_from_openai(&u, &[], &["completion_tokens_details.reasoning_tokens".into()], &[]);
            add_hidden_reasoning(&mut usage, &u);
            (usage.output_tokens, usage.reasoning_tokens)
        };
        // Gemini's compatible endpoint: only the total holds the reasoning.
        assert_eq!(
            read(serde_json::json!({ "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 45 })),
            (35, 30)
        );
        // xAI: reported apart and in the total.
        assert_eq!(
            read(serde_json::json!({ "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 45,
                                     "completion_tokens_details": { "reasoning_tokens": 30 } })),
            (35, 30)
        );
        // Reported apart, but not in the total.
        assert_eq!(
            read(serde_json::json!({ "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15,
                                     "completion_tokens_details": { "reasoning_tokens": 30 } })),
            (35, 30)
        );
        // No reasoning at all.
        assert_eq!(
            read(serde_json::json!({ "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 })),
            (5, 0)
        );
    }
}
