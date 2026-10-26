//! OpenAI's Responses API in managed mode (`POST /v1/responses`), stateless.
//!
//! A provider whose quirk profile says `responses_api` (OpenAI, xAI) is spoken to in this
//! dialect: the caller's bytes are patched, as on the chat route. Every other provider is
//! reached through the neutral model (`translate.rs`). Either way the gateway keeps no state
//! and lets the provider keep none: the stateful members are refused before resolution, and
//! `store` is always false at the provider, because a stored response on a credential that
//! several workspaces share could be read by any of them.

use axum::response::Response;
use bytes::{Bytes, BytesMut};
use memchr::memmem;
use pacod_gw_events::Usage;
use serde_json::{Value, json};

use super::common::GatewayParams;
use super::errors::{Envelope, GwError};
use super::managed::Shaped;
use super::openai::{cost_object, request_field_ops};
use super::shape;
use crate::cost::Costed;
use crate::extract::Dialect;
use crate::json::{Kind, Op, Patch, TopLevel, apply};
use crate::model::{ReasoningStyle, UnsupportedParams};
use crate::params::{MergeContext, merge};
use crate::relay::{CallRecorder, Transform};
use crate::resolve::Target;
use crate::sse::{SseFramer, write_event};

fn render(v: &Value) -> Bytes {
    Bytes::from(serde_json::to_vec(v).expect("a JSON value always serialises"))
}

/// Refuses a request that asks the provider to keep or reuse state.
pub(super) fn refuse_stateful(bytes: &[u8]) -> Result<(), GwError> {
    let v: Value = serde_json::from_slice(bytes)
        .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;
    match pacod_gw_dialects::responses::stateful(&v) {
        Some(why) => Err(GwError::invalid_request(why)),
        None => Ok(()),
    }
}

/// Proxy mode on the installation's shared credential: what the provider keeps for that
/// credential is every workspace's, so a request that reuses it needs a credential of its own.
pub(super) fn check_shared(bytes: &[u8]) -> Result<(), GwError> {
    let Ok(v) = serde_json::from_slice::<Value>(bytes) else { return Ok(()) };
    match pacod_gw_dialects::responses::reuses_state(&v) {
        Some(why) => Err(GwError::own_credential_required(format!(
            "{why}. On the shared credential that would reach other workspaces' responses: it needs a \
             credential of this workspace or organisation"
        ))),
        None => Ok(()),
    }
}

/// The paths of the Responses API that read or change a stored response.
pub async fn not_stored() -> Response {
    GwError::not_found(
        "not_stored",
        "this gateway serves /v1/responses statelessly: store is always false, and no response is kept to read, cancel or delete",
    )
    .response(Envelope::OpenAi, "")
}

/// Reasoning items whose `encrypted_content` another family made (`gw1.…`): an earlier turn
/// was answered by another provider, whose reasoning OpenAI cannot read and refuses.
fn foreign_reasoning(items: &mut Vec<Value>) -> bool {
    let before = items.len();
    items.retain(|i| {
        !(i.get("type").and_then(Value::as_str) == Some("reasoning")
            && i.get("encrypted_content")
                .and_then(Value::as_str)
                .is_some_and(|e| e.starts_with(pacod_gw_dialects::canonical::SIGNATURE_PREFIX)))
    });
    items.len() != before
}

/// Rewrites a Responses request for a provider that serves the Responses API. Everything not
/// named here keeps its bytes.
pub(super) fn shape_same(
    bytes: &Bytes,
    top: &TopLevel,
    target: &Target,
    _params: &GatewayParams,
    policy: UnsupportedParams,
) -> Result<Shaped, GwError> {
    let model = &target.model.doc;
    let caps = &model.capabilities;
    let quirks = &target.provider.doc.quirks;
    let spec = target.template.as_ref().map(|t| &t.spec);
    let mut dropped: Vec<String> = Vec::new();
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
    request_field_ops(&mut patch, &quirks.request_fields);
    // Nothing is kept at the provider (see the module's comment).
    patch.set("store", &json!(false));
    // Without a stored response, reasoning crosses turns only as encrypted content.
    if caps.reasoning != ReasoningStyle::None {
        let mut include: Vec<Value> = top
            .raw(bytes, "include")
            .and_then(|raw| serde_json::from_slice::<Vec<Value>>(raw).ok())
            .unwrap_or_default();
        if !include.iter().any(|v| v == "reasoning.encrypted_content") {
            include.push(json!("reasoning.encrypted_content"));
            patch.push("include", Op::Set(render(&Value::Array(include))));
        }
    }

    // --- parameters ---
    let sent = shape::read(Some(Dialect::OpenAiResponses), bytes, top);
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
    shape::write(Some(Dialect::OpenAiResponses), &mut patch, &decisions, &sent, &shape_target, &mut notes);
    dropped.extend(decisions.dropped.iter().cloned());
    dropped.extend(notes.dropped);
    let mut adjusted = decisions.adjusted.clone();
    let sources = decisions.sources.clone();
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
        } else if raw == b"\"none\"" && !caps.tool_choice_none {
            unsupported("tool_choice", "the tool choice none", &mut dropped)?;
            patch.remove("tool_choice");
        }
    }
    if let Some((raw, text)) = shape::inner(bytes, top, "text")
        && let Some((format_raw, format)) = shape::inner(raw, &text, "format")
        && format.str(format_raw, "type").as_deref() == Some("json_schema")
        && !caps.structured_output
    {
        unsupported("text.format", "a JSON schema for its answer", &mut dropped)?;
        patch.nested("text", "format", Op::Remove);
    }
    for field in quirks.drop_fields.iter().chain(&caps.drop_fields) {
        if top.get(field).is_some() && !dropped.iter().any(|d| d == field) {
            unsupported(field, &format!("the field {field}"), &mut dropped)?;
            patch.remove(field.as_str());
        }
    }

    // --- content: touched only where another provider's reasoning would be refused ---
    if let Some(raw) = top.raw(bytes, "input")
        && memmem::find(raw, b"\"gw1.").is_some()
        && let Ok(mut items) = serde_json::from_slice::<Vec<Value>>(raw)
        && foreign_reasoning(&mut items)
    {
        patch.push("input", Op::Set(render(&Value::Array(items))));
        adjusted.push("reasoning items another provider wrote were left out: this provider cannot read them".into());
    }

    let stream = top.bool(bytes, "stream").unwrap_or(false);
    let patched = apply(bytes, top, &patch)
        .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;
    // The Responses API always reports usage, streamed or not.
    Ok(Shaped { body: patched.to_bytes(), stream, wants_usage: true, dropped, adjusted, sources })
}

/// Adds the cost to the `usage` of a whole response.
pub(super) fn add_cost(body: &Bytes, top: &TopLevel, cost: Option<&Costed>) -> Option<Bytes> {
    let c = cost?;
    if top.get("usage").is_none_or(|e| e.kind != Kind::Object) {
        return None;
    }
    let mut patch = Patch::new();
    patch.nested("usage", "cost", Op::Set(render(&cost_object(c))));
    apply(body, top, &patch).ok().map(|p| p.to_bytes())
}

/// The usage of the event that ends a stream, read as the tap reads it.
fn final_usage(data: &[u8]) -> Option<Usage> {
    let found = crate::extract::parse_whole(Dialect::OpenAiResponses, data, &crate::extract::default_usage_fields());
    found.has_usage.then_some(found.usage)
}

/// Passes a Responses stream on as the provider sent it. Only the event that ends it is
/// touched: the cost is added to the usage of its response.
pub(super) struct Stream {
    pub(super) framer: SseFramer,
}

impl Stream {
    fn rewrite(data: &Bytes, rec: &CallRecorder) -> Option<Bytes> {
        let ends = [&b"\"response.completed\""[..], b"\"response.incomplete\"", b"\"response.failed\""];
        if !ends.iter().any(|e| memmem::find(data, e).is_some()) {
            return None;
        }
        let usage = final_usage(data)?;
        let cost = rec.cost_of(&usage)?;
        let mut v: Value = serde_json::from_slice(data).ok()?;
        let u = v.pointer_mut("/response/usage").filter(|u| u.is_object())?;
        u["cost"] = cost_object(&cost);
        serde_json::to_vec(&v).ok().map(Bytes::from)
    }
}

impl Transform for Stream {
    fn chunk(&mut self, chunk: &Bytes, rec: &mut CallRecorder) -> Bytes {
        let mut out = BytesMut::with_capacity(chunk.len());
        let mut framer = std::mem::take(&mut self.framer);
        framer.feed_raw(chunk, |raw, event| {
            match event.as_ref().and_then(|ev| Self::rewrite(&ev.data, rec).map(|d| (ev, d))) {
                Some((ev, data)) => write_event(&mut out, &ev.event, &data),
                None => out.extend_from_slice(&raw),
            }
        });
        self.framer = framer;
        out.freeze()
    }

    fn end(&mut self, _rec: &mut CallRecorder) -> Bytes {
        self.framer.take_pending()
    }

    fn error(&mut self, kind: &str, message: &str, _rec: &mut CallRecorder) -> Bytes {
        let code = if kind == "upstream_timeout" { "upstream_timeout" } else { "upstream_error" };
        let body = json!({ "type": "error", "code": code, "message": format!("the provider's stream broke: {message}"), "param": null });
        let mut out = BytesMut::new();
        write_event(&mut out, "error", body.to_string().as_bytes());
        out.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_providers_reasoning_is_left_out_for_openai() {
        let mut items = vec![
            json!({ "role": "user", "content": "hi" }),
            json!({ "type": "reasoning", "summary": [], "encrypted_content": "gw1.anthropic.SIG" }),
            json!({ "type": "reasoning", "summary": [], "encrypted_content": "gAAAA" }),
        ];
        assert!(foreign_reasoning(&mut items));
        assert_eq!(items.len(), 2);
        assert_eq!(items[1]["encrypted_content"], "gAAAA");
        assert!(!foreign_reasoning(&mut items));
    }

    #[test]
    fn state_is_refused_as_an_invalid_request() {
        let e = refuse_stateful(br#"{"model":"m","input":"hi","previous_response_id":"resp_1"}"#).unwrap_err();
        assert_eq!((e.status.as_u16(), e.kind), (400, "invalid_request"));
        assert!(refuse_stateful(br#"{"model":"m","input":"hi","store":false}"#).is_ok());
    }
}
