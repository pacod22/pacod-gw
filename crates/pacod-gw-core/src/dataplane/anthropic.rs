//! The Anthropic dialect in managed mode (concept sections 8.3 and 9.4): shaping a Messages
//! request for an Anthropic model.
//!
//! This is the path on which nothing may be lost. `system`, `tools` and `messages` are
//! forwarded as the bytes the caller sent: prompt caching matches the exact bytes of the
//! prefix, and thinking blocks are valid only in an unedited history. The gateway changes a
//! short list of fields outside them and forwards everything else, known or not.

use bytes::Bytes;
use serde::Deserialize;
use serde_json::{Value, json};

use super::common::GatewayParams;
use super::errors::GwError;
use super::managed::{Shaped, default_max_output};
use super::openai::strip_cache_markers;
use super::shape;
use crate::extract::Dialect;
use crate::json::{Kind, Op, Patch, TopLevel, apply};
use crate::model::{PromptCacheMode, UnsupportedParams};
use crate::params::{MergeContext, merge};
use crate::resolve::Target;

/// What the gateway needs to know about a message: who said it.
#[derive(Deserialize)]
struct RoleOnly {
    #[serde(default)]
    role: String,
}

/// Rewrites an Anthropic Messages request for an Anthropic model.
pub(super) fn shape_same(
    bytes: &Bytes,
    top: &TopLevel,
    target: &Target,
    params: &GatewayParams,
    policy: UnsupportedParams,
) -> Result<Shaped, GwError> {
    let model = &target.model.doc;
    let caps = &model.capabilities;
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

    // --- parameters ---
    let sent = shape::read(Some(Dialect::Anthropic), bytes, top);
    let cx = MergeContext {
        spec,
        capabilities: caps,
        terms: target.terms,
        model_max_output: model.max_output_tokens,
        policy,
        filter_caller: true,
    };
    let decisions = merge(&cx, &sent.params)?;
    let cache_mode = params.prompt_cache.or(spec.map(|s| s.prompt_cache)).unwrap_or_default();
    let mut notes = shape::Notes::default();
    let shape_target = shape::Target {
        capabilities: caps,
        quirks: &target.provider.doc.quirks,
        managed: true,
        cache_auto: cache_mode == PromptCacheMode::Auto,
    };
    shape::write(Some(Dialect::Anthropic), &mut patch, &decisions, &sent, &shape_target, &mut notes);
    dropped.extend(decisions.dropped.iter().cloned());
    dropped.extend(notes.dropped);
    let mut adjusted = decisions.adjusted.clone();
    adjusted.extend(notes.adjusted);
    // Anthropic requires an output-token limit. A caller that names none gets the template's
    // (written above) or the model's own.
    if decisions.effective_max_output.is_none() {
        patch.set("max_tokens", &json!(default_max_output(target)));
    }

    // --- what the capability profile rules out is never sent upstream to fail there ---
    let tools = top.get("tools").filter(|e| e.kind == Kind::Array && e.value.len() > 2);
    if tools.is_some() && !caps.tools {
        return Err(GwError::unsupported_feature(format!("the model \"{}\" does not take tools", model.name)));
    }
    if let Some((raw, choice)) = shape::inner(bytes, top, "tool_choice")
        && matches!(choice.str(raw, "type").as_deref(), Some("any" | "tool"))
        && !caps.tool_choice_forced
    {
        unsupported("tool_choice", "a forced tool choice", &mut dropped)?;
        patch.remove("tool_choice");
    }
    if let Some((raw, config)) = shape::inner(bytes, top, "output_config")
        && config.get("format").is_some()
        && !caps.structured_output
    {
        let _ = raw;
        unsupported("output_config.format", "a JSON schema for its answer", &mut dropped)?;
        patch.nested("output_config", "format", Op::Remove);
    }
    if !caps.assistant_prefill
        && let Some(raw) = top.raw(bytes, "messages")
        && let Ok(roles) = serde_json::from_slice::<Vec<RoleOnly>>(raw)
        && roles.last().is_some_and(|m| m.role == "assistant")
    {
        // There is nothing to drop here: without the last turn the request means something else.
        return Err(GwError::unsupported_feature(format!(
            "the model \"{}\" does not continue a partial assistant message; end the conversation with a user turn",
            model.name
        )));
    }

    // --- a template or the caller can switch prompt caching off; that is the one case in
    // which the content is touched on this path ---
    if cache_mode == PromptCacheMode::Off {
        for member in ["system", "messages", "tools"] {
            let Some(entry) = top.get(member).filter(|e| e.kind == Kind::Array) else { continue };
            let raw = &bytes[entry.value.clone()];
            if memchr::memmem::find(raw, b"\"cache_control\"").is_none() {
                continue;
            }
            let Ok(mut items) = serde_json::from_slice::<Vec<Value>>(raw) else { continue };
            if strip_cache_markers(&mut items) {
                let rendered = serde_json::to_vec(&Value::Array(items)).expect("a JSON value always serialises");
                patch.push(member, Op::Set(Bytes::from(rendered)));
            }
        }
        patch.remove("cache_control");
    }

    let stream = top.bool(bytes, "stream").unwrap_or(false);
    let patched = apply(bytes, top, &patch)
        .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;
    // Anthropic's streams always carry usage; there is nothing to ask for or to hide.
    Ok(Shaped { body: patched.to_bytes(), stream, wants_usage: true, dropped, adjusted })
}
