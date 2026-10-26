//! The translating path of managed mode (concept sections 8.2 to 8.5): the caller speaks one
//! dialect and the provider another, or the caller speaks the gateway's own.
//!
//! The request is read into the neutral model by the client half of the caller's dialect,
//! governed there (template, capability profile, cache policy), and written by the provider
//! half of the provider's dialect. The answer takes the same way back. The translators
//! themselves are pure functions in `pacod-gw-dialects`; this module decides what they are
//! given.

use bytes::Bytes;
use pacod_gw_dialects::canonical::{
    Block, EchoReasoning as WireEcho, Message, Notes, Profile, ReasoningParam as WireReasoningParam,
    ReasoningStyle as WireReasoning, Request, Response as NeutralResponse, Role, Stamp, ThinkingOff as WireThinkingOff,
    Tool, ToolChoice,
};
use pacod_gw_dialects::{ClientStream, DialectError, ProviderStream, anthropic, gemini, native, openai, responses};

use super::common::GatewayParams;
use super::errors::GwError;
use super::managed::{Interface, PathCall, Shaped, Wire, default_max_output};
use crate::extract::Dialect;
use crate::model::{
    EchoReasoning, MaxTokensField, PromptCacheMode, ReasoningParam, ReasoningStyle, Rule, ThinkingOff,
    UnsupportedParams,
};
use crate::params::{CallerParams, Decision, MergeContext, merge};
use crate::resolve::Target;

pub(super) fn dialect_error(e: DialectError) -> GwError {
    match e {
        DialectError::InvalidRequest(m) => GwError::invalid_request(m),
        DialectError::Unsupported(m) => GwError::unsupported_feature(m),
        DialectError::BadProviderResponse(m) => GwError::upstream_error(m),
    }
}

/// What the provider half needs to know about the model and its endpoint.
pub(super) fn profile_for(target: &Target, wire: Wire, cache_auto: bool) -> Profile {
    let caps = &target.model.doc.capabilities;
    let quirks = &target.provider.doc.quirks;
    Profile {
        family: wire.name().to_owned(),
        upstream_model: target.model.doc.upstream_id.clone(),
        reasoning: match caps.reasoning {
            ReasoningStyle::None => WireReasoning::None,
            ReasoningStyle::Budget => WireReasoning::Budget,
            ReasoningStyle::Adaptive => WireReasoning::Adaptive,
            ReasoningStyle::AlwaysOn => WireReasoning::AlwaysOn,
            ReasoningStyle::Effort => WireReasoning::Effort,
        },
        reasoning_efforts: caps.reasoning_efforts.clone(),
        thinking_off: match caps.thinking_off {
            ThinkingOff::Disabled => WireThinkingOff::Disabled,
            ThinkingOff::BetweenTools => WireThinkingOff::BetweenTools,
            ThinkingOff::Unsupported => WireThinkingOff::Unsupported,
        },
        max_completion_tokens: caps.max_tokens_field == MaxTokensField::MaxCompletionTokens,
        reasoning_param: match caps.reasoning_param_or(quirks.reasoning_param) {
            ReasoningParam::ReasoningEffort => WireReasoningParam::ReasoningEffort,
            ReasoningParam::ThinkingToggle => WireReasoningParam::ThinkingToggle,
            ReasoningParam::None => WireReasoningParam::None,
        },
        reasoning_with_tools: caps.reasoning_with_tools,
        reasoning_field: quirks.reasoning_field.clone(),
        // Anthropic takes cache markers; an OpenAI-compatible endpoint only where its quirk
        // profile says so.
        cache_markers: wire == Wire::Anthropic || quirks.cache_control,
        cache_auto: cache_auto && wire == Wire::Anthropic,
        stream_usage: quirks.stream_usage,
        default_max_output: default_max_output(target),
        documents: caps.documents,
        cached_tokens_at: quirks.cached_tokens_at.clone(),
        reasoning_tokens_at: quirks.reasoning_tokens_at.clone(),
        cache_write_tokens_at: quirks.cache_write_tokens_at.clone(),
        extra_params: quirks.extra_params.clone(),
        echo_reasoning: match (wire, quirks.echo_reasoning) {
            (Wire::Anthropic, _) | (_, EchoReasoning::Never) => WireEcho::Never,
            (_, EchoReasoning::WithTools) => WireEcho::WithTools,
            (_, EchoReasoning::Always) => WireEcho::Always,
        },
        provider: target.provider.doc.name.clone(),
        thought_signatures: wire == Wire::OpenAi && quirks.thought_signatures,
        signature_family: if target.provider.vertex.is_some() { "vertex".into() } else { String::new() },
        reasoning_outside_completion: wire == Wire::OpenAi && quirks.reasoning_outside_completion,
    }
}

fn set<T>(slot: &mut Option<T>, decision: Decision<T>) {
    match decision {
        Decision::Keep => {}
        Decision::Set(v) => *slot = Some(v),
        Decision::Remove => *slot = None,
    }
}

/// Removes every cache marker from a request. Used when a template switches caching off.
fn strip_cache(req: &mut Request) {
    fn strip(blocks: &mut [Block]) {
        for b in blocks {
            match b {
                Block::Text { cache, .. }
                | Block::Image { cache, .. }
                | Block::Document { cache, .. }
                | Block::ToolCall { cache, .. } => *cache = None,
                Block::ToolResult { cache, content, .. } => {
                    *cache = None;
                    strip(content);
                }
                Block::Reasoning { .. } | Block::Provider { .. } => {}
            }
        }
    }
    strip(&mut req.system);
    for Message { content, .. } in &mut req.messages {
        strip(content);
    }
    for t in &mut req.tools {
        if let Tool::Function { cache, .. } = t {
            *cache = None;
        }
    }
}

fn has_image(blocks: &[Block]) -> bool {
    blocks.iter().any(|b| match b {
        Block::Image { .. } => true,
        Block::ToolResult { content, .. } => has_image(content),
        _ => false,
    })
}

/// Reads the caller's request: the model and whether to stream are in a Gemini caller's path.
pub(super) fn parse(iface: Interface, bytes: &Bytes, call: Option<&PathCall>) -> Result<Request, GwError> {
    let mut req = match iface {
        Interface::OpenAi => openai::parse_request(bytes),
        Interface::Anthropic => anthropic::parse_request(bytes),
        Interface::Gemini => gemini::parse_request(bytes),
        Interface::Native => native::parse_request(bytes),
        Interface::Responses => responses::parse_request(bytes),
    }
    .map_err(dialect_error)?;
    if let Some(c) = call {
        req.model.clone_from(&c.model);
        req.stream = c.stream;
    }
    Ok(req)
}

/// Reads the caller's request, governs it and writes the provider's.
pub(super) fn shape(
    iface: Interface,
    wire: Wire,
    bytes: &Bytes,
    target: &Target,
    params: &GatewayParams,
    policy: UnsupportedParams,
    call: Option<&PathCall>,
) -> Result<(Shaped, Profile), GwError> {
    let mut req = parse(iface, bytes, call)?;

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

    // --- what the capability profile rules out ---
    if !req.tools.is_empty() && !caps.tools {
        return Err(GwError::unsupported_feature(format!("the model \"{}\" does not take tools", model.name)));
    }
    if !caps.vision && (has_image(&req.system) || req.messages.iter().any(|m| has_image(&m.content))) {
        return Err(GwError::unsupported_feature(format!("the model \"{}\" does not take images", model.name)));
    }
    if matches!(req.tool_choice, Some(ToolChoice::Required | ToolChoice::Tool(_))) && !caps.tool_choice_forced {
        unsupported("tool_choice", "a forced tool choice", &mut dropped)?;
        req.tool_choice = None;
    }
    if matches!(req.tool_choice, Some(ToolChoice::None)) && !caps.tool_choice_none {
        unsupported("tool_choice", "the tool choice none", &mut dropped)?;
        req.tool_choice = None;
    }
    if matches!(req.response_format, Some(pacod_gw_dialects::canonical::ResponseFormat::JsonSchema { .. }))
        && !caps.structured_output
    {
        unsupported("response_format", "a JSON schema for its answer", &mut dropped)?;
        req.response_format = None;
    }
    if !caps.assistant_prefill && req.messages.last().is_some_and(|m| m.role == Role::Assistant) {
        return Err(GwError::unsupported_feature(format!(
            "the model \"{}\" does not continue a partial assistant message; end the conversation with a user turn",
            model.name
        )));
    }

    // --- parameters: template, caller, capability filter ---
    let caller = CallerParams {
        max_output_tokens: req.params.max_output_tokens,
        temperature: req.params.temperature,
        top_p: req.params.top_p,
        top_k: req.params.top_k,
        frequency_penalty: req.params.frequency_penalty,
        presence_penalty: req.params.presence_penalty,
        reasoning_effort: req.params.reasoning.effort.clone(),
        reasoning_budget: req.params.reasoning.budget_tokens,
    };
    let cx = MergeContext {
        spec,
        capabilities: caps,
        terms: target.terms,
        model_max_output: model.max_output_tokens,
        policy,
        filter_caller: true,
    };
    let decisions = merge(&cx, &caller)?;
    dropped.extend(decisions.dropped.iter().cloned());
    let mut adjusted = decisions.adjusted.clone();
    let sources = decisions.sources.clone();
    let p = &mut req.params;
    set(&mut p.max_output_tokens, decisions.max_output_tokens);
    set(&mut p.temperature, decisions.temperature);
    set(&mut p.top_p, decisions.top_p);
    set(&mut p.top_k, decisions.top_k);
    set(&mut p.frequency_penalty, decisions.frequency_penalty);
    set(&mut p.presence_penalty, decisions.presence_penalty);
    set(&mut p.reasoning.effort, decisions.reasoning_effort);
    set(&mut p.reasoning.budget_tokens, decisions.reasoning_budget);
    match spec.map_or(Rule::Unset, |s| Rule::of(&s.reasoning.summary)) {
        Rule::Locked(v) => p.reasoning.summary = Some(v),
        Rule::Default(v) => p.reasoning.summary = p.reasoning.summary.or(Some(v)),
        Rule::Omit => p.reasoning.summary = None,
        Rule::Unset => {}
    }

    // --- prompt caching ---
    let cache_mode = params.prompt_cache.or(spec.map(|s| s.prompt_cache)).unwrap_or_default();
    if cache_mode == PromptCacheMode::Off {
        strip_cache(&mut req);
    }
    let profile = profile_for(target, wire, cache_mode == PromptCacheMode::Auto);

    // --- the provider's request ---
    let mut notes = Notes::default();
    let mut body = match wire {
        Wire::OpenAi => openai::build_request(&req, &profile, &mut notes),
        Wire::Anthropic => anthropic::build_request(&req, &profile, &mut notes),
        Wire::Gemini => gemini::build_request(&req, &profile, &mut notes),
    }
    .map_err(dialect_error)?;
    // What the provider's quirk profile adds and removes, as on the same-dialect route.
    if wire == Wire::OpenAi
        && let Some(obj) = body.as_object_mut()
    {
        let quirks = &target.provider.doc.quirks;
        for field in quirks.drop_fields.iter().chain(&target.model.doc.capabilities.drop_fields) {
            if obj.shift_remove(field.as_str()).is_some() {
                unsupported(field, &format!("the field {field}"), &mut dropped)?;
            }
        }
        super::openai::add_request_fields(obj, &quirks.request_fields);
        if quirks.normalise_history
            && let Some(serde_json::Value::Array(messages)) = obj.get_mut("messages")
        {
            super::openai::normalise_history(messages);
        }
    }
    // What has no meaning on the target was left out. Whether that is acceptable is the
    // caller's choice (concept section 9.3).
    if policy == UnsupportedParams::Reject && !notes.dropped.is_empty() {
        return Err(GwError::unsupported_feature(format!(
            "the model \"{}\" cannot take: {}",
            model.name,
            notes.dropped.join(", ")
        )));
    }
    for name in notes.dropped {
        if !dropped.contains(&name) {
            dropped.push(name);
        }
    }
    adjusted.extend(notes.adjusted);

    let rendered = serde_json::to_vec(&body).map_err(GwError::internal)?;
    Ok((
        Shaped {
            body: Bytes::from(rendered),
            stream: req.stream,
            wants_usage: req.stream_usage,
            dropped,
            adjusted,
            sources,
        },
        profile,
    ))
}

/// Reads a complete provider answer into the neutral model.
pub(super) fn read_response(wire: Wire, body: &[u8], profile: &Profile) -> Result<NeutralResponse, GwError> {
    match wire {
        Wire::OpenAi => openai::read_response(body, profile),
        Wire::Anthropic => anthropic::read_response(body, profile),
        Wire::Gemini => gemini::read_response(body, profile),
    }
    .map_err(dialect_error)
}

/// Writes a complete answer in the caller's dialect.
pub(super) fn write_response(iface: Interface, response: &NeutralResponse, stamp: &Stamp) -> Bytes {
    let value = match iface {
        Interface::OpenAi => openai::write_response(response, stamp),
        Interface::Anthropic => anthropic::write_response(response, stamp),
        Interface::Gemini => gemini::write_response(response, stamp),
        Interface::Native => native::write_response(response, stamp),
        Interface::Responses => responses::write_response(response, stamp),
    };
    Bytes::from(serde_json::to_vec(&value).expect("a JSON value always serialises"))
}

/// A writer of the caller's stream. `sse` frames a Gemini caller's stream as server-sent events
/// rather than one JSON array.
pub(super) fn client_stream(iface: Interface, stamp: Stamp, include_usage: bool, sse: bool) -> Box<dyn ClientStream> {
    match iface {
        Interface::OpenAi => openai::client_stream(stamp, include_usage),
        Interface::Anthropic => anthropic::client_stream(stamp, include_usage),
        Interface::Gemini => gemini::client_stream(stamp, sse),
        Interface::Native => native::client_stream(stamp, include_usage),
        Interface::Responses => responses::client_stream(stamp),
    }
}

pub(super) fn provider_stream(wire: Wire, profile: &Profile) -> Box<dyn ProviderStream> {
    match wire {
        Wire::OpenAi => openai::provider_stream(profile),
        Wire::Anthropic => anthropic::provider_stream(profile),
        Wire::Gemini => gemini::provider_stream(profile),
    }
}

/// A reader of a provider's stream in the dialect it answers in (see `Plan::reads`).
pub(super) fn provider_stream_of(reads: Dialect, wire: Wire, profile: &Profile) -> Box<dyn ProviderStream> {
    match reads {
        Dialect::OpenAiResponses => responses::provider_stream(profile),
        _ => provider_stream(wire, profile),
    }
}

/// Reads a complete answer in the dialect the provider answers in (see `Plan::reads`).
pub(super) fn read_response_of(
    reads: Dialect,
    wire: Wire,
    body: &[u8],
    profile: &Profile,
) -> Result<NeutralResponse, GwError> {
    match reads {
        Dialect::OpenAiResponses => responses::read_response(body, profile).map_err(dialect_error),
        _ => read_response(wire, body, profile),
    }
}
