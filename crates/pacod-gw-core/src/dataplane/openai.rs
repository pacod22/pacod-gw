//! The OpenAI dialect in managed mode (concept sections 4.3 and 9.3): shaping a request for
//! a provider that speaks the same dialect, shaping its answer, and the model list.
//!
//! On this path the conversation content is forwarded as the bytes the caller sent and only
//! the fields the gateway governs are rewritten. The shared pipeline is in `managed.rs`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use bytes::{Bytes, BytesMut};
use memchr::memmem;
use pacod_gw_events::Usage;
use serde_json::{Map, Value, json};

use super::Gw;
use super::auth::{Caller, identify};
use super::common::GatewayParams;
use super::errors::{Envelope, GwError};
use super::managed::Shaped;
use super::shape;
use crate::cost::Costed;
use crate::extract::{Dialect, openai_chat_usage};
use crate::json::{Kind, Op, Patch, TopLevel, apply, scan};
use crate::model::{PromptCacheMode, Quirks, UnsupportedParams};
use crate::params::{MergeContext, merge};
use crate::relay::{CallRecorder, Transform};
use crate::resolve::{Target, callable_models};
use crate::sse::{SseFramer, write_event};

fn render(v: &Value) -> Bytes {
    Bytes::from(serde_json::to_vec(v).expect("a JSON value always serialises"))
}

/// Removes `cache_control` markers from a list of messages or tools. Returns whether any was there.
pub(super) fn strip_cache_markers(items: &mut [Value]) -> bool {
    let mut found = false;
    for item in items {
        let Some(obj) = item.as_object_mut() else { continue };
        found |= obj.remove("cache_control").is_some();
        for nested in ["content", "function"] {
            match obj.get_mut(nested) {
                Some(Value::Array(blocks)) => {
                    for b in blocks {
                        if let Some(o) = b.as_object_mut() {
                            found |= o.remove("cache_control").is_some();
                        }
                    }
                }
                Some(Value::Object(o)) => found |= o.remove("cache_control").is_some(),
                _ => {}
            }
        }
    }
    found
}

fn role_of(m: &Value) -> &str {
    m.get("role").and_then(Value::as_str).unwrap_or_default()
}

/// Whether two neighbouring turns can be joined into one.
fn mergeable(a: &Value, b: &Value) -> bool {
    let plain = |m: &Value| m.get("tool_calls").is_none() && m.get("tool_call_id").is_none();
    let role = role_of(a);
    matches!(role, "user" | "assistant" | "system") && role == role_of(b) && plain(a) && plain(b)
}

fn parts_of(content: Value) -> Vec<Value> {
    match content {
        Value::Array(parts) => parts,
        Value::String(s) if s.is_empty() => Vec::new(),
        Value::String(s) => vec![json!({ "type": "text", "text": s })],
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

/// Joins `next` onto `into`. Two texts stay a text; anything else becomes a list of parts.
fn merge_turn(into: &mut Map<String, Value>, next: Map<String, Value>) {
    let a = into.remove("content").unwrap_or(Value::Null);
    let b = next.get("content").cloned().unwrap_or(Value::Null);
    let merged = match (a, b) {
        (Value::String(x), Value::String(y)) => Value::String(match (x.is_empty(), y.is_empty()) {
            (true, _) => y,
            (_, true) => x,
            _ => format!("{x}\n\n{y}"),
        }),
        (x, y) => {
            let mut parts = parts_of(x);
            parts.extend(parts_of(y));
            Value::Array(parts)
        }
    };
    into.insert("content".into(), merged);
}

/// Makes a history acceptable to a provider that rejects consecutive turns of one role and a
/// leading assistant turn (Gemini). Returns whether anything changed.
fn normalise_history(messages: &mut Vec<Value>) -> bool {
    let before = messages.len();
    // A leading assistant turn, after any system messages, has no user turn to answer.
    let first = messages.iter().position(|m| role_of(m) != "system");
    if let Some(i) = first {
        let lead =
            messages[i..].iter().take_while(|m| role_of(m) == "assistant" && m.get("tool_calls").is_none()).count();
        messages.drain(i..i + lead);
    }
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    for m in messages.drain(..) {
        match (out.last_mut(), m) {
            (Some(prev), Value::Object(next)) if mergeable(prev, &Value::Object(next.clone())) => {
                if let Some(p) = prev.as_object_mut() {
                    merge_turn(p, next);
                }
            }
            (_, m) => out.push(m),
        }
    }
    *messages = out;
    messages.len() != before
}

/// Rewrites an OpenAI-dialect request for a provider that speaks the same dialect.
/// Everything not named here keeps its bytes.
pub(super) fn shape_same(
    bytes: &Bytes,
    top: &TopLevel,
    target: &Target,
    params: &GatewayParams,
    policy: UnsupportedParams,
) -> Result<Shaped, GwError> {
    let model = &target.model.doc;
    let caps = &model.capabilities;
    let quirks: &Quirks = &target.provider.doc.quirks;
    let spec = target.template.as_ref().map(|t| &t.spec);
    let mut dropped: Vec<String> = Vec::new();
    // What the model cannot take is dropped and reported, or the request is refused.
    let unsupported = |name: &str, what: &str, dropped: &mut Vec<String>| -> Result<(), GwError> {
        if policy == UnsupportedParams::Reject {
            return Err(GwError::unsupported_feature(format!("the model \"{}\" does not take {what}", model.name)));
        }
        dropped.push(name.to_owned());
        Ok(())
    };

    let mut patch = Patch::new();
    patch.set("model", &json!(model.upstream_id));
    patch.remove("gateway");

    // --- parameters ---
    let mut sent = shape::read(Some(Dialect::OpenAiChat), bytes, top);
    if let Some((raw, thinking)) = shape::inner(bytes, top, "thinking") {
        // An Anthropic-style thinking object is accepted on every model and mapped.
        match thinking.str(raw, "type").as_deref() {
            Some("enabled") => sent.params.reasoning_budget = thinking.u64(raw, "budget_tokens"),
            Some("disabled") if sent.params.reasoning_effort.is_none() => {
                sent.params.reasoning_effort = Some("none".into())
            }
            _ => {}
        }
        if !quirks.thinking_object {
            patch.remove("thinking");
        }
    }
    let cx = MergeContext {
        spec,
        capabilities: caps,
        terms: target.terms,
        model_max_output: model.max_output_tokens,
        policy,
        filter_caller: true,
    };
    let decisions = merge(&cx, &sent.params)?;
    let mut notes = shape::Notes::default();
    let shape_target = shape::Target { capabilities: caps, quirks, managed: true, cache_auto: false };
    shape::write(Some(Dialect::OpenAiChat), &mut patch, &decisions, &sent, &shape_target, &mut notes);
    dropped.extend(decisions.dropped.iter().cloned());
    dropped.extend(notes.dropped);
    let mut adjusted = decisions.adjusted.clone();
    adjusted.extend(notes.adjusted);

    // --- features the capability profile rules out ---
    let has_tools = top.get("tools").is_some_and(|e| e.kind == Kind::Array && e.value.len() > 2);
    if has_tools && !caps.tools {
        return Err(GwError::unsupported_feature(format!("the model \"{}\" does not take tools", model.name)));
    }
    if let Some(e) = top.get("tool_choice") {
        let raw = &bytes[e.value.clone()];
        let forced = e.kind == Kind::Object || raw == b"\"required\"";
        if forced && !caps.tool_choice_forced {
            unsupported("tool_choice", "a forced tool choice", &mut dropped)?;
            patch.remove("tool_choice");
        }
    }
    if let Some((raw, format)) = shape::inner(bytes, top, "response_format")
        && format.str(raw, "type").as_deref() == Some("json_schema")
        && !caps.structured_output
    {
        unsupported("response_format", "a JSON schema for its answer", &mut dropped)?;
        patch.remove("response_format");
    }
    for field in &quirks.drop_fields {
        if top.get(field).is_some() {
            unsupported(field, &format!("the field {field}"), &mut dropped)?;
            patch.remove(field.as_str());
        }
    }

    // --- content: touched only where the provider demands it ---
    let cache_mode = params.prompt_cache.or(spec.map(|s| s.prompt_cache)).unwrap_or_default();
    let strip = cache_mode == PromptCacheMode::Off || !quirks.cache_control;
    for member in ["messages", "tools"] {
        let Some(raw) = top.raw(bytes, member) else { continue };
        let has_markers = strip && memmem::find(raw, b"\"cache_control\"").is_some();
        let normalise = member == "messages" && quirks.normalise_history;
        if !has_markers && !normalise {
            continue;
        }
        let Ok(mut items) = serde_json::from_slice::<Vec<Value>>(raw) else { continue };
        let mut changed = false;
        if has_markers && strip_cache_markers(&mut items) {
            changed = true;
            // Markers the template switches off are the template's doing, not a shortfall.
            if cache_mode != PromptCacheMode::Off && !dropped.iter().any(|d| d == "cache_control") {
                unsupported("cache_control", "prompt-cache markers", &mut dropped)?;
            }
        }
        if normalise {
            changed |= normalise_history(&mut items);
        }
        if changed {
            patch.push(member, Op::Set(render(&Value::Array(items))));
        }
    }

    // --- streaming and usage ---
    let stream = top.bool(bytes, "stream").unwrap_or(false);
    let wants_usage =
        shape::inner(bytes, top, "stream_options").and_then(|(raw, o)| o.bool(raw, "include_usage")).unwrap_or(false);
    if stream {
        if quirks.stream_usage {
            patch.nested("stream_options", "include_usage", Op::Set(Bytes::from_static(b"true")));
        } else {
            patch.remove("stream_options");
        }
    }

    let patched = apply(bytes, top, &patch)
        .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;
    Ok(Shaped { body: patched.to_bytes(), stream, wants_usage, dropped, adjusted })
}

/// The `cost` object the gateway adds to `usage` (concept section 9.3).
pub(super) fn cost_object(c: &Costed) -> Value {
    match c.amount {
        Some(amount) if c.price_version > 0 => {
            json!({ "amount": amount, "currency": c.currency, "price_version": c.price_version })
        }
        Some(amount) => json!({ "amount": amount, "currency": c.currency }),
        None => json!({ "amount": null, "price_missing": true }),
    }
}

/// Adds the cost to the `usage` object of a response or stream chunk, and the cached-token
/// detail where the provider reported it under another name.
pub(super) fn add_cost(data: &Bytes, top: &TopLevel, usage: &Usage, cost: Option<&Costed>) -> Option<Bytes> {
    let mut patch = Patch::new();
    if let Some(c) = cost {
        patch.nested("usage", "cost", Op::Set(render(&cost_object(c))));
    }
    if usage.cache_read_tokens > 0 {
        patch.nested(
            "usage",
            "prompt_tokens_details",
            Op::SetIfAbsent(render(&json!({ "cached_tokens": usage.cache_read_tokens }))),
        );
    }
    let out = apply(data, top, &patch).ok()?;
    out.changed().then(|| out.to_bytes())
}

/// Shapes an OpenAI-dialect stream on its way to the caller. Events pass as the bytes the
/// provider sent; only the chunk that carries usage is touched: the cost is added to it, or it
/// is left out when the caller did not ask for usage (the gateway always asks the provider).
pub(super) struct ChatStream {
    pub(super) framer: SseFramer,
    pub(super) wants_usage: bool,
}

impl ChatStream {
    fn rewrite(&self, data: &Bytes, rec: &CallRecorder) -> Rewritten {
        // Every chunk of a stream with usage switched on carries `"usage":null`; only the
        // final one has an object there.
        if memmem::find(data, b"\"usage\"").is_none() || memmem::find(data, b"\"usage\":null").is_some() {
            return Rewritten::Same;
        }
        let Ok(top) = scan(data) else { return Rewritten::Same };
        if top.get("usage").is_none_or(|e| e.kind != Kind::Object) {
            return Rewritten::Same;
        }
        if !self.wants_usage {
            let no_choices =
                top.raw(data, "choices").is_none_or(|c| c.iter().filter(|b| !b.is_ascii_whitespace()).eq(b"[]"));
            if no_choices {
                return Rewritten::Dropped;
            }
            let mut patch = Patch::new();
            patch.remove("usage");
            return apply(data, &top, &patch).map_or(Rewritten::Same, |p| Rewritten::Data(p.to_bytes()));
        }
        let Some(usage) = openai_chat_usage(data) else { return Rewritten::Same };
        let cost = rec.cost_of(&usage);
        add_cost(data, &top, &usage, cost.as_ref()).map_or(Rewritten::Same, Rewritten::Data)
    }
}

enum Rewritten {
    Same,
    Dropped,
    Data(Bytes),
}

impl Transform for ChatStream {
    fn chunk(&mut self, chunk: &Bytes, rec: &mut CallRecorder) -> Bytes {
        let mut pieces: Vec<Bytes> = Vec::with_capacity(2);
        let mut framer = std::mem::take(&mut self.framer);
        framer.feed_raw(chunk, |raw, event| match event.as_ref().map(|ev| (ev, self.rewrite(&ev.data, rec))) {
            Some((_, Rewritten::Dropped)) => {}
            Some((ev, Rewritten::Data(data))) => {
                let mut out = BytesMut::with_capacity(data.len() + 16);
                write_event(&mut out, &ev.event, &data);
                pieces.push(out.freeze());
            }
            _ => pieces.push(raw),
        });
        self.framer = framer;
        match pieces.len() {
            0 => Bytes::new(),
            1 => pieces.pop().expect("one piece"),
            _ => {
                let mut out = BytesMut::with_capacity(pieces.iter().map(Bytes::len).sum());
                for p in &pieces {
                    out.extend_from_slice(p);
                }
                out.freeze()
            }
        }
    }

    fn end(&mut self, _rec: &mut CallRecorder) -> Bytes {
        // Whatever the provider sent after its last complete event goes out as it came.
        self.framer.take_pending()
    }

    fn error(&mut self, kind: &str, message: &str, rec: &mut CallRecorder) -> Bytes {
        let e = if kind == "upstream_timeout" {
            GwError::upstream_timeout(message)
        } else {
            GwError::upstream_error(format!("the provider's stream broke: {message}"))
        };
        let body = serde_json::to_vec(&e.body(Envelope::OpenAi, &rec.request_id())).unwrap_or_default();
        let mut out = BytesMut::new();
        write_event(&mut out, "", &body);
        out.freeze()
    }
}

// --- the model list ----------------------------------------------------------------------------

/// A model or template as the list endpoints describe it.
struct Listed {
    id: String,
    display_name: String,
    owned_by: String,
    /// `model` or `template`.
    kind: &'static str,
    /// For a template: the model it uses.
    uses: Option<String>,
    deprecated: bool,
}

/// What a key can put into `model`: the models granted to its workspace and callable, and
/// the workspace's templates (concept section 6.5).
fn listed(caller: &Caller) -> Vec<Listed> {
    let snap = &caller.snapshot;
    let key = &caller.key.doc;
    let mut out: Vec<Listed> = callable_models(snap, &caller.workspace, &caller.key)
        .into_iter()
        .map(|m| Listed {
            id: m.doc.name.clone(),
            display_name: if m.doc.display_name.is_empty() { m.doc.name.clone() } else { m.doc.display_name.clone() },
            owned_by: snap.providers.get(&m.doc.provider_id).map(|p| p.doc.name.clone()).unwrap_or_default(),
            kind: "model",
            uses: None,
            deprecated: m.doc.state == crate::model::ModelState::Deprecated,
        })
        .collect();
    let mut templates: Vec<_> = caller
        .workspace
        .templates
        .values()
        .filter(|t| key.allowed_templates.is_empty() || key.allowed_templates.iter().any(|a| *a == t.name))
        .collect();
    templates.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(templates.into_iter().map(|t| Listed {
        id: format!("@{}", t.name),
        display_name: format!("@{}", t.name),
        owned_by: "template".into(),
        kind: "template",
        uses: snap.models.get(&t.model_id).map(|m| m.doc.name.clone()),
        deprecated: false,
    }));
    out
}

fn anthropic_shape(headers: &HeaderMap) -> bool {
    headers.contains_key("anthropic-version")
}

fn entry(l: &Listed, anthropic: bool) -> Value {
    let gateway = json!({ "kind": l.kind, "model": l.uses, "deprecated": l.deprecated });
    if anthropic {
        json!({ "type": "model", "id": l.id, "display_name": l.display_name, "created_at": "1970-01-01T00:00:00Z", "gateway": gateway })
    } else {
        json!({ "id": l.id, "object": "model", "created": 0, "owned_by": l.owned_by, "gateway": gateway })
    }
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    let mut r = Response::new(Body::from(serde_json::to_vec(body).unwrap_or_default()));
    *r.status_mut() = status;
    r.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r
}

/// `GET /v1/models`: in Anthropic's shape when the request carries `anthropic-version`,
/// otherwise in OpenAI's.
pub async fn list_models(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let anthropic = anthropic_shape(&headers);
    let envelope = if anthropic { Envelope::Anthropic } else { Envelope::OpenAi };
    let caller = match identify(&gw, &headers, Some(peer.ip())) {
        Ok(c) => c,
        Err(e) => return e.response(envelope, ""),
    };
    let all = listed(&caller);
    let data: Vec<Value> = all.iter().map(|l| entry(l, anthropic)).collect();
    let body = if anthropic {
        json!({ "data": data, "has_more": false, "first_id": all.first().map(|l| &l.id), "last_id": all.last().map(|l| &l.id) })
    } else {
        json!({ "object": "list", "data": data })
    };
    json_response(StatusCode::OK, &body)
}

/// `GET /v1/models/{id}`. Model ids contain slashes, so the id is the rest of the path.
pub async fn get_model(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let anthropic = anthropic_shape(&headers);
    let envelope = if anthropic { Envelope::Anthropic } else { Envelope::OpenAi };
    let caller = match identify(&gw, &headers, Some(peer.ip())) {
        Ok(c) => c,
        Err(e) => return e.response(envelope, ""),
    };
    match listed(&caller).iter().find(|l| l.id == id) {
        Some(l) => json_response(StatusCode::OK, &entry(l, anthropic)),
        None => GwError::not_found(
            "model_not_found",
            format!("the model \"{id}\" does not exist or is not available to this workspace"),
        )
        .response(envelope, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_markers_are_removed_wherever_callers_put_them() {
        let mut messages: Vec<Value> = serde_json::from_value(json!([
            { "role": "system", "content": [{ "type": "text", "text": "static", "cache_control": { "type": "ephemeral" } }] },
            { "role": "user", "content": "plain", "cache_control": { "type": "ephemeral" } },
            { "role": "assistant", "content": "no marker" }
        ]))
        .unwrap();
        assert!(strip_cache_markers(&mut messages));
        assert_eq!(
            Value::Array(messages.clone()),
            json!([
                { "role": "system", "content": [{ "type": "text", "text": "static" }] },
                { "role": "user", "content": "plain" },
                { "role": "assistant", "content": "no marker" }
            ])
        );
        assert!(!strip_cache_markers(&mut messages), "nothing left to remove");

        let mut tools: Vec<Value> = serde_json::from_value(
            json!([{ "type": "function", "function": { "name": "f", "cache_control": {} }, "cache_control": {} }]),
        )
        .unwrap();
        assert!(strip_cache_markers(&mut tools));
        assert_eq!(Value::Array(tools), json!([{ "type": "function", "function": { "name": "f" } }]));
    }

    #[test]
    fn a_history_is_made_acceptable_to_a_provider_that_wants_alternating_turns() {
        let mut m: Vec<Value> = serde_json::from_value(json!([
            { "role": "system", "content": "be brief" },
            { "role": "assistant", "content": "leftover greeting" },
            { "role": "user", "content": "first" },
            { "role": "user", "content": [{ "type": "image_url", "image_url": { "url": "data:x" } }] },
            { "role": "assistant", "content": "a" },
            { "role": "assistant", "content": "b" },
            { "role": "assistant", "content": null, "tool_calls": [{ "id": "c1" }] },
            { "role": "tool", "tool_call_id": "c1", "content": "r1" },
            { "role": "tool", "tool_call_id": "c2", "content": "r2" },
            { "role": "user", "content": "last" }
        ]))
        .unwrap();
        assert!(normalise_history(&mut m));
        assert_eq!(
            Value::Array(m.clone()),
            json!([
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": [{ "type": "text", "text": "first" }, { "type": "image_url", "image_url": { "url": "data:x" } }] },
                { "role": "assistant", "content": "a\n\nb" },
                { "role": "assistant", "content": null, "tool_calls": [{ "id": "c1" }] },
                { "role": "tool", "tool_call_id": "c1", "content": "r1" },
                { "role": "tool", "tool_call_id": "c2", "content": "r2" },
                { "role": "user", "content": "last" }
            ])
        );
        // An acceptable history is left alone.
        assert!(!normalise_history(&mut m));
    }

    #[test]
    fn the_cost_is_added_inside_usage_and_nothing_else_moves() {
        let body = Bytes::from_static(
            br#"{"id":"c1","choices":[{"message":{"content":"hi  there"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        );
        let top = scan(&body).unwrap();
        let usage = openai_chat_usage(&body).unwrap();
        let cost = Costed {
            amount: Some(0.007044),
            currency: "USD".into(),
            price_version: 37,
            variant: String::new(),
            price_missing: false,
        };
        let out = add_cost(&body, &top, &usage, Some(&cost)).unwrap();
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            r#"{"id":"c1","choices":[{"message":{"content":"hi  there"}}],"usage":{"prompt_tokens":10,"completion_tokens":5,"cost":{"amount":0.007044,"currency":"USD","price_version":37}}}"#
        );
        // A model without a price says so instead of showing a zero.
        assert_eq!(cost_object(&Costed::unpriced()), json!({ "amount": null, "price_missing": true }));
        assert_eq!(cost_object(&Costed::free("USD")), json!({ "amount": 0.0, "currency": "USD" }));

        // DeepSeek reports cache hits under its own name: the standard detail is added.
        let ds = Bytes::from_static(
            br#"{"usage":{"prompt_tokens":100,"completion_tokens":1,"prompt_cache_hit_tokens":60}}"#,
        );
        let u = openai_chat_usage(&ds).unwrap();
        let out = add_cost(&ds, &scan(&ds).unwrap(), &u, None).unwrap();
        assert!(
            std::str::from_utf8(&out)
                .unwrap()
                .ends_with(r#""prompt_cache_hit_tokens":60,"prompt_tokens_details":{"cached_tokens":60}}}"#)
        );
    }
}
