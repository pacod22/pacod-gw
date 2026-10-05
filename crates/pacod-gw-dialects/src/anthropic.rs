//! The Anthropic Messages dialect (concept sections 8.3 and 9.4).
//!
//! Client half: `parse_request`, `write_response`, `client_stream`. Callers that speak this
//! dialect reach a model of another provider through these.
//!
//! Provider half: `build_request`, `read_response`, `provider_stream`. An Anthropic model is
//! spoken to through these when the caller speaks another dialect.
//!
//! When caller and model are both Anthropic, none of this runs: the core patches the
//! caller's bytes, so nothing Anthropic's API accepts can be lost.

use std::collections::BTreeMap;

use bytes::BytesMut;
use pacod_gw_events::Usage;
use serde_json::{Map, Value, json};

use crate::canonical::{
    Block, BlockStart, Cache, Delta, Event, Finish, ImageSource, Message, Notes, Profile, ReasoningStyle, Request,
    Response, ResponseFormat, Role, Stamp, ThinkingOff, Tool, ToolChoice,
};
use crate::error::DialectError;
use crate::sse::write_event;
use crate::util::{
    arguments_value, cache_from, cache_to, merge_usage_anthropic, object, str_of, strings, take_bool, take_f64,
    take_string, take_u64, usage_anthropic,
};
use crate::{ClientStream, ProviderStream};

const FAMILY: &str = "anthropic";

// --- blocks, in both directions ------------------------------------------------------------------

fn image_source(source: &Value) -> Option<ImageSource> {
    match str_of(source, "type") {
        Some("base64") => Some(ImageSource::Base64 {
            media_type: str_of(source, "media_type").unwrap_or_default().to_owned(),
            data: str_of(source, "data").unwrap_or_default().to_owned(),
        }),
        Some("url") => Some(ImageSource::Url { url: str_of(source, "url").unwrap_or_default().to_owned() }),
        _ => None,
    }
}

/// Reads one content block. Used for requests and for answers.
fn read_block(b: &Value) -> Block {
    let cache = cache_from(b.get("cache_control"));
    match str_of(b, "type") {
        Some("text") => Block::Text {
            text: str_of(b, "text").unwrap_or_default().to_owned(),
            citations: b.get("citations").filter(|c| !c.is_null()).cloned(),
            cache,
        },
        Some("image") => match b.get("source").and_then(image_source) {
            Some(source) => Block::Image { source, cache },
            // A file reference or a source type of the future: only Anthropic understands it.
            None => Block::Provider { family: FAMILY.into(), raw: b.clone() },
        },
        Some("document") => {
            let mut body = b.as_object().cloned().unwrap_or_default();
            body.shift_remove("type");
            body.shift_remove("cache_control");
            Block::Document { body, cache }
        }
        Some("tool_use") => Block::ToolCall {
            id: str_of(b, "id").unwrap_or_default().to_owned(),
            name: str_of(b, "name").unwrap_or_default().to_owned(),
            arguments: b.get("input").map(Value::to_string).unwrap_or_default(),
            cache,
        },
        Some("tool_result") => Block::ToolResult {
            tool_call_id: str_of(b, "tool_use_id").unwrap_or_default().to_owned(),
            content: read_content(b.get("content")),
            is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            cache,
        },
        Some("thinking") => Block::Reasoning {
            text: str_of(b, "thinking").unwrap_or_default().to_owned(),
            signature: str_of(b, "signature").filter(|s| !s.is_empty()).map(str::to_owned),
            redacted: None,
        },
        Some("redacted_thinking") => {
            Block::Reasoning { text: String::new(), signature: None, redacted: str_of(b, "data").map(str::to_owned) }
        }
        // Server tools, their results, and whatever Anthropic adds next.
        _ => Block::Provider { family: FAMILY.into(), raw: b.clone() },
    }
}

fn read_content(content: Option<&Value>) -> Vec<Block> {
    match content {
        Some(Value::String(s)) if !s.is_empty() => vec![Block::text(s.clone())],
        Some(Value::Array(blocks)) => blocks.iter().map(read_block).collect(),
        _ => Vec::new(),
    }
}

/// Writes one content block for Anthropic. `None` for a block Anthropic cannot be given.
fn write_block(b: &Block, markers: bool, notes: &mut Notes) -> Result<Option<Value>, DialectError> {
    let marked = |mut v: Value, cache: &Option<Cache>, notes: &mut Notes| -> Value {
        match cache {
            Some(c) if markers => v["cache_control"] = cache_to(c),
            Some(_) => note(notes, "cache_control"),
            None => {}
        }
        v
    };
    Ok(Some(match b {
        Block::Text { text, citations, cache } => {
            let mut v = json!({ "type": "text", "text": text });
            if let Some(c) = citations {
                v["citations"] = c.clone();
            }
            marked(v, cache, notes)
        }
        Block::Image { source, cache } => {
            let source = match source {
                ImageSource::Base64 { media_type, data } => {
                    json!({ "type": "base64", "media_type": media_type, "data": data })
                }
                ImageSource::Url { url } => json!({ "type": "url", "url": url }),
            };
            marked(json!({ "type": "image", "source": source }), cache, notes)
        }
        Block::Document { body, cache } => {
            let mut v = Map::new();
            v.insert("type".into(), json!("document"));
            for (k, value) in body {
                v.insert(k.clone(), value.clone());
            }
            marked(Value::Object(v), cache, notes)
        }
        Block::ToolCall { id, name, arguments, cache } => {
            let input = arguments_value(arguments).map_err(|e| {
                DialectError::invalid(format!("the arguments of the tool call \"{id}\" are not valid JSON: {e}"))
            })?;
            marked(json!({ "type": "tool_use", "id": id, "name": name, "input": input }), cache, notes)
        }
        Block::ToolResult { tool_call_id, content, is_error, cache } => {
            let mut inner = Vec::with_capacity(content.len());
            for c in content {
                if let Some(v) = write_block(c, markers, notes)? {
                    inner.push(v);
                }
            }
            let mut v = json!({ "type": "tool_result", "tool_use_id": tool_call_id, "content": inner });
            if *is_error {
                v["is_error"] = json!(true);
            }
            marked(v, cache, notes)
        }
        Block::Reasoning { redacted: Some(data), .. } => json!({ "type": "redacted_thinking", "data": data }),
        // Only a block Anthropic signed can be given back to it.
        Block::Reasoning { text, signature: Some(sig), .. } => {
            json!({ "type": "thinking", "thinking": text, "signature": sig })
        }
        Block::Reasoning { .. } => return Ok(None),
        Block::Provider { family, raw } if family == FAMILY || family.is_empty() => raw.clone(),
        Block::Provider { .. } => return Ok(None),
    }))
}

// --- client half: reading a caller's request -----------------------------------------------------

/// Client half: reads a Messages request.
pub fn parse_request(body: &[u8]) -> Result<Request, DialectError> {
    let mut top = object(body)?;
    let mut req =
        Request { model: take_string(&mut top, "model").unwrap_or_default(), stream_usage: true, ..Default::default() };

    req.system = match top.shift_remove("system") {
        Some(Value::String(s)) if !s.is_empty() => vec![Block::text(s)],
        Some(Value::Array(blocks)) => blocks.iter().map(read_block).collect(),
        _ => Vec::new(),
    };
    match top.shift_remove("messages") {
        Some(Value::Array(messages)) => {
            for m in &messages {
                let role = match str_of(m, "role") {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    Some(other) => return Err(DialectError::invalid(format!("\"{other}\" is not a message role"))),
                    None => return Err(DialectError::invalid("every message needs a role")),
                };
                req.messages.push(Message { role, content: read_content(m.get("content")) });
            }
        }
        _ => return Err(DialectError::invalid("messages is required and must be a list")),
    }

    for tool in top.shift_remove("tools").and_then(|v| v.as_array().cloned()).unwrap_or_default() {
        match str_of(&tool, "type") {
            None | Some("custom") => req.tools.push(Tool::Function {
                name: str_of(&tool, "name").unwrap_or_default().to_owned(),
                description: str_of(&tool, "description").map(str::to_owned),
                parameters: tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                strict: tool.get("strict").and_then(Value::as_bool),
                cache: cache_from(tool.get("cache_control")),
            }),
            // A tool Anthropic defines or runs: passed by name, so a new version needs no change here.
            Some(_) => req.tools.push(Tool::Provider { family: FAMILY.into(), definition: tool }),
        }
    }
    if let Some(choice) = top.shift_remove("tool_choice").filter(|c| !c.is_null()) {
        req.tool_choice = Some(match str_of(&choice, "type") {
            Some("auto") => ToolChoice::Auto,
            Some("any") => ToolChoice::Required,
            Some("none") => ToolChoice::None,
            Some("tool") => match str_of(&choice, "name") {
                Some(name) => ToolChoice::Tool(name.to_owned()),
                None => return Err(DialectError::invalid("tool_choice of type tool needs a name")),
            },
            _ => return Err(DialectError::invalid("tool_choice.type must be auto, any, tool or none")),
        });
        if choice.get("disable_parallel_tool_use").and_then(Value::as_bool) == Some(true) {
            top.insert("parallel_tool_calls".into(), json!(false));
        }
    }

    req.stop = strings(top.shift_remove("stop_sequences"));
    req.stream = take_bool(&mut top, "stream").unwrap_or(false);
    let p = &mut req.params;
    p.max_output_tokens = take_u64(&mut top, "max_tokens");
    p.temperature = take_f64(&mut top, "temperature");
    p.top_p = take_f64(&mut top, "top_p");
    p.top_k = take_f64(&mut top, "top_k");
    if let Some(thinking) = top.shift_remove("thinking") {
        match str_of(&thinking, "type") {
            Some("enabled") => p.reasoning.budget_tokens = thinking.get("budget_tokens").and_then(Value::as_u64),
            Some("disabled") => p.reasoning.effort = Some("none".into()),
            _ => {}
        }
        match str_of(&thinking, "display") {
            Some("summarized") => p.reasoning.summary = Some(true),
            Some("omitted") => p.reasoning.summary = Some(false),
            _ => {}
        }
    }
    // `output_config` and `metadata` hold members the gateway governs next to members it does
    // not: the governed ones are taken out, the rest stays in `extra`.
    if let Some(Value::Object(mut config)) = top.shift_remove("output_config") {
        if let Some(Value::String(effort)) = config.shift_remove("effort") {
            p.reasoning.effort = Some(effort);
        }
        match config.get("format") {
            Some(format) if str_of(format, "type") == Some("json_schema") => {
                req.response_format = Some(ResponseFormat::JsonSchema {
                    name: None,
                    schema: format.get("schema").cloned().unwrap_or_else(|| json!({})),
                    strict: None,
                });
                config.shift_remove("format");
            }
            _ => {}
        }
        if !config.is_empty() {
            top.insert("output_config".into(), Value::Object(config));
        }
    }
    if let Some(Value::Object(mut metadata)) = top.shift_remove("metadata") {
        if let Some(Value::String(user)) = metadata.shift_remove("user_id") {
            req.user = Some(user).filter(|u| !u.is_empty());
        }
        if !metadata.is_empty() {
            top.insert("metadata".into(), Value::Object(metadata));
        }
    }
    req.gateway = top.shift_remove("gateway");
    req.extra = top;
    Ok(req)
}

// --- client half: writing an answer --------------------------------------------------------------

const OWN_STOP_REASONS: &[&str] =
    &["end_turn", "max_tokens", "stop_sequence", "tool_use", "pause_turn", "refusal", "model_context_window_exceeded"];

fn stop_reason(finish: Finish, native: &str) -> &str {
    if OWN_STOP_REASONS.contains(&native) {
        return native;
    }
    match finish {
        Finish::Stop => "end_turn",
        Finish::Length => "max_tokens",
        Finish::ToolCalls => "tool_use",
        Finish::ContentFilter => "refusal",
        Finish::Paused => "pause_turn",
    }
}

/// A block of an answer in Anthropic's shape, for a caller that speaks this dialect.
fn answer_block(b: &Block) -> Option<Value> {
    Some(match b {
        Block::Text { text, citations, .. } => {
            let mut v = json!({ "type": "text", "text": text });
            if let Some(c) = citations {
                v["citations"] = c.clone();
            }
            v
        }
        Block::ToolCall { id, name, arguments, .. } => {
            json!({ "type": "tool_use", "id": id, "name": name, "input": arguments_value(arguments).unwrap_or_else(|_| json!({})) })
        }
        Block::Reasoning { redacted: Some(data), .. } => json!({ "type": "redacted_thinking", "data": data }),
        // Reasoning of another provider has no signature; the member is still required.
        Block::Reasoning { text, signature, .. } => {
            json!({ "type": "thinking", "thinking": text, "signature": signature.as_deref().unwrap_or_default() })
        }
        Block::Provider { family, raw } if family == FAMILY => raw.clone(),
        Block::Provider { .. } | Block::Image { .. } | Block::Document { .. } | Block::ToolResult { .. } => {
            return None;
        }
    })
}

/// Client half: writes a `message` object.
pub fn write_response(response: &Response, stamp: &Stamp) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(stamp.id));
    out.insert("type".into(), json!("message"));
    out.insert("role".into(), json!("assistant"));
    out.insert("model".into(), json!(stamp.model));
    out.insert("content".into(), Value::Array(response.content.iter().filter_map(answer_block).collect()));
    out.insert("stop_reason".into(), json!(stop_reason(response.finish, &response.native_finish)));
    out.insert("stop_sequence".into(), Value::Null);
    out.insert("usage".into(), usage_anthropic(&response.usage.unwrap_or_default()));
    for (k, v) in &response.extra {
        if !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

struct Writer {
    stamp: Stamp,
}

impl Writer {
    fn event(out: &mut BytesMut, name: &str, data: Value) {
        write_event(out, name, data.to_string().as_bytes());
    }
}

impl ClientStream for Writer {
    fn finalize(&mut self, _cost: Option<Value>, _gateway: Option<Value>) {
        // Anthropic's shape has no place for either; the cost of a stream is in the request log.
    }

    fn write(&mut self, event: &Event, out: &mut BytesMut) {
        match event {
            Event::MessageStart { id, model } => {
                if self.stamp.id.is_empty() {
                    self.stamp.id = id.clone();
                }
                if self.stamp.model.is_empty() {
                    self.stamp.model = model.clone();
                }
                let message = json!({
                    "id": self.stamp.id, "type": "message", "role": "assistant", "model": self.stamp.model,
                    "content": [], "stop_reason": null, "stop_sequence": null,
                    "usage": usage_anthropic(&Usage::default()),
                });
                Self::event(out, "message_start", json!({ "type": "message_start", "message": message }));
            }
            Event::ContentStart { index, block } => {
                let content_block = match block {
                    BlockStart::Text => json!({ "type": "text", "text": "" }),
                    BlockStart::Reasoning => json!({ "type": "thinking", "thinking": "", "signature": "" }),
                    BlockStart::ToolCall { id, name } => {
                        json!({ "type": "tool_use", "id": id, "name": name, "input": {} })
                    }
                    BlockStart::Provider { raw, .. } => raw.clone(),
                };
                Self::event(
                    out,
                    "content_block_start",
                    json!({ "type": "content_block_start", "index": index, "content_block": content_block }),
                );
            }
            Event::ContentDelta { index, delta } => {
                let delta = match delta {
                    Delta::Text(t) => json!({ "type": "text_delta", "text": t }),
                    Delta::Reasoning(t) => json!({ "type": "thinking_delta", "thinking": t }),
                    Delta::ToolArguments(a) => json!({ "type": "input_json_delta", "partial_json": a }),
                    Delta::Signature(s) => json!({ "type": "signature_delta", "signature": s }),
                    Delta::Citation(c) => json!({ "type": "citations_delta", "citation": c }),
                    Delta::Provider(raw) => raw.clone(),
                };
                Self::event(
                    out,
                    "content_block_delta",
                    json!({ "type": "content_block_delta", "index": index, "delta": delta }),
                );
            }
            Event::ContentStop { index } => {
                Self::event(out, "content_block_stop", json!({ "type": "content_block_stop", "index": index }));
            }
            Event::MessageStop { finish, native_finish, usage } => {
                let delta = json!({ "stop_reason": stop_reason(*finish, native_finish), "stop_sequence": null });
                let usage = usage_anthropic(&usage.unwrap_or_default());
                Self::event(out, "message_delta", json!({ "type": "message_delta", "delta": delta, "usage": usage }));
                Self::event(out, "message_stop", json!({ "type": "message_stop" }));
            }
            Event::Ping => Self::event(out, "ping", json!({ "type": "ping" })),
            Event::Error { message, .. } => {
                Self::event(
                    out,
                    "error",
                    json!({ "type": "error", "error": { "type": "api_error", "message": message } }),
                );
            }
        }
    }
}

/// Client half: a writer of Anthropic's stream events. Usage is always part of them.
pub fn client_stream(stamp: Stamp, _include_usage: bool) -> Box<dyn ClientStream> {
    Box::new(Writer { stamp })
}

// --- provider half: writing Anthropic's request --------------------------------------------------

fn note(notes: &mut Notes, name: &str) {
    if !notes.dropped.iter().any(|d| d == name) {
        notes.dropped.push(name.to_owned());
    }
}

/// The conversation in Anthropic's terms. Tool results are grouped: Anthropic expects all
/// results of one assistant turn in a single user message, so neighbouring messages of the
/// same Anthropic role become one, with the results first.
fn messages_out(req: &Request, markers: bool, notes: &mut Notes) -> Result<Vec<Value>, DialectError> {
    let mut merged: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for m in &req.messages {
        let role = if m.role == Role::Assistant { "assistant" } else { "user" };
        let mut blocks = Vec::with_capacity(m.content.len());
        for b in &m.content {
            if let Some(v) = write_block(b, markers, notes)? {
                blocks.push(v);
            }
        }
        match merged.last_mut() {
            Some((last, content)) if *last == role => content.extend(blocks),
            _ => merged.push((role, blocks)),
        }
    }
    Ok(merged
        .into_iter()
        .map(|(role, mut content)| {
            if role == "user" {
                // A stable sort: results first, everything else in its order.
                content.sort_by_key(|b| str_of(b, "type") != Some("tool_result"));
            }
            json!({ "role": role, "content": content })
        })
        .collect())
}

fn lowest_effort(profile: &Profile) -> Option<&String> {
    profile.reasoning_efforts.iter().find(|e| e.as_str() != "none")
}

/// Thinking, in the form this model takes (concept section 8.3).
fn reasoning_out(
    req: &Request,
    profile: &Profile,
    body: &mut Map<String, Value>,
    config: &mut Map<String, Value>,
    notes: &mut Notes,
) {
    let r = &req.params.reasoning;
    let off = r.effort.as_deref() == Some("none");
    match profile.reasoning {
        ReasoningStyle::Budget => {
            if off {
                body.insert("thinking".into(), json!({ "type": "disabled" }));
            } else if let Some(budget) = r.budget_tokens {
                body.insert("thinking".into(), json!({ "type": "enabled", "budget_tokens": budget }));
            }
        }
        ReasoningStyle::Adaptive | ReasoningStyle::AlwaysOn => {
            if off {
                match profile.thinking_off {
                    ThinkingOff::Disabled => drop(body.insert("thinking".into(), json!({ "type": "disabled" }))),
                    ThinkingOff::BetweenTools => {
                        drop(body.insert("thinking".into(), json!({ "type": "between_tools" })))
                    }
                    ThinkingOff::Unsupported => {
                        if let Some(low) = lowest_effort(profile) {
                            notes.adjusted.push(format!(
                                "thinking cannot be switched off on this model; the effort {low} was used"
                            ));
                            config.insert("effort".into(), json!(low));
                        }
                    }
                }
                return;
            }
            // Recent models return no reasoning text unless asked: a caller who steers the
            // reasoning gets its summary, unless they said they do not want it.
            let wanted = r.effort.is_some() || r.summary == Some(true);
            if wanted {
                let thinking = if r.summary == Some(false) {
                    json!({ "type": "adaptive" })
                } else {
                    json!({ "type": "adaptive", "display": "summarized" })
                };
                body.insert("thinking".into(), thinking);
            }
            if let Some(effort) = &r.effort {
                config.insert("effort".into(), json!(effort));
            }
        }
        ReasoningStyle::None | ReasoningStyle::Effort => {}
    }
}

/// Provider half: writes a Messages request for an Anthropic model.
pub fn build_request(request: &Request, profile: &Profile, notes: &mut Notes) -> Result<Value, DialectError> {
    let markers = profile.cache_markers;
    let mut body = Map::new();
    body.insert("model".into(), json!(profile.upstream_model));
    // Anthropic requires the limit.
    body.insert("max_tokens".into(), json!(request.params.max_output_tokens.unwrap_or(profile.default_max_output)));
    if profile.cache_auto {
        body.insert("cache_control".into(), json!({ "type": "ephemeral" }));
    }
    if !request.system.is_empty() {
        let mut system = Vec::with_capacity(request.system.len());
        for b in &request.system {
            if let Some(v) = write_block(b, markers, notes)? {
                system.push(v);
            }
        }
        body.insert("system".into(), Value::Array(system));
    }
    body.insert("messages".into(), Value::Array(messages_out(request, markers, notes)?));

    if !request.tools.is_empty() {
        let mut tools = Vec::with_capacity(request.tools.len());
        for t in &request.tools {
            match t {
                Tool::Function { name, description, parameters, strict, cache } => {
                    let mut tool = Map::new();
                    tool.insert("name".into(), json!(name));
                    if let Some(d) = description {
                        tool.insert("description".into(), json!(d));
                    }
                    tool.insert("input_schema".into(), parameters.clone());
                    if let Some(s) = strict {
                        tool.insert("strict".into(), json!(s));
                    }
                    match cache {
                        Some(c) if markers => drop(tool.insert("cache_control".into(), cache_to(c))),
                        Some(_) => note(notes, "cache_control"),
                        None => {}
                    }
                    tools.push(Value::Object(tool));
                }
                Tool::Provider { family, definition } if family == FAMILY || family.is_empty() => {
                    tools.push(definition.clone())
                }
                Tool::Provider { family, .. } => {
                    return Err(DialectError::unsupported(format!(
                        "tools defined by {family} are not available on this model"
                    )));
                }
            }
        }
        body.insert("tools".into(), Value::Array(tools));
    }
    let serial = request.extra.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false);
    let mut choice = match &request.tool_choice {
        None => None,
        Some(ToolChoice::Auto) => Some(json!({ "type": "auto" })),
        Some(ToolChoice::Required) => Some(json!({ "type": "any" })),
        Some(ToolChoice::None) => Some(json!({ "type": "none" })),
        Some(ToolChoice::Tool(name)) => Some(json!({ "type": "tool", "name": name })),
    };
    if serial && !request.tools.is_empty() {
        let c = choice.get_or_insert_with(|| json!({ "type": "auto" }));
        if str_of(c, "type") != Some("none") {
            c["disable_parallel_tool_use"] = json!(true);
        }
    }
    if let Some(c) = choice {
        body.insert("tool_choice".into(), c);
    }

    let mut config = Map::new();
    match &request.response_format {
        None | Some(ResponseFormat::Text) => {}
        Some(ResponseFormat::JsonObject) => {
            return Err(DialectError::unsupported(
                "this model needs a JSON schema for structured output; JSON mode without a schema is not available",
            ));
        }
        Some(ResponseFormat::JsonSchema { schema, .. }) => {
            config.insert("format".into(), json!({ "type": "json_schema", "schema": schema }));
        }
    }
    if !request.stop.is_empty() {
        body.insert("stop_sequences".into(), json!(request.stop));
    }
    let p = &request.params;
    for (name, value) in [("temperature", p.temperature), ("top_p", p.top_p), ("top_k", p.top_k)] {
        if let Some(v) = value {
            body.insert(name.into(), json!(v));
        }
    }
    if p.frequency_penalty.is_some() {
        note(notes, "frequency_penalty");
    }
    if p.presence_penalty.is_some() {
        note(notes, "presence_penalty");
    }
    reasoning_out(request, profile, &mut body, &mut config, notes);
    if request.stream {
        body.insert("stream".into(), json!(true));
    }

    for (key, value) in &request.extra {
        match key.as_str() {
            "parallel_tool_calls" => {}
            "n" if value.as_u64().is_some_and(|n| n > 1) => {
                return Err(DialectError::unsupported(
                    "this model gives one answer per request (n > 1 is not available)",
                ));
            }
            "n" => {}
            "logprobs" if value.as_bool() == Some(true) => {
                return Err(DialectError::unsupported("log-probabilities are not available on this model"));
            }
            "top_logprobs" if !value.is_null() => {
                return Err(DialectError::unsupported("log-probabilities are not available on this model"));
            }
            // What is left of a Messages request's own objects goes back where it was.
            "output_config" => {
                for (k, v) in value.as_object().into_iter().flatten() {
                    config.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            "metadata" if value.is_object() => drop(body.insert("metadata".into(), value.clone())),
            _ => note(notes, key),
        }
    }
    if !config.is_empty() {
        body.insert("output_config".into(), Value::Object(config));
    }
    if let Some(Value::Object(options)) = request.provider_options.get(FAMILY) {
        for (key, value) in options {
            body.insert(key.clone(), value.clone());
        }
    }
    Ok(Value::Object(body))
}

// --- provider half: reading Anthropic's answer ---------------------------------------------------

fn finish_of(reason: &str) -> Finish {
    match reason {
        "max_tokens" | "model_context_window_exceeded" => Finish::Length,
        "tool_use" => Finish::ToolCalls,
        "refusal" => Finish::ContentFilter,
        "pause_turn" => Finish::Paused,
        _ => Finish::Stop,
    }
}

/// Provider half: reads a `message` object.
pub fn read_response(body: &[u8], _profile: &Profile) -> Result<Response, DialectError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| DialectError::bad_response(format!("the provider's answer is not JSON: {e}")))?;
    let Value::Object(mut top) = v else {
        return Err(DialectError::bad_response("the provider's answer is not an object"));
    };
    let content = match top.shift_remove("content") {
        Some(Value::Array(blocks)) => blocks.iter().map(read_block).collect(),
        _ => return Err(DialectError::bad_response("the provider's answer has no content")),
    };
    let mut usage = Usage::default();
    let has_usage = merge_usage_anthropic(top.shift_remove("usage").as_ref(), &mut usage);
    let native = take_string(&mut top, "stop_reason").unwrap_or_default();
    let id = take_string(&mut top, "id").unwrap_or_default();
    let model = take_string(&mut top, "model").unwrap_or_default();
    for known in ["type", "role", "stop_sequence"] {
        top.shift_remove(known);
    }
    Ok(Response {
        id,
        model,
        content,
        finish: finish_of(&native),
        native_finish: native,
        usage: has_usage.then_some(usage),
        // What is left has no neutral meaning: `stop_details`, `container`, and what comes next.
        extra: top,
    })
}

struct Reader {
    usage: Usage,
    has_usage: bool,
    stop_reason: Option<String>,
    /// Which open blocks are provider blocks: their deltas are passed on as they are.
    provider: BTreeMap<u32, bool>,
    stopped: bool,
}

impl ProviderStream for Reader {
    fn read(&mut self, event: &str, data: &[u8], out: &mut Vec<Event>) {
        if self.stopped {
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(data) else { return };
        let kind = if event.is_empty() { str_of(&v, "type").unwrap_or_default() } else { event };
        let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
        match kind {
            "message_start" => {
                let message = v.get("message").cloned().unwrap_or(Value::Null);
                self.has_usage |= merge_usage_anthropic(message.get("usage"), &mut self.usage);
                out.push(Event::MessageStart {
                    id: str_of(&message, "id").unwrap_or_default().to_owned(),
                    model: str_of(&message, "model").unwrap_or_default().to_owned(),
                });
            }
            "content_block_start" => {
                let block = v.get("content_block").cloned().unwrap_or(Value::Null);
                let start = match str_of(&block, "type") {
                    Some("text") => BlockStart::Text,
                    Some("thinking") => BlockStart::Reasoning,
                    Some("tool_use") => BlockStart::ToolCall {
                        id: str_of(&block, "id").unwrap_or_default().to_owned(),
                        name: str_of(&block, "name").unwrap_or_default().to_owned(),
                    },
                    _ => BlockStart::Provider { family: FAMILY.into(), raw: block },
                };
                self.provider.insert(index, matches!(start, BlockStart::Provider { .. }));
                out.push(Event::ContentStart { index, block: start });
            }
            "content_block_delta" => {
                let raw = v.get("delta").cloned().unwrap_or(Value::Null);
                let delta = if self.provider.get(&index) == Some(&true) {
                    Delta::Provider(raw)
                } else {
                    match str_of(&raw, "type") {
                        Some("text_delta") => Delta::Text(str_of(&raw, "text").unwrap_or_default().to_owned()),
                        Some("thinking_delta") => {
                            Delta::Reasoning(str_of(&raw, "thinking").unwrap_or_default().to_owned())
                        }
                        Some("signature_delta") => {
                            Delta::Signature(str_of(&raw, "signature").unwrap_or_default().to_owned())
                        }
                        Some("input_json_delta") => {
                            let fragment = str_of(&raw, "partial_json").unwrap_or_default();
                            if fragment.is_empty() {
                                return;
                            }
                            Delta::ToolArguments(fragment.to_owned())
                        }
                        Some("citations_delta") => Delta::Citation(raw.get("citation").cloned().unwrap_or(Value::Null)),
                        _ => Delta::Provider(raw),
                    }
                };
                out.push(Event::ContentDelta { index, delta });
            }
            "content_block_stop" => {
                self.provider.remove(&index);
                out.push(Event::ContentStop { index });
            }
            "message_delta" => {
                self.has_usage |= merge_usage_anthropic(v.get("usage"), &mut self.usage);
                if let Some(reason) = v.get("delta").and_then(|d| str_of(d, "stop_reason")) {
                    self.stop_reason = Some(reason.to_owned());
                }
            }
            "message_stop" => self.finish(out),
            "ping" => out.push(Event::Ping),
            "error" => {
                self.stopped = true;
                let error = v.get("error").cloned().unwrap_or(Value::Null);
                out.push(Event::Error {
                    kind: str_of(&error, "type").unwrap_or("upstream_error").to_owned(),
                    message: str_of(&error, "message").unwrap_or("the provider reported an error").to_owned(),
                });
            }
            // An event type of the future has no neutral meaning; the stream goes on.
            _ => {}
        }
    }

    fn finish(&mut self, out: &mut Vec<Event>) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        // Only a stream that began can end.
        if !self.has_usage && self.stop_reason.is_none() && self.provider.is_empty() && out.is_empty() {
            return;
        }
        let native = self.stop_reason.clone().unwrap_or_default();
        out.push(Event::MessageStop {
            finish: finish_of(&native),
            native_finish: native,
            usage: self.has_usage.then_some(self.usage),
        });
    }
}

/// Provider half: a reader of Anthropic's stream events.
pub fn provider_stream(_profile: &Profile) -> Box<dyn ProviderStream> {
    Box::new(Reader {
        usage: Usage::default(),
        has_usage: false,
        stop_reason: None,
        provider: BTreeMap::new(),
        stopped: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{Params, Reasoning};
    use crate::openai;

    fn parse(v: Value) -> Request {
        parse_request(v.to_string().as_bytes()).unwrap()
    }

    fn profile(reasoning: ReasoningStyle) -> Profile {
        Profile {
            family: FAMILY.into(),
            upstream_model: "claude-x".into(),
            reasoning,
            reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
            cache_markers: true,
            default_max_output: 8000,
            ..Default::default()
        }
    }

    fn build(req: &Request, p: &Profile) -> (Value, Notes) {
        let mut notes = Notes::default();
        let v = build_request(req, p, &mut notes).unwrap();
        (v, notes)
    }

    fn full_request() -> Value {
        json!({
            "model": "claude-x", "max_tokens": 1024,
            "system": [{ "type": "text", "text": "static prefix", "cache_control": { "type": "ephemeral", "ttl": "1h" } }],
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "look at this" },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } },
                    { "type": "document", "source": { "type": "text", "media_type": "text/plain", "data": "doc" }, "title": "T", "citations": { "enabled": true } }
                ] },
                { "role": "assistant", "content": [
                    { "type": "thinking", "thinking": "let me think", "signature": "sig==" },
                    { "type": "redacted_thinking", "data": "enc" },
                    { "type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": { "query": "q" } },
                    { "type": "web_search_tool_result", "tool_use_id": "srv_1", "content": [] },
                    { "type": "text", "text": "I will call a tool.", "citations": [{ "type": "char_location" }] },
                    { "type": "tool_use", "id": "toolu_1", "name": "look", "input": { "q": 1, "deep": { "a": [1, 2] } } }
                ] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_1", "content": [{ "type": "text", "text": "found" }], "is_error": true },
                    { "type": "text", "text": "continue" }
                ] }
            ],
            "tools": [
                { "name": "look", "description": "d", "input_schema": { "type": "object" }, "strict": true, "cache_control": { "type": "ephemeral" } },
                { "type": "web_search_20260209", "name": "web_search", "max_uses": 3 }
            ],
            "tool_choice": { "type": "tool", "name": "look", "disable_parallel_tool_use": true },
            "stop_sequences": ["END"], "stream": true, "temperature": 0.5, "top_k": 40,
            "thinking": { "type": "adaptive", "display": "summarized" },
            "output_config": { "effort": "high", "format": { "type": "json_schema", "schema": { "type": "object" } }, "task_budget": { "type": "tokens", "total": 1000 } },
            "metadata": { "user_id": "u_1" },
            "gateway": { "trace_id": "t" },
            "context_management": { "edits": [] }
        })
    }

    #[test]
    fn a_request_is_read_into_neutral_terms() {
        let req = parse(full_request());
        assert_eq!(
            req.system,
            [Block::Text {
                text: "static prefix".into(),
                citations: None,
                cache: Some(Cache { ttl: Some("1h".into()) })
            }]
        );
        assert_eq!(req.messages.len(), 3);
        assert!(
            matches!(&req.messages[0].content[2], Block::Document { body, .. } if body["title"] == "T" && !body.contains_key("type"))
        );
        let a = &req.messages[1].content;
        assert_eq!(
            a[0],
            Block::Reasoning { text: "let me think".into(), signature: Some("sig==".into()), redacted: None }
        );
        assert_eq!(a[1], Block::Reasoning { text: String::new(), signature: None, redacted: Some("enc".into()) });
        assert!(
            matches!(&a[2], Block::Provider { family, raw } if family == FAMILY && raw["type"] == "server_tool_use")
        );
        assert!(matches!(&a[4], Block::Text { citations: Some(_), .. }));
        assert_eq!(
            a[5],
            Block::ToolCall {
                id: "toolu_1".into(),
                name: "look".into(),
                arguments: r#"{"q":1,"deep":{"a":[1,2]}}"#.into(),
                cache: None
            }
        );
        assert!(
            matches!(&req.messages[2].content[0], Block::ToolResult { tool_call_id, is_error: true, .. } if tool_call_id == "toolu_1")
        );
        assert_eq!(req.messages[2].role, Role::User, "results stay in the user turn they came in");
        assert!(matches!(&req.tools[0], Tool::Function { strict: Some(true), cache: Some(_), .. }));
        assert!(matches!(&req.tools[1], Tool::Provider { family, .. } if family == FAMILY));
        assert_eq!(req.tool_choice, Some(ToolChoice::Tool("look".into())));
        assert_eq!(req.stop, ["END"]);
        assert!(req.stream && req.stream_usage);
        assert_eq!(
            (req.params.max_output_tokens, req.params.temperature, req.params.top_k),
            (Some(1024), Some(0.5), Some(40.0))
        );
        assert_eq!(
            req.params.reasoning,
            Reasoning { effort: Some("high".into()), budget_tokens: None, summary: Some(true) }
        );
        assert!(matches!(req.response_format, Some(ResponseFormat::JsonSchema { .. })));
        assert_eq!(req.user.as_deref(), Some("u_1"));
        assert_eq!(req.gateway, Some(json!({ "trace_id": "t" })));
        // What the gateway does not govern stays, in its order, for Anthropic to get back.
        assert_eq!(
            req.extra.keys().collect::<Vec<_>>(),
            ["context_management", "parallel_tool_calls", "output_config"]
        );
        assert_eq!(req.extra["output_config"], json!({ "task_budget": { "type": "tokens", "total": 1000 } }));

        let old = parse(
            json!({ "model": "m", "max_tokens": 5, "messages": [{ "role": "user", "content": "hi" }], "system": "be brief",
                                "thinking": { "type": "enabled", "budget_tokens": 2048 } }),
        );
        assert_eq!(old.params.reasoning.budget_tokens, Some(2048));
        assert_eq!((old.system.len(), &old.messages[0].content), (1, &vec![Block::text("hi")]));
        let off = parse(json!({ "messages": [], "thinking": { "type": "disabled" } }));
        assert_eq!(off.params.reasoning.effort.as_deref(), Some("none"));
    }

    #[test]
    fn malformed_requests_are_errors_not_panics() {
        for body in [
            &b"nope"[..],
            b"42",
            b"{}",
            br#"{"messages":{}}"#,
            br#"{"messages":[{"content":"x"}]}"#,
            br#"{"messages":[{"role":"system","content":"x"}]}"#,
            br#"{"messages":[],"tool_choice":{"type":"tool"}}"#,
            br#"{"messages":[],"tool_choice":{"type":"sometimes"}}"#,
        ] {
            assert!(
                matches!(parse_request(body), Err(DialectError::InvalidRequest(_))),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
        // Odd shapes in optional places are tolerated.
        let r = parse(
            json!({ "messages": [{ "role": "user", "content": [42, { "no": "type" }, { "type": "image", "source": { "type": "file", "file_id": "f" } }] }],
                              "tools": "x", "thinking": 3, "output_config": [], "metadata": "m", "system": 9 }),
        );
        assert_eq!(r.messages[0].content.len(), 3);
        assert!(r.messages[0].content.iter().all(|b| matches!(b, Block::Provider { .. })));
    }

    #[test]
    fn a_messages_request_survives_the_neutral_model() {
        // Anthropic → neutral → Anthropic: the meaning is the same, block for block.
        let original = full_request();
        let (v, notes) = build(&parse(original.clone()), &profile(ReasoningStyle::Adaptive));
        // A field the gateway does not model has no place in the neutral model: on a
        // translating path it is left out and named (`provider_options.anthropic` carries
        // such fields on purpose). The Anthropic interface itself never comes this way.
        assert_eq!(notes.dropped, ["context_management"]);
        assert_eq!(v["system"], original["system"]);
        assert_eq!(v["messages"], original["messages"]);
        assert_eq!(v["tools"], original["tools"]);
        assert_eq!(v["tool_choice"], original["tool_choice"]);
        assert_eq!(v["thinking"], original["thinking"]);
        assert_eq!(v["output_config"], original["output_config"]);
        assert_eq!(v["stop_sequences"], original["stop_sequences"]);
        assert_eq!(
            (v["max_tokens"].as_u64(), v["stream"].as_bool(), v["top_k"].as_f64()),
            (Some(1024), Some(true), Some(40.0))
        );
        assert!(
            v.get("gateway").is_none() && v.get("context_management").is_none(),
            "extras are the caller's dialect, not this path's"
        );
        // Twice the same bytes.
        assert_eq!(v.to_string(), build(&parse(original), &profile(ReasoningStyle::Adaptive)).0.to_string());
    }

    #[test]
    fn an_openai_conversation_becomes_a_messages_request() {
        let req = openai::parse_request(
            json!({
                "model": "@synth",
                "messages": [
                    { "role": "system", "content": [{ "type": "text", "text": "static", "cache_control": { "type": "ephemeral" } }] },
                    { "role": "user", "content": "weather in three cities?" },
                    { "role": "assistant", "content": null,
                      "provider_specific_fields": { "thinking_blocks": [{ "type": "thinking", "thinking": "three calls", "signature": "sig==" }] },
                      "tool_calls": [
                        { "id": "c1", "type": "function", "function": { "name": "weather", "arguments": "{\"city\":\"Sofia\"}" } },
                        { "id": "c2", "type": "function", "function": { "name": "weather", "arguments": "{\"city\":\"Wien\"}" } },
                        { "id": "c3", "type": "function", "function": { "name": "weather", "arguments": "" } }
                    ] },
                    { "role": "tool", "tool_call_id": "c1", "content": "sunny" },
                    { "role": "tool", "tool_call_id": "c2", "content": "rain" },
                    { "role": "tool", "tool_call_id": "c3", "content": "fog" },
                    { "role": "user", "content": "and tomorrow?" }
                ],
                "tools": [{ "type": "function", "function": { "name": "weather", "description": "w", "parameters": { "type": "object" } } },
                          { "type": "web_search_20260209", "name": "web_search" }],
                "tool_choice": "required", "parallel_tool_calls": false,
                "response_format": { "type": "json_schema", "json_schema": { "name": "out", "schema": { "type": "object" } } },
                "stop": ["END"], "reasoning_effort": "high", "frequency_penalty": 0.5, "seed": 7, "n": 1
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        let (v, notes) = build(&req, &profile(ReasoningStyle::Adaptive));
        assert_eq!(
            v,
            json!({
                "model": "claude-x",
                "max_tokens": 8000,
                "system": [{ "type": "text", "text": "static", "cache_control": { "type": "ephemeral" } }],
                "messages": [
                    { "role": "user", "content": [{ "type": "text", "text": "weather in three cities?" }] },
                    { "role": "assistant", "content": [
                        { "type": "thinking", "thinking": "three calls", "signature": "sig==" },
                        { "type": "tool_use", "id": "c1", "name": "weather", "input": { "city": "Sofia" } },
                        { "type": "tool_use", "id": "c2", "name": "weather", "input": { "city": "Wien" } },
                        { "type": "tool_use", "id": "c3", "name": "weather", "input": {} }
                    ] },
                    // Three tool messages and the user's next turn: one user message, results first.
                    { "role": "user", "content": [
                        { "type": "tool_result", "tool_use_id": "c1", "content": [{ "type": "text", "text": "sunny" }] },
                        { "type": "tool_result", "tool_use_id": "c2", "content": [{ "type": "text", "text": "rain" }] },
                        { "type": "tool_result", "tool_use_id": "c3", "content": [{ "type": "text", "text": "fog" }] },
                        { "type": "text", "text": "and tomorrow?" }
                    ] }
                ],
                "tools": [
                    { "name": "weather", "description": "w", "input_schema": { "type": "object" } },
                    { "type": "web_search_20260209", "name": "web_search" }
                ],
                "tool_choice": { "type": "any", "disable_parallel_tool_use": true },
                "stop_sequences": ["END"],
                "thinking": { "type": "adaptive", "display": "summarized" },
                "output_config": { "format": { "type": "json_schema", "schema": { "type": "object" } }, "effort": "high" }
            })
        );
        assert_eq!(notes.dropped, ["frequency_penalty", "seed"]);
    }

    #[test]
    fn thinking_is_written_for_each_kind_of_model() {
        let with = |effort: Option<&str>, budget: Option<u64>, summary: Option<bool>| Request {
            params: Params {
                reasoning: Reasoning { effort: effort.map(str::to_owned), budget_tokens: budget, summary },
                ..Default::default()
            },
            ..Default::default()
        };
        let adaptive = profile(ReasoningStyle::Adaptive);
        let (v, _) = build(&with(Some("low"), None, Some(false)), &adaptive);
        assert_eq!(
            (&v["thinking"], &v["output_config"]),
            (&json!({ "type": "adaptive" }), &json!({ "effort": "low" }))
        );
        let (v, _) = build(&with(None, None, Some(true)), &adaptive);
        assert_eq!(v["thinking"], json!({ "type": "adaptive", "display": "summarized" }));
        assert!(v.get("output_config").is_none());
        let (v, _) = build(&with(None, None, None), &adaptive);
        assert!(v.get("thinking").is_none(), "no opinion, no field");
        let (v, _) = build(&with(Some("none"), None, None), &adaptive);
        assert_eq!(v["thinking"], json!({ "type": "disabled" }));
        let (v, _) = build(
            &with(Some("none"), None, None),
            &Profile { thinking_off: ThinkingOff::BetweenTools, ..adaptive.clone() },
        );
        assert_eq!(v["thinking"], json!({ "type": "between_tools" }));
        let always =
            Profile { reasoning: ReasoningStyle::AlwaysOn, thinking_off: ThinkingOff::Unsupported, ..adaptive };
        let (v, notes) = build(&with(Some("none"), None, None), &always);
        assert_eq!((v.get("thinking"), &v["output_config"]), (None, &json!({ "effort": "low" })));
        assert_eq!(notes.adjusted.len(), 1);

        let budget = profile(ReasoningStyle::Budget);
        let (v, _) = build(&with(None, Some(4096), None), &budget);
        assert_eq!(v["thinking"], json!({ "type": "enabled", "budget_tokens": 4096 }));
        let (v, _) = build(&with(Some("none"), Some(4096), None), &budget);
        assert_eq!(v["thinking"], json!({ "type": "disabled" }));
        let (v, _) = build(&with(Some("high"), None, None), &profile(ReasoningStyle::None));
        assert!(v.get("thinking").is_none() && v.get("output_config").is_none());
    }

    #[test]
    fn what_anthropic_cannot_deliver_is_refused_and_what_it_cannot_use_is_named() {
        let refuse = |req: Request| {
            matches!(
                build_request(&req, &profile(ReasoningStyle::None), &mut Notes::default()),
                Err(DialectError::Unsupported(_))
            )
        };
        let extra =
            |k: &str, v: Value| Request { extra: [(k.to_owned(), v)].into_iter().collect(), ..Default::default() };
        assert!(refuse(extra("n", json!(2))));
        assert!(refuse(extra("logprobs", json!(true))));
        assert!(refuse(extra("top_logprobs", json!(3))));
        assert!(refuse(Request { response_format: Some(ResponseFormat::JsonObject), ..Default::default() }));
        assert!(refuse(Request {
            tools: vec![Tool::Provider { family: "openai".into(), definition: json!({}) }],
            ..Default::default()
        }));
        assert!(!refuse(extra("n", json!(1))) && !refuse(extra("logprobs", json!(false))));
        let bad_args = Request {
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![Block::ToolCall {
                    id: "c".into(),
                    name: "f".into(),
                    arguments: "{oops".into(),
                    cache: None,
                }],
            }],
            ..Default::default()
        };
        assert!(matches!(
            build_request(&bad_args, &profile(ReasoningStyle::None), &mut Notes::default()),
            Err(DialectError::InvalidRequest(_))
        ));

        // Without cache markers (never the case for Anthropic itself, but the profile decides).
        let marked = Request {
            system: vec![Block::Text { text: "s".into(), citations: None, cache: Some(Cache::default()) }],
            ..Default::default()
        };
        let (v, notes) = build(&marked, &Profile { cache_markers: false, ..profile(ReasoningStyle::None) });
        assert_eq!(
            (&v["system"], notes.dropped),
            (&json!([{ "type": "text", "text": "s" }]), vec!["cache_control".to_owned()])
        );
        let (v, _) = build(&marked, &Profile { cache_auto: true, ..profile(ReasoningStyle::None) });
        assert_eq!(v["cache_control"], json!({ "type": "ephemeral" }));

        // Reasoning another provider produced has no signature and is not sent; its blocks neither.
        let foreign = Request {
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![
                    Block::Reasoning { text: "unsigned".into(), signature: None, redacted: None },
                    Block::Provider { family: "openai".into(), raw: json!({ "x": 1 }) },
                    Block::text("answer"),
                ],
            }],
            provider_options: [(FAMILY.to_owned(), json!({ "service_tier": "auto" }))].into_iter().collect(),
            ..Default::default()
        };
        let (v, _) = build(&foreign, &profile(ReasoningStyle::None));
        assert_eq!(v["messages"][0]["content"], json!([{ "type": "text", "text": "answer" }]));
        assert_eq!(v["service_tier"], "auto");
    }

    fn answer() -> Value {
        json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-x-20260101",
            "content": [
                { "type": "thinking", "thinking": "hm", "signature": "sig==" },
                { "type": "text", "text": "Paris", "citations": [{ "type": "char_location" }] },
                { "type": "tool_use", "id": "toolu_1", "name": "f", "input": { "a": 1 } },
                { "type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {} }
            ],
            "stop_reason": "tool_use", "stop_sequence": null,
            "usage": { "input_tokens": 10, "cache_read_input_tokens": 5, "cache_creation_input_tokens": 3, "output_tokens": 7,
                       "cache_creation": { "ephemeral_5m_input_tokens": 1, "ephemeral_1h_input_tokens": 2 },
                       "server_tool_use": { "web_search_requests": 2 } },
            "container": { "id": "c_1" }
        })
    }

    #[test]
    fn an_anthropic_answer_is_read_and_written_back_the_same() {
        let r = read_response(answer().to_string().as_bytes(), &profile(ReasoningStyle::None)).unwrap();
        assert_eq!(
            (r.id.as_str(), r.model.as_str(), r.finish, r.native_finish.as_str()),
            ("msg_1", "claude-x-20260101", Finish::ToolCalls, "tool_use")
        );
        let u = r.usage.unwrap();
        assert_eq!(
            (
                u.input_tokens,
                u.cache_read_tokens,
                u.cache_write_tokens,
                u.cache_write_5m_tokens,
                u.cache_write_1h_tokens,
                u.output_tokens,
                u.server_tool_calls
            ),
            (10, 5, 3, 1, 2, 7, 2)
        );
        assert_eq!(r.extra.keys().collect::<Vec<_>>(), ["container"]);

        let stamp = Stamp { id: r.id.clone(), model: r.model.clone(), ..Default::default() };
        let back = write_response(&r, &stamp);
        assert_eq!(back["content"], answer()["content"]);
        assert_eq!((back["stop_reason"].as_str(), back["container"]["id"].as_str()), (Some("tool_use"), Some("c_1")));
        assert_eq!(
            back["usage"],
            json!({ "input_tokens": 10, "cache_creation_input_tokens": 3, "cache_read_input_tokens": 5, "output_tokens": 7 })
        );

        for (reason, finish) in [
            ("end_turn", Finish::Stop),
            ("stop_sequence", Finish::Stop),
            ("max_tokens", Finish::Length),
            ("refusal", Finish::ContentFilter),
            ("pause_turn", Finish::Paused),
            ("something_new", Finish::Stop),
        ] {
            assert_eq!(finish_of(reason), finish, "{reason}");
        }
        assert!(matches!(
            read_response(b"{}", &profile(ReasoningStyle::None)),
            Err(DialectError::BadProviderResponse(_))
        ));
        assert!(matches!(
            read_response(b"[]", &profile(ReasoningStyle::None)),
            Err(DialectError::BadProviderResponse(_))
        ));
    }

    #[test]
    fn an_answer_of_another_provider_is_written_in_anthropics_shape() {
        let r = openai::read_response(
            json!({ "id": "chatcmpl-1", "model": "gpt-x", "choices": [{ "finish_reason": "length", "message": {
                "content": "partial", "reasoning_content": "thought",
                "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "f", "arguments": "{\"a\":1}" } }] } }],
                "usage": { "prompt_tokens": 12, "completion_tokens": 3, "prompt_tokens_details": { "cached_tokens": 2 } } })
            .to_string()
            .as_bytes(),
            &Profile::default(),
        )
        .unwrap();
        let v =
            write_response(&r, &Stamp { id: "chatcmpl-1".into(), model: "openai/gpt-x".into(), ..Default::default() });
        assert_eq!(
            v,
            json!({
                "id": "chatcmpl-1", "type": "message", "role": "assistant", "model": "openai/gpt-x",
                "content": [
                    { "type": "thinking", "thinking": "thought", "signature": "" },
                    { "type": "text", "text": "partial" },
                    { "type": "tool_use", "id": "c1", "name": "f", "input": { "a": 1 } }
                ],
                "stop_reason": "max_tokens", "stop_sequence": null,
                "usage": { "input_tokens": 10, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 2, "output_tokens": 3 }
            })
        );
    }

    fn sse(name: &str, data: Value) -> (String, String) {
        (name.to_owned(), data.to_string())
    }

    fn anthropic_stream() -> Vec<(String, String)> {
        vec![
            sse(
                "message_start",
                json!({ "type": "message_start", "message": { "id": "msg_9", "model": "claude-x", "usage": { "input_tokens": 25, "cache_read_input_tokens": 100, "output_tokens": 1 } } }),
            ),
            sse(
                "content_block_start",
                json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
            ),
            sse("ping", json!({ "type": "ping" })),
            sse(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "let me " } }),
            ),
            sse(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "think" } }),
            ),
            sse(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "sig==" } }),
            ),
            sse("content_block_stop", json!({ "type": "content_block_stop", "index": 0 })),
            sse(
                "content_block_start",
                json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
            ),
            sse(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "Hello" } }),
            ),
            sse("content_block_stop", json!({ "type": "content_block_stop", "index": 1 })),
            sse(
                "content_block_start",
                json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "tool_use", "id": "toolu_1", "name": "f", "input": {} } }),
            ),
            sse(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "" } }),
            ),
            sse(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{\"a\":" } }),
            ),
            sse(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "1}" } }),
            ),
            sse("content_block_stop", json!({ "type": "content_block_stop", "index": 2 })),
            sse(
                "content_block_start",
                json!({ "type": "content_block_start", "index": 3, "content_block": { "type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {} } }),
            ),
            sse(
                "content_block_delta",
                json!({ "type": "content_block_delta", "index": 3, "delta": { "type": "input_json_delta", "partial_json": "{}" } }),
            ),
            sse("content_block_stop", json!({ "type": "content_block_stop", "index": 3 })),
            sse(
                "message_delta",
                json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use", "stop_sequence": null }, "usage": { "output_tokens": 42 } }),
            ),
            sse("message_stop", json!({ "type": "message_stop" })),
        ]
    }

    fn read(events: &[(String, String)]) -> Vec<Event> {
        let mut reader = provider_stream(&profile(ReasoningStyle::Adaptive));
        let mut out = Vec::new();
        for (name, data) in events {
            reader.read(name, data.as_bytes(), &mut out);
        }
        reader.finish(&mut out);
        out
    }

    #[test]
    fn an_anthropic_stream_is_read_into_neutral_events() {
        let events = read(&anthropic_stream());
        assert_eq!(events[0], Event::MessageStart { id: "msg_9".into(), model: "claude-x".into() });
        assert_eq!(events[1], Event::ContentStart { index: 0, block: BlockStart::Reasoning });
        assert_eq!(events[2], Event::Ping);
        assert_eq!(events[5], Event::ContentDelta { index: 0, delta: Delta::Signature("sig==".into()) });
        assert_eq!(
            events[10],
            Event::ContentStart { index: 2, block: BlockStart::ToolCall { id: "toolu_1".into(), name: "f".into() } }
        );
        assert_eq!(
            events[11],
            Event::ContentDelta { index: 2, delta: Delta::ToolArguments("{\"a\":".into()) },
            "the empty first fragment is left out"
        );
        assert!(
            matches!(&events[14], Event::ContentStart { index: 3, block: BlockStart::Provider { family, raw } } if family == FAMILY && raw["name"] == "web_search")
        );
        assert!(
            matches!(&events[15], Event::ContentDelta { index: 3, delta: Delta::Provider(raw) } if raw["partial_json"] == "{}"),
            "a provider block's deltas pass as they are"
        );
        assert_eq!(
            events.last(),
            Some(&Event::MessageStop {
                finish: Finish::ToolCalls,
                native_finish: "tool_use".into(),
                usage: Some(Usage {
                    input_tokens: 25,
                    cache_read_tokens: 100,
                    output_tokens: 42,
                    ..Default::default()
                }),
            })
        );
        assert_eq!(
            events.iter().filter(|e| matches!(e, Event::MessageStop { .. })).count(),
            1,
            "finish after message_stop adds nothing"
        );

        // An error event ends the stream; an empty stream gives nothing.
        let failed = read(&[sse(
            "error",
            json!({ "type": "error", "error": { "type": "overloaded_error", "message": "busy" } }),
        )]);
        assert_eq!(failed, vec![Event::Error { kind: "overloaded_error".into(), message: "busy".into() }]);
        assert!(read(&[]).is_empty());
        // A stream cut before message_stop still ends, with what is known.
        let cut = read(&anthropic_stream()[..9]);
        assert!(matches!(cut.last(), Some(Event::MessageStop { finish: Finish::Stop, usage: Some(_), .. })));
    }

    #[test]
    fn a_stream_crosses_from_anthropic_to_an_openai_caller_and_back() {
        // Anthropic events → neutral → OpenAI chunks, twice: the same bytes.
        let to_openai = |cost: Option<Value>| {
            let mut w = openai::client_stream(
                Stamp { model: "anthropic/claude-x".into(), created: 7, ..Default::default() },
                true,
            );
            let mut out = BytesMut::new();
            for e in read(&anthropic_stream()) {
                if matches!(e, Event::MessageStop { .. }) {
                    w.finalize(cost.clone(), None);
                }
                w.write(&e, &mut out);
            }
            String::from_utf8(out.to_vec()).unwrap()
        };
        let text = to_openai(Some(json!({ "amount": 0.001 })));
        assert_eq!(text, to_openai(Some(json!({ "amount": 0.001 }))), "translation is deterministic");
        assert!(text.contains(r#""reasoning_content":"let me ""#));
        assert!(
            text.contains(r#""thinking_blocks":[{"type":"thinking","thinking":"let me think","signature":"sig=="}]"#)
        );
        assert!(text.contains(
            r#""tool_calls":[{"index":0,"id":"toolu_1","type":"function","function":{"name":"f","arguments":""}}]"#
        ));
        assert!(
            text.contains(
                r#""provider_blocks":[{"type":"server_tool_use","id":"srv_1","name":"web_search","input":{}}]"#
            )
        );
        assert!(text.contains(r#""usage":{"prompt_tokens":125,"completion_tokens":42,"total_tokens":167,"prompt_tokens_details":{"cached_tokens":100}"#));
        assert!(text.contains(r#""cost":{"amount":0.001}"#) && text.ends_with("data: [DONE]\n\n"));

        // OpenAI chunks → neutral → Anthropic events.
        let mut reader = openai::provider_stream(&Profile::default());
        let mut events = Vec::new();
        for chunk in [
            r#"{"id":"chatcmpl-1","model":"gpt-x","choices":[{"delta":{"role":"assistant","content":""}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"hm"}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hi"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"f","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":3}}}"#,
            "[DONE]",
        ] {
            reader.read("", chunk.as_bytes(), &mut events);
        }
        let mut w = client_stream(Stamp { model: "openai/gpt-x".into(), ..Default::default() }, true);
        let mut out = BytesMut::new();
        for e in &events {
            w.write(e, &mut out);
        }
        let text = String::from_utf8(out.to_vec()).unwrap();
        let names: Vec<&str> = text.lines().filter_map(|l| l.strip_prefix("event: ")).collect();
        assert_eq!(
            names,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );
        assert!(
            text.contains(r#""message":{"id":"chatcmpl-1","type":"message","role":"assistant","model":"openai/gpt-x""#)
        );
        assert!(text.contains(r#""content_block":{"type":"tool_use","id":"c1","name":"f","input":{}}"#));
        assert!(text.contains(r#""delta":{"type":"input_json_delta","partial_json":"{}"}"#));
        assert!(text.contains(r#""delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"input_tokens":6,"cache_creation_input_tokens":0,"cache_read_input_tokens":3,"output_tokens":4}"#));
    }

    #[test]
    fn thinking_blocks_come_back_from_an_openai_caller_unchanged() {
        // Anthropic answers; the OpenAI-dialect caller gets the blocks and sends them back
        // with the assistant message; Anthropic receives the block it signed.
        let r = read_response(answer().to_string().as_bytes(), &profile(ReasoningStyle::Adaptive)).unwrap();
        let shown = openai::write_response(&r, &Stamp::default());
        let message = shown["choices"][0]["message"].clone();
        let next = openai::parse_request(
            json!({ "model": "m", "messages": [{ "role": "user", "content": "q" }, message, { "role": "tool", "tool_call_id": "toolu_1", "content": "r" }] })
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        let (v, _) = build(&next, &profile(ReasoningStyle::Adaptive));
        assert_eq!(
            v["messages"][1]["content"],
            json!([
                { "type": "thinking", "thinking": "hm", "signature": "sig==" },
                { "type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {} },
                { "type": "text", "text": "Paris" },
                { "type": "tool_use", "id": "toolu_1", "name": "f", "input": { "a": 1 } }
            ]),
            "the signed block is first and byte-equal; citations are the answer's, not part of the replay"
        );
    }
}
