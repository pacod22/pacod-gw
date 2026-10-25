//! What makes two calls the same call, and which calls may be answered again.
//!
//! The key is taken over the request the provider is sent, after the gateway applied the
//! template and shaped it for the model. Two calls with the same key would reach the same model
//! with the same request. SHA-256, not a per-process hash: the store is shared by every
//! instance.
//!
//! The body is read once and only at its top level: each member's value stays the slice of the
//! body it was, so a long conversation costs one pass over its bytes, not a tree.

use std::collections::BTreeMap;

use pacod_gw_plugin::{Dialect, PlannedCall};
use ring::digest::{Context, SHA256};
use serde_json::Value;
use serde_json::value::RawValue;

/// Raised when what goes into the key changes, so that older answers are no longer found.
const VERSION: &[u8] = b"pgw-response-cache-1";

/// Members of the request that change how the answer is delivered, not what it says. A stream
/// and a whole answer to the same request share one kept answer.
const DELIVERY: &[&str] = &["stream", "stream_options"];

/// What the request says that matters for answering it again.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Traits {
    /// The temperature is 0: in the request the provider is sent, or, for a model that takes
    /// none, in the caller's request. A caller that asks for temperature 0 expects the same
    /// answer to the same request, whatever the model can be told.
    pub deterministic: bool,
    /// The request asks for something a kept answer cannot give back: several choices
    /// (`n`), or log-probabilities.
    pub unanswerable: bool,
}

/// The key of a planned call, and what its request says.
pub fn read(call: &PlannedCall<'_>) -> ([u8; 32], Traits) {
    // Members in name order: the key does not depend on the order they were written in.
    let top = serde_json::from_slice::<BTreeMap<String, &RawValue>>(call.body).ok();
    (key(call, top.as_ref()), traits(call.dialect, top.as_ref(), call.asked_temperature))
}

fn key(call: &PlannedCall<'_>, top: Option<&BTreeMap<String, &RawValue>>) -> [u8; 32] {
    let mut ctx = Context::new(&SHA256);
    let mut part = |bytes: &[u8]| {
        // Each part with its length, so that no two different calls run together alike.
        ctx.update(&(bytes.len() as u64).to_be_bytes());
        ctx.update(bytes);
    };
    part(VERSION);
    part(call.workspace_id.as_bytes());
    part(call.provider_id.as_bytes());
    part(call.model_id.as_bytes());
    part(call.upstream_id.as_bytes());
    let (template, version) = call.template.unwrap_or_default();
    part(template.as_bytes());
    part(&version.to_be_bytes());
    part(dialect_name(call.dialect).as_bytes());
    match top {
        Some(members) => {
            for (name, value) in members.iter().filter(|(name, _)| !DELIVERY.contains(&name.as_str())) {
                part(name.as_bytes());
                part(value.get().as_bytes());
            }
        }
        // A body that is not a JSON object is taken as it is.
        None => part(call.body),
    }
    for (name, value) in call.headers {
        part(name.as_bytes());
        part(value.as_bytes());
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(ctx.finish().as_ref());
    key
}

fn dialect_name(d: Dialect) -> &'static str {
    match d {
        Dialect::OpenAiChat => "openai",
        Dialect::OpenAiResponses => "openai_responses",
        Dialect::Anthropic => "anthropic",
        Dialect::Gemini => "gemini",
        Dialect::Native => "native",
        Dialect::Ollama => "ollama",
    }
}

fn traits(dialect: Dialect, top: Option<&BTreeMap<String, &RawValue>>, asked_temperature: Option<f64>) -> Traits {
    let Some(top) = top else { return Traits::default() };
    let member = |name: &str| top.get(name).map(|v| v.get());
    // Gemini keeps its sampling in an object of its own, which is small: read it whole.
    let config: Option<Value> = match dialect {
        Dialect::Gemini => member("generationConfig")
            .or_else(|| member("generation_config"))
            .and_then(|raw| serde_json::from_str(raw).ok()),
        _ => None,
    };
    let temperature = match &config {
        Some(c) => c.get("temperature").map(Value::as_f64),
        None if dialect == Dialect::Gemini => None,
        None => member("temperature").map(|raw| serde_json::from_str::<f64>(raw).ok()),
    };
    let several = match &config {
        Some(c) => {
            c.get("candidateCount").or_else(|| c.get("candidate_count")).and_then(Value::as_u64).is_some_and(|n| n > 1)
        }
        None => member("n").and_then(|raw| serde_json::from_str::<u64>(raw).ok()).is_some_and(|n| n > 1),
    };
    let logprobs = member("logprobs").is_some_and(|raw| raw != "false" && raw != "null")
        || member("top_logprobs").is_some_and(|raw| raw != "null");
    let temperature = match temperature {
        Some(t) => t,
        None => asked_temperature,
    };
    Traits { deterministic: temperature == Some(0.0), unanswerable: several || logprobs }
}

#[cfg(test)]
mod tests {
    use pacod_gw_plugin::CacheDirective;

    use super::*;

    fn call<'a>(ws: &'a str, model: &'a str, body: &'a [u8]) -> PlannedCall<'a> {
        PlannedCall {
            workspace_id: ws,
            model_id: model,
            upstream_id: "gpt-x",
            provider_id: "prv_1",
            template: Some(("assistant", 3)),
            dialect: Dialect::OpenAiChat,
            body,
            headers: &[],
            directive: CacheDirective::Default,
            asked_temperature: None,
        }
    }

    fn of(call: &PlannedCall<'_>) -> [u8; 32] {
        read(call).0
    }

    fn traits_of(dialect: Dialect, body: &[u8], asked: Option<f64>) -> Traits {
        let mut c = call("ws", "m", body);
        c.dialect = dialect;
        c.asked_temperature = asked;
        read(&c).1
    }

    #[test]
    fn a_stream_and_a_whole_answer_share_a_key() {
        let whole = br#"{"model":"gpt-x","messages":[{"role":"user","content":"hi"}],"temperature":0}"#;
        let stream = br#"{"model":"gpt-x","stream":true,"messages":[{"role":"user","content":"hi"}],"temperature":0,"stream_options":{"include_usage":true}}"#;
        assert_eq!(of(&call("ws_a", "mdl_1", whole)), of(&call("ws_a", "mdl_1", stream)));
    }

    #[test]
    fn workspaces_models_templates_and_content_never_share_a_key() {
        let body = br#"{"messages":[{"role":"user","content":"hi"}]}"#;
        let k = of(&call("ws_a", "mdl_1", body));
        assert_ne!(k, of(&call("ws_b", "mdl_1", body)), "another workspace");
        assert_ne!(k, of(&call("ws_a", "mdl_2", body)), "another model");
        let mut newer = call("ws_a", "mdl_1", body);
        newer.template = Some(("assistant", 4));
        assert_ne!(k, of(&newer), "another version of the template");
        let other = br#"{"messages":[{"role":"user","content":"hello"}]}"#;
        assert_ne!(k, of(&call("ws_a", "mdl_1", other)), "other content");
        let spaced = br#"{"messages":[{"role": "user","content":"hi"}]}"#;
        assert_ne!(k, of(&call("ws_a", "mdl_1", spaced)), "other bytes are another request");
        let mut beta = call("ws_a", "mdl_1", body);
        beta.headers = &[("anthropic-beta", "files-api-2025-04-14")];
        assert_ne!(k, of(&beta), "a header that changes the answer");
        assert_ne!(of(&call("ws_a", "mdl_1", b"not json")), of(&call("ws_a", "mdl_1", b"not json!")));
    }

    #[test]
    fn the_order_of_members_does_not_matter() {
        let a = br#"{"model":"gpt-x","temperature":0,"messages":[]}"#;
        let b = br#"{"messages":[],"model":"gpt-x","temperature":0}"#;
        assert_eq!(of(&call("ws_a", "mdl_1", a)), of(&call("ws_a", "mdl_1", b)));
    }

    #[test]
    fn the_temperature_decides_whether_a_call_is_deterministic() {
        let t = |d, b: &[u8]| traits_of(d, b, None).deterministic;
        assert!(t(Dialect::OpenAiChat, br#"{"temperature":0}"#));
        assert!(t(Dialect::Anthropic, br#"{"temperature":0.0}"#));
        assert!(t(Dialect::Gemini, br#"{"generationConfig":{"temperature":0}}"#));
        assert!(!t(Dialect::OpenAiChat, br#"{"temperature":0.7}"#));
        assert!(!t(Dialect::OpenAiChat, br#"{"messages":[]}"#), "the provider's default is not 0");
        assert!(!t(Dialect::Gemini, br#"{"temperature":0}"#), "Gemini keeps it in generationConfig");
        // A model that takes no temperature is sent none: what the caller asked for counts.
        assert!(traits_of(Dialect::Anthropic, br#"{"messages":[]}"#, Some(0.0)).deterministic);
        assert!(!traits_of(Dialect::Anthropic, br#"{"messages":[]}"#, Some(1.0)).deterministic);
        // What the provider is sent wins: a template that set another temperature.
        assert!(!traits_of(Dialect::OpenAiChat, br#"{"temperature":0.5}"#, Some(0.0)).deterministic);
    }

    #[test]
    fn several_choices_and_log_probabilities_cannot_be_answered_again() {
        let u = |d, b: &[u8]| traits_of(d, b, None).unanswerable;
        assert!(u(Dialect::OpenAiChat, br#"{"n":2}"#));
        assert!(u(Dialect::OpenAiChat, br#"{"logprobs":true}"#));
        assert!(u(Dialect::OpenAiChat, br#"{"top_logprobs":3}"#));
        assert!(u(Dialect::Gemini, br#"{"generationConfig":{"candidateCount":3}}"#));
        assert!(!u(Dialect::OpenAiChat, br#"{"n":1,"logprobs":false,"top_logprobs":null}"#));
    }
}
