//! The OpenAI chat-completions dialect (concept sections 8.2 and 9.3).
//!
//! Client half: `parse_request`, `write_response`, `client_stream`. Callers that speak this
//! dialect are served by a provider of another family through these.
//!
//! Provider half: `build_request`, `read_response`, `provider_stream`. An OpenAI-compatible
//! provider is spoken to through these when the caller speaks another dialect.
//!
//! When caller and provider both speak this dialect, none of this runs: the core patches the
//! caller's bytes instead.

use std::collections::BTreeMap;

use bytes::BytesMut;
use pacod_gw_events::Usage;
use serde_json::{Map, Value, json};

use crate::canonical::{
    Block, BlockStart, Delta, Event, Finish, Message, Notes, Profile, ReasoningParam, ReasoningStyle, Request,
    Response, ResponseFormat, Role, Stamp, Tool, ToolChoice,
};
use crate::error::DialectError;
use crate::sse::write_event;
use crate::util::{
    arguments_text, cache_from, image_from_url, image_url, joined_text, object, parse_data_url, read_usage_openai,
    str_of, strings, take_bool, take_f64, take_string, take_u64, usage_openai,
};
use crate::{ClientStream, ProviderStream};

/// The provider-specific members an OpenAI-dialect caller may send, which are passed on to an
/// OpenAI-compatible provider. Anything else in `extra` has no meaning there.
const OPENAI_EXTRAS: &[&str] = &[
    "parallel_tool_calls",
    "seed",
    "n",
    "logprobs",
    "top_logprobs",
    "logit_bias",
    "metadata",
    "service_tier",
    "store",
];

// --- client half: reading a caller's request -----------------------------------------------------

/// Content of a message: a string or a list of parts.
fn parse_content(content: Option<&Value>, message_cache: Option<&Value>) -> Result<Vec<Block>, DialectError> {
    match content {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) if s.is_empty() => Ok(Vec::new()),
        // A marker on a message with plain text marks that text.
        Some(Value::String(s)) => {
            Ok(vec![Block::Text { text: s.clone(), citations: None, cache: cache_from(message_cache) }])
        }
        Some(Value::Array(parts)) => {
            let mut out = Vec::with_capacity(parts.len());
            for part in parts {
                let cache = cache_from(part.get("cache_control"));
                match str_of(part, "type") {
                    Some("text") => out.push(Block::Text {
                        text: str_of(part, "text").unwrap_or_default().to_owned(),
                        citations: None,
                        cache,
                    }),
                    Some("image_url") => {
                        let url = match part.get("image_url") {
                            Some(Value::String(u)) => u.as_str(),
                            Some(o) => str_of(o, "url").unwrap_or_default(),
                            None => "",
                        };
                        if url.is_empty() {
                            return Err(DialectError::invalid("an image_url part needs a url"));
                        }
                        out.push(Block::Image { source: image_from_url(url), cache });
                    }
                    Some("file") => out.push(file_part(part, cache)?),
                    Some("input_audio") => return Err(DialectError::unsupported("audio input is not supported")),
                    // A part in another provider's own form: carried as it is.
                    _ => out.push(Block::Provider { family: String::new(), raw: part.clone() }),
                }
            }
            Ok(out)
        }
        Some(_) => Err(DialectError::invalid("message content must be a string or a list of parts")),
    }
}

/// An OpenAI file part with inline data becomes a document.
fn file_part(part: &Value, cache: Option<crate::canonical::Cache>) -> Result<Block, DialectError> {
    let file = part.get("file").cloned().unwrap_or(Value::Null);
    let data = str_of(&file, "file_data").and_then(parse_data_url);
    let Some((media_type, data)) = data else {
        return Err(DialectError::unsupported("a file part must carry its data inline (file_data as a data: URL)"));
    };
    let mut body = Map::new();
    body.insert("source".into(), json!({ "type": "base64", "media_type": media_type, "data": data }));
    if let Some(name) = str_of(&file, "filename") {
        body.insert("title".into(), json!(name));
    }
    Ok(Block::Document { body, cache })
}

/// Thinking blocks a caller sends back with an assistant message.
fn replayed_reasoning(message: &Value) -> Vec<Block> {
    let fields = message.get("provider_specific_fields");
    let blocks = fields.and_then(|f| f.get("thinking_blocks")).or_else(|| message.get("thinking_blocks"));
    let mut out = Vec::new();
    for b in blocks.and_then(Value::as_array).into_iter().flatten() {
        match str_of(b, "type") {
            Some("redacted_thinking") => out.push(Block::Reasoning {
                text: String::new(),
                signature: None,
                redacted: str_of(b, "data").map(str::to_owned),
            }),
            _ => out.push(Block::Reasoning {
                text: str_of(b, "thinking").unwrap_or_default().to_owned(),
                signature: str_of(b, "signature").filter(|s| !s.is_empty()).map(str::to_owned),
                redacted: None,
            }),
        }
    }
    for b in fields.and_then(|f| f.get("provider_blocks")).and_then(Value::as_array).into_iter().flatten() {
        out.push(Block::Provider { family: "anthropic".into(), raw: b.clone() });
    }
    out
}

fn parse_message(m: &Value, req: &mut Request) -> Result<(), DialectError> {
    let cache = m.get("cache_control");
    match str_of(m, "role") {
        Some("system" | "developer") => req.system.extend(parse_content(m.get("content"), cache)?),
        Some("user") => {
            req.messages.push(Message { role: Role::User, content: parse_content(m.get("content"), cache)? });
        }
        Some("assistant") => {
            let mut content = replayed_reasoning(m);
            content.extend(parse_content(m.get("content"), cache)?);
            for call in m.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
                let function = call.get("function").cloned().unwrap_or(Value::Null);
                content.push(Block::ToolCall {
                    id: str_of(call, "id").unwrap_or_default().to_owned(),
                    name: str_of(&function, "name").unwrap_or_default().to_owned(),
                    arguments: arguments_text(function.get("arguments")),
                    cache: None,
                });
            }
            req.messages.push(Message { role: Role::Assistant, content });
        }
        Some("tool") => {
            let result = Block::ToolResult {
                tool_call_id: str_of(m, "tool_call_id").unwrap_or_default().to_owned(),
                content: parse_content(m.get("content"), None)?,
                is_error: false,
                cache: cache_from(cache),
            };
            req.messages.push(Message { role: Role::Tool, content: vec![result] });
        }
        Some(other) => return Err(DialectError::invalid(format!("\"{other}\" is not a message role"))),
        None => return Err(DialectError::invalid("every message needs a role")),
    }
    Ok(())
}

fn parse_tools(v: Option<Value>) -> Vec<Tool> {
    let mut out = Vec::new();
    for tool in v.and_then(|v| v.as_array().cloned()).unwrap_or_default() {
        match (str_of(&tool, "type"), tool.get("function")) {
            (Some("function"), Some(f)) => out.push(Tool::Function {
                name: str_of(f, "name").unwrap_or_default().to_owned(),
                description: str_of(f, "description").map(str::to_owned),
                parameters: f
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                strict: f.get("strict").and_then(Value::as_bool),
                cache: cache_from(tool.get("cache_control")),
            }),
            // A tool in a provider's own form, for example Anthropic's web search.
            _ => out.push(Tool::Provider { family: String::new(), definition: tool }),
        }
    }
    out
}

fn parse_tool_choice(v: Option<Value>) -> Result<Option<ToolChoice>, DialectError> {
    Ok(match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(match s.as_str() {
            "auto" => ToolChoice::Auto,
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            other => return Err(DialectError::invalid(format!("\"{other}\" is not a tool_choice"))),
        }),
        Some(o) => match o.get("function").and_then(|f| str_of(f, "name")) {
            Some(name) => Some(ToolChoice::Tool(name.to_owned())),
            None => return Err(DialectError::invalid("tool_choice must name a function")),
        },
    })
}

fn parse_response_format(v: Option<Value>) -> Result<Option<ResponseFormat>, DialectError> {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Ok(None) };
    Ok(Some(match str_of(&v, "type") {
        Some("text") => ResponseFormat::Text,
        Some("json_object") => ResponseFormat::JsonObject,
        Some("json_schema") => {
            let inner = v.get("json_schema").cloned().unwrap_or(Value::Null);
            let schema = inner
                .get("schema")
                .cloned()
                .ok_or_else(|| DialectError::invalid("response_format.json_schema needs a schema"))?;
            ResponseFormat::JsonSchema {
                name: str_of(&inner, "name").map(str::to_owned),
                schema,
                strict: inner.get("strict").and_then(Value::as_bool),
            }
        }
        _ => return Err(DialectError::invalid("response_format.type must be text, json_object or json_schema")),
    }))
}

/// Client half: reads a chat-completions request.
pub fn parse_request(body: &[u8]) -> Result<Request, DialectError> {
    let mut top = object(body)?;
    let mut req = Request { model: take_string(&mut top, "model").unwrap_or_default(), ..Default::default() };

    match top.shift_remove("messages") {
        Some(Value::Array(messages)) => {
            for m in &messages {
                parse_message(m, &mut req)?;
            }
        }
        _ => return Err(DialectError::invalid("messages is required and must be a list")),
    }
    req.tools = parse_tools(top.shift_remove("tools"));
    req.tool_choice = parse_tool_choice(top.shift_remove("tool_choice"))?;
    req.response_format = parse_response_format(top.shift_remove("response_format"))?;
    req.stop = strings(top.shift_remove("stop"));
    req.stream = take_bool(&mut top, "stream").unwrap_or(false);
    req.stream_usage = top
        .shift_remove("stream_options")
        .and_then(|o| o.get("include_usage").and_then(Value::as_bool))
        .unwrap_or(false);

    let p = &mut req.params;
    let max_tokens = take_u64(&mut top, "max_tokens");
    p.max_output_tokens = take_u64(&mut top, "max_completion_tokens").or(max_tokens);
    p.temperature = take_f64(&mut top, "temperature");
    p.top_p = take_f64(&mut top, "top_p");
    p.top_k = take_f64(&mut top, "top_k");
    p.frequency_penalty = take_f64(&mut top, "frequency_penalty");
    p.presence_penalty = take_f64(&mut top, "presence_penalty");
    p.reasoning.effort = take_string(&mut top, "reasoning_effort");
    // An Anthropic-style thinking object is accepted on every model and mapped.
    if let Some(thinking) = top.shift_remove("thinking") {
        match str_of(&thinking, "type") {
            Some("enabled") => p.reasoning.budget_tokens = thinking.get("budget_tokens").and_then(Value::as_u64),
            Some("disabled") if p.reasoning.effort.is_none() => p.reasoning.effort = Some("none".into()),
            _ => {}
        }
        if str_of(&thinking, "display") == Some("summarized") {
            p.reasoning.summary = Some(true);
        }
    }

    req.user = take_string(&mut top, "user").filter(|u| !u.is_empty());
    req.gateway = top.shift_remove("gateway");
    req.extra = top;
    Ok(req)
}

// --- client half: writing an answer --------------------------------------------------------------

fn finish_word(f: Finish) -> &'static str {
    match f {
        Finish::Stop | Finish::Paused => "stop",
        Finish::Length => "length",
        Finish::ToolCalls => "tool_calls",
        Finish::ContentFilter => "content_filter",
    }
}

fn thinking_block(text: &str, signature: Option<&str>, redacted: Option<&str>) -> Option<Value> {
    match (redacted, signature) {
        (Some(data), _) => Some(json!({ "type": "redacted_thinking", "data": data })),
        (None, Some(sig)) => Some(json!({ "type": "thinking", "thinking": text, "signature": sig })),
        (None, None) => None,
    }
}

/// Client half: writes a `chat.completion` object.
pub fn write_response(response: &Response, stamp: &Stamp) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let (mut thinking, mut citations, mut web, mut provider) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for block in &response.content {
        match block {
            Block::Text { text: t, citations: c, .. } => {
                text.push_str(t);
                if let Some(Value::Array(list)) = c {
                    citations.extend(list.iter().cloned());
                }
            }
            Block::Reasoning { text: t, signature, redacted } => {
                reasoning.push_str(t);
                thinking.extend(thinking_block(t, signature.as_deref(), redacted.as_deref()));
            }
            Block::ToolCall { id, name, arguments, .. } => tool_calls.push(json!({
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": if arguments.is_empty() { "{}" } else { arguments.as_str() } },
            })),
            Block::Provider { raw, .. } => {
                if str_of(raw, "type") == Some("web_search_tool_result") {
                    web.push(raw.clone());
                }
                provider.push(raw.clone());
            }
            Block::Image { .. } | Block::Document { .. } | Block::ToolResult { .. } => {}
        }
    }

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    // With tool calls and no text the content is null, as OpenAI writes it.
    message.insert("content".into(), if text.is_empty() && !tool_calls.is_empty() { Value::Null } else { json!(text) });
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(reasoning));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    let mut fields = Map::new();
    for (name, list) in [
        ("thinking_blocks", thinking),
        ("citations", citations),
        ("web_search_results", web),
        ("provider_blocks", provider),
    ] {
        if !list.is_empty() {
            fields.insert(name.into(), Value::Array(list));
        }
    }
    if !fields.is_empty() {
        message.insert("provider_specific_fields".into(), Value::Object(fields));
    }

    let mut out = Map::new();
    out.insert("id".into(), json!(stamp.id));
    out.insert("object".into(), json!("chat.completion"));
    out.insert("created".into(), json!(stamp.created));
    out.insert("model".into(), json!(stamp.model));
    out.insert(
        "choices".into(),
        json!([{ "index": 0, "message": Value::Object(message), "finish_reason": finish_word(response.finish) }]),
    );
    if let Some(u) = &response.usage {
        out.insert("usage".into(), usage_openai(u, stamp.cost.as_ref()));
    }
    Value::Object(out)
}

/// A content block of the stream that is open.
enum Open {
    Text,
    Reasoning { text: String, signature: Option<String> },
    Tool { position: u32 },
    Provider { raw: Value, deltas: Vec<Value> },
}

struct Writer {
    stamp: Stamp,
    include_usage: bool,
    open: BTreeMap<u32, Open>,
    /// Tool calls opened so far: the dialect numbers them on their own.
    tools: u32,
}

impl Writer {
    fn chunk(&self, out: &mut BytesMut, delta: Value, finish: Option<&str>) {
        let chunk = json!({
            "id": self.stamp.id,
            "object": "chat.completion.chunk",
            "created": self.stamp.created,
            "model": self.stamp.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        });
        write_event(out, "", chunk.to_string().as_bytes());
    }

    fn fields(&self, out: &mut BytesMut, name: &str, list: Vec<Value>) {
        let mut fields = Map::new();
        fields.insert(name.to_owned(), Value::Array(list));
        self.chunk(out, json!({ "provider_specific_fields": Value::Object(fields) }), None);
    }
}

/// Puts the argument fragments of a provider block back into its `input`, when that is what
/// the deltas are. Otherwise the block is followed by its deltas as they came.
fn fold_provider(mut raw: Value, deltas: Vec<Value>) -> Vec<Value> {
    if deltas.is_empty() {
        return vec![raw];
    }
    let all_json = deltas.iter().all(|d| str_of(d, "type") == Some("input_json_delta"));
    if all_json {
        let text: String = deltas.iter().filter_map(|d| str_of(d, "partial_json")).collect();
        if let (Ok(input), Some(obj)) = (crate::util::arguments_value(&text), raw.as_object_mut()) {
            obj.insert("input".into(), input);
            return vec![raw];
        }
    }
    let mut out = vec![raw];
    out.extend(deltas);
    out
}

impl ClientStream for Writer {
    fn finalize(&mut self, cost: Option<Value>, _gateway: Option<Value>) {
        self.stamp.cost = cost;
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
                self.chunk(out, json!({ "role": "assistant", "content": "" }), None);
            }
            Event::ContentStart { index, block } => {
                let open = match block {
                    BlockStart::Text => Open::Text,
                    BlockStart::Reasoning => Open::Reasoning { text: String::new(), signature: None },
                    BlockStart::ToolCall { id, name } => {
                        let position = self.tools;
                        self.tools += 1;
                        let call = json!({ "index": position, "id": id, "type": "function", "function": { "name": name, "arguments": "" } });
                        self.chunk(out, json!({ "tool_calls": [call] }), None);
                        Open::Tool { position }
                    }
                    BlockStart::Provider { raw, .. } => Open::Provider { raw: raw.clone(), deltas: Vec::new() },
                };
                self.open.insert(*index, open);
            }
            Event::ContentDelta { index, delta } => match (delta, self.open.get_mut(index)) {
                (Delta::Text(t), _) => self.chunk(out, json!({ "content": t }), None),
                (Delta::Reasoning(t), block) => {
                    if let Some(Open::Reasoning { text, .. }) = block {
                        text.push_str(t);
                    }
                    self.chunk(out, json!({ "reasoning_content": t }), None);
                }
                (Delta::ToolArguments(a), Some(Open::Tool { position })) => {
                    let call = json!({ "index": *position, "function": { "arguments": a } });
                    self.chunk(out, json!({ "tool_calls": [call] }), None);
                }
                (Delta::Signature(s), Some(Open::Reasoning { signature, .. })) => *signature = Some(s.clone()),
                (Delta::Citation(c), _) => self.fields(out, "citations", vec![c.clone()]),
                (Delta::Provider(d), Some(Open::Provider { deltas, .. })) => deltas.push(d.clone()),
                _ => {}
            },
            Event::ContentStop { index } => match self.open.remove(index) {
                // The complete block, with its signature, is what the caller sends back.
                Some(Open::Reasoning { text, signature: Some(sig) }) => {
                    self.fields(
                        out,
                        "thinking_blocks",
                        vec![json!({ "type": "thinking", "thinking": text, "signature": sig })],
                    );
                }
                Some(Open::Provider { raw, deltas }) => {
                    let is_web = str_of(&raw, "type") == Some("web_search_tool_result");
                    let blocks = fold_provider(raw, deltas);
                    if is_web {
                        self.fields(out, "web_search_results", blocks.clone());
                    }
                    self.fields(out, "provider_blocks", blocks);
                }
                _ => {}
            },
            Event::MessageStop { finish, usage, .. } => {
                self.chunk(out, json!({}), Some(finish_word(*finish)));
                if let (true, Some(u)) = (self.include_usage, usage) {
                    let chunk = json!({
                        "id": self.stamp.id,
                        "object": "chat.completion.chunk",
                        "created": self.stamp.created,
                        "model": self.stamp.model,
                        "choices": [],
                        "usage": usage_openai(u, self.stamp.cost.as_ref()),
                    });
                    write_event(out, "", chunk.to_string().as_bytes());
                }
                write_event(out, "", b"[DONE]");
            }
            Event::Ping => {}
            Event::Error { kind, message } => {
                let body = json!({ "error": { "message": message, "type": "api_error", "param": null, "code": kind } });
                write_event(out, "", body.to_string().as_bytes());
            }
        }
    }
}

/// Client half: a writer of `chat.completion.chunk` events. The usage chunk is written only
/// when the caller asked for it.
pub fn client_stream(stamp: Stamp, include_usage: bool) -> Box<dyn ClientStream> {
    Box::new(Writer { stamp, include_usage, open: BTreeMap::new(), tools: 0 })
}

// --- provider half: writing the provider's request -----------------------------------------------

fn note(notes: &mut Notes, name: &str) {
    if !notes.dropped.iter().any(|d| d == name) {
        notes.dropped.push(name.to_owned());
    }
}

/// Content of a user or system message: a string when it is one plain text, parts otherwise.
fn content_out(blocks: &[&Block], profile: &Profile, notes: &mut Notes) -> Result<Value, DialectError> {
    let marker = |cache: &Option<crate::canonical::Cache>, notes: &mut Notes| -> Option<Value> {
        let c = cache.as_ref()?;
        if profile.cache_markers {
            Some(crate::util::cache_to(c))
        } else {
            note(notes, "cache_control");
            None
        }
    };
    if let [Block::Text { text, cache, .. }] = blocks
        && (cache.is_none() || !profile.cache_markers)
    {
        marker(cache, notes);
        return Ok(json!(text));
    }
    let mut parts = Vec::with_capacity(blocks.len());
    for b in blocks {
        match b {
            Block::Text { text, cache, .. } => {
                let mut part = json!({ "type": "text", "text": text });
                if let Some(m) = marker(cache, notes) {
                    part["cache_control"] = m;
                }
                parts.push(part);
            }
            Block::Image { source, cache } => {
                marker(cache, notes);
                parts.push(json!({ "type": "image_url", "image_url": { "url": image_url(source) } }));
            }
            Block::Document { .. } => {
                return Err(DialectError::unsupported("documents cannot be sent to this model through this interface"));
            }
            // A part that came in another provider's form and has no meaning here.
            Block::Provider { .. } | Block::Reasoning { .. } => {}
            Block::ToolCall { .. } | Block::ToolResult { .. } => {}
        }
    }
    Ok(Value::Array(parts))
}

fn messages_out(req: &Request, profile: &Profile, notes: &mut Notes) -> Result<Vec<Value>, DialectError> {
    let mut out = Vec::with_capacity(req.messages.len() + 1);
    if !req.system.is_empty() {
        let blocks: Vec<&Block> = req.system.iter().collect();
        let plain =
            blocks.iter().all(|b| matches!(b, Block::Text { cache, .. } if cache.is_none() || !profile.cache_markers));
        let content = if plain {
            for b in &blocks {
                if b.cache().is_some() {
                    note(notes, "cache_control");
                }
            }
            json!(joined_text(&req.system, "\n\n"))
        } else {
            content_out(&blocks, profile, notes)?
        };
        out.push(json!({ "role": "system", "content": content }));
    }
    for m in &req.messages {
        match m.role {
            Role::User | Role::Tool => {
                // Each tool result is a message of its own, before whatever else the turn says.
                for b in &m.content {
                    if let Block::ToolResult { tool_call_id, content, cache, .. } = b {
                        if cache.is_some() && !profile.cache_markers {
                            note(notes, "cache_control");
                        }
                        if content.iter().any(|c| !matches!(c, Block::Text { .. })) {
                            note(notes, "tool_result.image");
                        }
                        out.push(json!({ "role": "tool", "tool_call_id": tool_call_id, "content": joined_text(content, "\n") }));
                    }
                }
                let rest: Vec<&Block> = m.content.iter().filter(|b| !matches!(b, Block::ToolResult { .. })).collect();
                if !rest.is_empty() {
                    out.push(json!({ "role": "user", "content": content_out(&rest, profile, notes)? }));
                }
            }
            Role::Assistant => {
                let text = joined_text(&m.content, "");
                let calls: Vec<Value> = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        Block::ToolCall { id, name, arguments, .. } => Some(json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": if arguments.is_empty() { "{}" } else { arguments.as_str() } },
                        })),
                        _ => None,
                    })
                    .collect();
                let mut msg = Map::new();
                msg.insert("role".into(), json!("assistant"));
                msg.insert(
                    "content".into(),
                    if text.is_empty() && !calls.is_empty() { Value::Null } else { json!(text) },
                );
                if !calls.is_empty() {
                    msg.insert("tool_calls".into(), Value::Array(calls));
                }
                // Another provider's reasoning and provider blocks cannot be replayed here.
                out.push(Value::Object(msg));
            }
        }
    }
    Ok(out)
}

fn lowest_effort(profile: &Profile) -> Option<&String> {
    profile.reasoning_efforts.iter().find(|e| e.as_str() != "none")
}

/// Reasoning, in the form this endpoint takes. Mirrors the core's same-dialect path.
fn reasoning_out(req: &Request, profile: &Profile, body: &mut Map<String, Value>, notes: &mut Notes) {
    let Some(effort) = req.params.reasoning.effort.as_deref() else { return };
    match profile.reasoning_param {
        ReasoningParam::ReasoningEffort => {
            if effort != "none" {
                body.insert("reasoning_effort".into(), json!(effort));
            } else if profile.reasoning_efforts.iter().any(|e| e == "none") {
                body.insert("reasoning_effort".into(), json!("none"));
            } else if let Some(low) = lowest_effort(profile).filter(|_| profile.reasoning != ReasoningStyle::None) {
                notes
                    .adjusted
                    .push(format!("reasoning cannot be switched off on this model; the effort {low} was used"));
                body.insert("reasoning_effort".into(), json!(low));
            }
        }
        ReasoningParam::ThinkingToggle => {
            body.insert("thinking".into(), json!({ "type": if effort == "none" { "disabled" } else { "enabled" } }));
        }
        ReasoningParam::None => note(notes, "reasoning.effort"),
    }
}

/// Provider half: writes a chat-completions request for an OpenAI-compatible provider.
pub fn build_request(request: &Request, profile: &Profile, notes: &mut Notes) -> Result<Value, DialectError> {
    let mut body = Map::new();
    body.insert("model".into(), json!(profile.upstream_model));
    body.insert("messages".into(), Value::Array(messages_out(request, profile, notes)?));

    if !request.tools.is_empty() {
        let mut tools = Vec::with_capacity(request.tools.len());
        for t in &request.tools {
            match t {
                Tool::Function { name, description, parameters, strict, cache } => {
                    if cache.is_some() && !profile.cache_markers {
                        note(notes, "cache_control");
                    }
                    let mut f = Map::new();
                    f.insert("name".into(), json!(name));
                    if let Some(d) = description {
                        f.insert("description".into(), json!(d));
                    }
                    f.insert("parameters".into(), parameters.clone());
                    if let Some(s) = strict {
                        f.insert("strict".into(), json!(s));
                    }
                    tools.push(json!({ "type": "function", "function": Value::Object(f) }));
                }
                Tool::Provider { .. } => {
                    return Err(DialectError::unsupported("provider-defined tools are not available on this model"));
                }
            }
        }
        body.insert("tools".into(), Value::Array(tools));
    }
    match &request.tool_choice {
        None => {}
        Some(ToolChoice::Auto) => drop(body.insert("tool_choice".into(), json!("auto"))),
        Some(ToolChoice::None) => drop(body.insert("tool_choice".into(), json!("none"))),
        Some(ToolChoice::Required) => drop(body.insert("tool_choice".into(), json!("required"))),
        Some(ToolChoice::Tool(name)) => {
            body.insert("tool_choice".into(), json!({ "type": "function", "function": { "name": name } }));
        }
    }
    match &request.response_format {
        None => {}
        Some(ResponseFormat::Text) => drop(body.insert("response_format".into(), json!({ "type": "text" }))),
        Some(ResponseFormat::JsonObject) => {
            drop(body.insert("response_format".into(), json!({ "type": "json_object" })))
        }
        Some(ResponseFormat::JsonSchema { name, schema, strict }) => {
            let mut inner = Map::new();
            // OpenAI requires a name; a caller of another dialect has none to give.
            inner.insert("name".into(), json!(name.as_deref().unwrap_or("response")));
            inner.insert("schema".into(), schema.clone());
            if let Some(s) = strict {
                inner.insert("strict".into(), json!(s));
            }
            body.insert(
                "response_format".into(),
                json!({ "type": "json_schema", "json_schema": Value::Object(inner) }),
            );
        }
    }
    if !request.stop.is_empty() {
        body.insert("stop".into(), json!(request.stop));
    }
    if request.stream {
        body.insert("stream".into(), json!(true));
        if profile.stream_usage {
            body.insert("stream_options".into(), json!({ "include_usage": true }));
        }
    }

    let p = &request.params;
    if let Some(v) = p.max_output_tokens {
        let field = if profile.max_completion_tokens { "max_completion_tokens" } else { "max_tokens" };
        body.insert(field.into(), json!(v));
    }
    for (name, value) in [
        ("temperature", p.temperature),
        ("top_p", p.top_p),
        ("top_k", p.top_k),
        ("frequency_penalty", p.frequency_penalty),
        ("presence_penalty", p.presence_penalty),
    ] {
        if let Some(v) = value {
            body.insert(name.into(), json!(v));
        }
    }
    reasoning_out(request, profile, &mut body, notes);

    for (key, value) in &request.extra {
        if OPENAI_EXTRAS.contains(&key.as_str()) {
            body.insert(key.clone(), value.clone());
        } else {
            note(notes, key);
        }
    }
    if let Some(Value::Object(options)) = request.provider_options.get("openai") {
        for (key, value) in options {
            body.insert(key.clone(), value.clone());
        }
    }
    Ok(Value::Object(body))
}

// --- provider half: reading the provider's answer ------------------------------------------------

fn finish_of(word: Option<&str>) -> Finish {
    match word {
        Some("length") => Finish::Length,
        Some("tool_calls" | "function_call") => Finish::ToolCalls,
        Some("content_filter") => Finish::ContentFilter,
        _ => Finish::Stop,
    }
}

/// Provider half: reads a `chat.completion` object.
pub fn read_response(body: &[u8], profile: &Profile) -> Result<Response, DialectError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| DialectError::bad_response(format!("the provider's answer is not JSON: {e}")))?;
    let choice = v
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| DialectError::bad_response("the provider's answer has no choices"))?;
    let message = choice.get("message").cloned().unwrap_or(Value::Null);

    let mut content = Vec::new();
    let reasoning = str_of(&message, &profile.reasoning_field).or_else(|| str_of(&message, "reasoning"));
    if let Some(text) = reasoning.filter(|t| !t.is_empty()) {
        content.push(Block::Reasoning { text: text.to_owned(), signature: None, redacted: None });
    }
    if let Some(text) = str_of(&message, "content").filter(|t| !t.is_empty()) {
        content.push(Block::text(text));
    }
    let refusal = str_of(&message, "refusal").filter(|t| !t.is_empty());
    if let Some(text) = refusal {
        content.push(Block::text(text));
    }
    for call in message.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
        let function = call.get("function").cloned().unwrap_or(Value::Null);
        content.push(Block::ToolCall {
            id: str_of(call, "id").unwrap_or_default().to_owned(),
            name: str_of(&function, "name").unwrap_or_default().to_owned(),
            arguments: arguments_text(function.get("arguments")),
            cache: None,
        });
    }

    let native = str_of(choice, "finish_reason").unwrap_or_default().to_owned();
    Ok(Response {
        id: str_of(&v, "id").unwrap_or_default().to_owned(),
        model: str_of(&v, "model").unwrap_or_default().to_owned(),
        content,
        finish: if refusal.is_some() { Finish::ContentFilter } else { finish_of(Some(&native)) },
        native_finish: native,
        usage: read_usage_openai(v.get("usage")),
        extra: Map::new(),
    })
}

struct Reader {
    reasoning_field: String,
    started: bool,
    /// The neutral index of the next block to open.
    next: u32,
    text: Option<u32>,
    reasoning: Option<u32>,
    /// The provider's tool-call index to the neutral block index.
    tools: BTreeMap<u64, u32>,
    finish: Option<String>,
    usage: Option<Usage>,
    stopped: bool,
}

impl Reader {
    fn open(&mut self, out: &mut Vec<Event>, block: BlockStart) -> u32 {
        let index = self.next;
        self.next += 1;
        out.push(Event::ContentStart { index, block });
        index
    }

    fn close_text_and_reasoning(&mut self, out: &mut Vec<Event>) {
        for index in [self.reasoning.take(), self.text.take()].into_iter().flatten() {
            out.push(Event::ContentStop { index });
        }
    }

    fn close_all(&mut self, out: &mut Vec<Event>) {
        self.close_text_and_reasoning(out);
        for (_, index) in std::mem::take(&mut self.tools) {
            out.push(Event::ContentStop { index });
        }
    }

    fn stop(&mut self, out: &mut Vec<Event>) {
        if self.stopped || !self.started {
            return;
        }
        self.stopped = true;
        self.close_all(out);
        let native = self.finish.clone().unwrap_or_default();
        out.push(Event::MessageStop {
            finish: finish_of(self.finish.as_deref()),
            native_finish: native,
            usage: self.usage,
        });
    }
}

impl ProviderStream for Reader {
    fn read(&mut self, _event: &str, data: &[u8], out: &mut Vec<Event>) {
        if self.stopped {
            return;
        }
        if data == b"[DONE]" {
            self.stop(out);
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(data) else { return };
        if let Some(error) = v.get("error") {
            self.stopped = true;
            out.push(Event::Error {
                kind: "upstream_error".into(),
                message: str_of(error, "message").map(str::to_owned).unwrap_or_else(|| error.to_string()),
            });
            return;
        }
        if !self.started {
            self.started = true;
            out.push(Event::MessageStart {
                id: str_of(&v, "id").unwrap_or_default().to_owned(),
                model: str_of(&v, "model").unwrap_or_default().to_owned(),
            });
        }
        if let Some(u) = read_usage_openai(v.get("usage")) {
            self.usage = Some(u);
        }
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { return };
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);

        let reasoning = str_of(&delta, &self.reasoning_field).or_else(|| str_of(&delta, "reasoning"));
        if let Some(t) = reasoning.filter(|t| !t.is_empty()) {
            let index = match self.reasoning {
                Some(i) => i,
                None => {
                    let i = self.open(out, BlockStart::Reasoning);
                    self.reasoning = Some(i);
                    i
                }
            };
            out.push(Event::ContentDelta { index, delta: Delta::Reasoning(t.to_owned()) });
        }
        if let Some(t) = str_of(&delta, "content").filter(|t| !t.is_empty()) {
            // Text after reasoning: the reasoning is complete.
            if let Some(index) = self.reasoning.take() {
                out.push(Event::ContentStop { index });
            }
            let index = match self.text {
                Some(i) => i,
                None => {
                    let i = self.open(out, BlockStart::Text);
                    self.text = Some(i);
                    i
                }
            };
            out.push(Event::ContentDelta { index, delta: Delta::Text(t.to_owned()) });
        }
        for call in delta.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
            let position = call.get("index").and_then(Value::as_u64).unwrap_or(0);
            let function = call.get("function").cloned().unwrap_or(Value::Null);
            let index = match self.tools.get(&position) {
                Some(i) => *i,
                None => {
                    self.close_text_and_reasoning(out);
                    let start = BlockStart::ToolCall {
                        id: str_of(call, "id").unwrap_or_default().to_owned(),
                        name: str_of(&function, "name").unwrap_or_default().to_owned(),
                    };
                    let i = self.open(out, start);
                    self.tools.insert(position, i);
                    i
                }
            };
            if let Some(a) = str_of(&function, "arguments").filter(|a| !a.is_empty()) {
                out.push(Event::ContentDelta { index, delta: Delta::ToolArguments(a.to_owned()) });
            }
        }
        if let Some(word) = str_of(choice, "finish_reason") {
            self.finish = Some(word.to_owned());
            self.close_all(out);
        }
    }

    fn finish(&mut self, out: &mut Vec<Event>) {
        self.stop(out);
    }
}

/// Provider half: a reader of `chat.completion.chunk` events.
pub fn provider_stream(profile: &Profile) -> Box<dyn ProviderStream> {
    Box::new(Reader {
        reasoning_field: profile.reasoning_field.clone(),
        started: false,
        next: 0,
        text: None,
        reasoning: None,
        tools: BTreeMap::new(),
        finish: None,
        usage: None,
        stopped: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{Cache, ImageSource, ThinkingOff};

    fn parse(v: Value) -> Request {
        parse_request(v.to_string().as_bytes()).unwrap()
    }

    fn build(req: &Request, profile: &Profile) -> (Value, Notes) {
        let mut notes = Notes::default();
        let v = build_request(req, profile, &mut notes).unwrap();
        (v, notes)
    }

    fn profile() -> Profile {
        Profile { upstream_model: "gpt-x".into(), ..Default::default() }
    }

    #[test]
    fn a_request_is_read_into_neutral_terms() {
        let req = parse(json!({
            "model": "@writer",
            "messages": [
                { "role": "system", "content": [{ "type": "text", "text": "static", "cache_control": { "type": "ephemeral", "ttl": "1h" } }] },
                { "role": "developer", "content": "be brief" },
                { "role": "user", "content": [
                    { "type": "text", "text": "what is this?" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
                    { "type": "image_url", "image_url": "https://x.test/i.png" }
                ] },
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "call_1", "type": "function", "function": { "name": "look", "arguments": "{\"q\": 1}" } }
                ] },
                { "role": "tool", "tool_call_id": "call_1", "content": "found" }
            ],
            "tools": [
                { "type": "function", "function": { "name": "look", "description": "d", "parameters": { "type": "object" }, "strict": true } },
                { "type": "web_search_20260209", "name": "web_search", "max_uses": 3 }
            ],
            "tool_choice": { "type": "function", "function": { "name": "look" } },
            "response_format": { "type": "json_schema", "json_schema": { "name": "out", "schema": { "type": "object" }, "strict": true } },
            "stop": "END", "stream": true, "stream_options": { "include_usage": true },
            "max_tokens": 10, "max_completion_tokens": 20, "temperature": 0, "top_p": 0.5,
            "reasoning_effort": "high", "thinking": { "type": "adaptive", "display": "summarized" },
            "user": "u_1", "gateway": { "trace_id": "t" }, "seed": 7, "n": 1
        }));
        assert_eq!(req.model, "@writer");
        assert_eq!(req.system.len(), 2);
        assert_eq!(req.system[0].cache(), Some(&Cache { ttl: Some("1h".into()) }));
        assert_eq!(req.messages.len(), 3);
        assert!(
            matches!(&req.messages[0].content[1], Block::Image { source: ImageSource::Base64 { media_type, data }, .. } if media_type == "image/png" && data == "AAAA")
        );
        assert!(matches!(&req.messages[0].content[2], Block::Image { source: ImageSource::Url { .. }, .. }));
        assert_eq!(
            req.messages[1].content,
            [Block::ToolCall { id: "call_1".into(), name: "look".into(), arguments: "{\"q\": 1}".into(), cache: None }],
            "the arguments keep their exact text"
        );
        assert_eq!(req.messages[2].role, Role::Tool);
        assert!(matches!(&req.tools[0], Tool::Function { name, strict: Some(true), .. } if name == "look"));
        assert!(
            matches!(&req.tools[1], Tool::Provider { family, definition } if family.is_empty() && definition["max_uses"] == 3)
        );
        assert_eq!(req.tool_choice, Some(ToolChoice::Tool("look".into())));
        assert!(matches!(req.response_format, Some(ResponseFormat::JsonSchema { strict: Some(true), .. })));
        assert_eq!(req.stop, ["END"]);
        assert!(req.stream && req.stream_usage);
        assert_eq!(req.params.max_output_tokens, Some(20), "max_completion_tokens wins");
        assert_eq!((req.params.temperature, req.params.top_p), (Some(0.0), Some(0.5)));
        assert_eq!(req.params.reasoning.effort.as_deref(), Some("high"));
        assert_eq!(req.params.reasoning.summary, Some(true));
        assert_eq!(req.user.as_deref(), Some("u_1"));
        assert_eq!(req.gateway, Some(json!({ "trace_id": "t" })));
        assert_eq!(req.extra.keys().collect::<Vec<_>>(), ["seed", "n"]);

        // A thinking object alone: a budget, or "off".
        let r =
            parse(json!({ "model": "m", "messages": [], "thinking": { "type": "enabled", "budget_tokens": 2048 } }));
        assert_eq!(r.params.reasoning.budget_tokens, Some(2048));
        let r = parse(json!({ "model": "m", "messages": [], "thinking": { "type": "disabled" } }));
        assert_eq!(r.params.reasoning.effort.as_deref(), Some("none"));
    }

    #[test]
    fn malformed_requests_are_errors_not_panics() {
        for body in [
            &b"not json"[..],
            b"[]",
            b"{}",
            br#"{"messages":"hi"}"#,
            br#"{"messages":[{"content":"no role"}]}"#,
            br#"{"messages":[{"role":"wizard","content":"x"}]}"#,
            br#"{"messages":[{"role":"user","content":42}]}"#,
            br#"{"messages":[{"role":"user","content":[{"type":"image_url"}]}]}"#,
            br#"{"messages":[],"tool_choice":"sometimes"}"#,
            br#"{"messages":[],"tool_choice":{"type":"function"}}"#,
            br#"{"messages":[],"response_format":{"type":"json_schema","json_schema":{}}}"#,
            br#"{"messages":[],"response_format":{"type":"yaml"}}"#,
        ] {
            assert!(
                matches!(parse_request(body), Err(DialectError::InvalidRequest(_))),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
        let audio = br#"{"messages":[{"role":"user","content":[{"type":"input_audio","input_audio":{}}]}]}"#;
        assert!(matches!(parse_request(audio), Err(DialectError::Unsupported(_))));
        // Wrong types in optional places are ignored rather than fatal.
        let r = parse(
            json!({ "model": 5, "messages": [], "tools": "none", "stop": 3, "temperature": "hot", "stream": "yes" }),
        );
        assert!(r.model.is_empty() && r.tools.is_empty() && r.stop.is_empty() && !r.stream);
    }

    #[test]
    fn a_request_is_written_for_an_openai_compatible_provider() {
        let mut req = parse(json!({
            "model": "anything",
            "messages": [
                { "role": "system", "content": "one" },
                { "role": "system", "content": [{ "type": "text", "text": "two", "cache_control": { "type": "ephemeral" } }] },
                { "role": "user", "content": "hello" },
                { "role": "assistant", "content": "calling", "tool_calls": [
                    { "id": "c1", "type": "function", "function": { "name": "f", "arguments": "" } }
                ] },
                { "role": "tool", "tool_call_id": "c1", "content": "r1" },
                { "role": "user", "content": [{ "type": "text", "text": "and this" }, { "type": "image_url", "image_url": { "url": "https://x.test/a.png" } }] }
            ],
            "tools": [{ "type": "function", "function": { "name": "f", "parameters": { "type": "object" } } }],
            "tool_choice": "required", "stop": ["a", "b"], "stream": true,
            "max_tokens": 100, "temperature": 0.2, "reasoning_effort": "low",
            "seed": 7, "anthropic_thing": true
        }));
        req.provider_options.insert("openai".into(), json!({ "service_tier": "flex" }));
        let p = Profile {
            reasoning_efforts: vec!["low".into(), "high".into()],
            reasoning: ReasoningStyle::Effort,
            ..profile()
        };
        let (v, notes) = build(&req, &p);
        assert_eq!(
            v,
            json!({
                "model": "gpt-x",
                "messages": [
                    { "role": "system", "content": "one\n\ntwo" },
                    { "role": "user", "content": "hello" },
                    { "role": "assistant", "content": "calling", "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "f", "arguments": "{}" } }] },
                    { "role": "tool", "tool_call_id": "c1", "content": "r1" },
                    { "role": "user", "content": [{ "type": "text", "text": "and this" }, { "type": "image_url", "image_url": { "url": "https://x.test/a.png" } }] }
                ],
                "tools": [{ "type": "function", "function": { "name": "f", "parameters": { "type": "object" } } }],
                "tool_choice": "required",
                "stop": ["a", "b"],
                "stream": true,
                "stream_options": { "include_usage": true },
                "max_tokens": 100,
                "temperature": 0.2,
                "reasoning_effort": "low",
                "seed": 7,
                "service_tier": "flex"
            })
        );
        assert_eq!(
            notes.dropped,
            ["cache_control", "anthropic_thing"],
            "what has no meaning here is left out and named"
        );

        // The same request twice gives the same bytes.
        assert_eq!(build(&req, &p).0.to_string(), v.to_string());

        // A reasoning model: its own field for the limit; an endpoint with an on/off switch.
        let (v, _) = build(
            &req,
            &Profile { max_completion_tokens: true, reasoning_param: ReasoningParam::ThinkingToggle, ..p.clone() },
        );
        assert_eq!((v["max_completion_tokens"].as_u64(), v.get("max_tokens")), (Some(100), None));
        assert_eq!(v["thinking"], json!({ "type": "enabled" }));
        assert!(v.get("reasoning_effort").is_none());
        // An endpoint that reports no usage on streams is not asked for it.
        let (v, _) = build(&req, &Profile { stream_usage: false, ..p.clone() });
        assert!(v.get("stream_options").is_none());
        // "Off" where it cannot be switched off.
        req.params.reasoning.effort = Some("none".into());
        let (v, notes) = build(&req, &p);
        assert_eq!(v["reasoning_effort"], "low");
        assert_eq!(notes.adjusted.len(), 1);
        let (v, notes) = build(&req, &Profile { reasoning_param: ReasoningParam::None, ..p });
        assert!(v.get("reasoning_effort").is_none() && notes.dropped.contains(&"reasoning.effort".to_owned()));
    }

    #[test]
    fn what_another_provider_made_is_left_out_or_refused() {
        let req = Request {
            messages: vec![
                Message {
                    role: Role::Assistant,
                    content: vec![
                        Block::Reasoning { text: "hm".into(), signature: Some("sig".into()), redacted: None },
                        Block::Provider { family: "anthropic".into(), raw: json!({ "type": "server_tool_use" }) },
                        Block::text("answer"),
                    ],
                },
                Message {
                    role: Role::User,
                    content: vec![
                        Block::ToolResult {
                            tool_call_id: "t1".into(),
                            content: vec![
                                Block::text("a"),
                                Block::Image { source: ImageSource::Url { url: "u".into() }, cache: None },
                            ],
                            is_error: false,
                            cache: None,
                        },
                        Block::ToolResult {
                            tool_call_id: "t2".into(),
                            content: vec![Block::text("b")],
                            is_error: true,
                            cache: None,
                        },
                        Block::text("go on"),
                    ],
                },
            ],
            ..Default::default()
        };
        let (v, notes) = build(&req, &profile());
        assert_eq!(
            v["messages"],
            json!([
                { "role": "assistant", "content": "answer" },
                { "role": "tool", "tool_call_id": "t1", "content": "a" },
                { "role": "tool", "tool_call_id": "t2", "content": "b" },
                { "role": "user", "content": "go on" }
            ])
        );
        assert_eq!(notes.dropped, ["tool_result.image"]);

        let with_tool = Request {
            tools: vec![Tool::Provider { family: "anthropic".into(), definition: json!({}) }],
            ..Default::default()
        };
        assert!(matches!(
            build_request(&with_tool, &profile(), &mut Notes::default()),
            Err(DialectError::Unsupported(_))
        ));
        let with_doc = Request {
            messages: vec![Message {
                role: Role::User,
                content: vec![Block::Document { body: Map::new(), cache: None }],
            }],
            ..Default::default()
        };
        assert!(matches!(
            build_request(&with_doc, &profile(), &mut Notes::default()),
            Err(DialectError::Unsupported(_))
        ));
    }

    #[test]
    fn a_provider_answer_is_read() {
        let body = json!({
            "id": "chatcmpl-1", "model": "gpt-x-2026",
            "choices": [{ "index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": "let me look", "reasoning_content": "thinking…",
                "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "f", "arguments": "{\"a\":1}" } }]
            } }],
            "usage": { "prompt_tokens": 100, "completion_tokens": 20, "prompt_tokens_details": { "cached_tokens": 60 }, "completion_tokens_details": { "reasoning_tokens": 5 } }
        });
        let r = read_response(body.to_string().as_bytes(), &profile()).unwrap();
        assert_eq!(
            (r.id.as_str(), r.model.as_str(), r.finish, r.native_finish.as_str()),
            ("chatcmpl-1", "gpt-x-2026", Finish::ToolCalls, "tool_calls")
        );
        assert_eq!(r.content.len(), 3);
        assert!(matches!(&r.content[0], Block::Reasoning { text, signature: None, .. } if text == "thinking…"));
        let u = r.usage.unwrap();
        assert_eq!((u.input_tokens, u.cache_read_tokens, u.output_tokens, u.reasoning_tokens), (40, 60, 20, 5));

        // A refusal is a successful answer with its own finish reason.
        let refused = json!({ "choices": [{ "finish_reason": "stop", "message": { "content": null, "refusal": "I cannot help with that." } }] });
        let r = read_response(refused.to_string().as_bytes(), &profile()).unwrap();
        assert_eq!((r.finish, r.usage), (Finish::ContentFilter, None));
        assert_eq!(r.content, [Block::text("I cannot help with that.")]);

        assert!(matches!(read_response(b"{}", &profile()), Err(DialectError::BadProviderResponse(_))));
        assert!(matches!(read_response(b"<html>", &profile()), Err(DialectError::BadProviderResponse(_))));
    }

    fn neutral_answer() -> Response {
        Response {
            id: "msg_1".into(),
            model: "claude".into(),
            content: vec![
                Block::Reasoning { text: "let me think".into(), signature: Some("sig==".into()), redacted: None },
                Block::Reasoning { text: String::new(), signature: None, redacted: Some("enc".into()) },
                Block::Provider {
                    family: "anthropic".into(),
                    raw: json!({ "type": "web_search_tool_result", "tool_use_id": "s1", "content": [] }),
                },
                Block::Text {
                    text: "Paris".into(),
                    citations: Some(json!([{ "url": "https://x.test" }])),
                    cache: None,
                },
                Block::ToolCall { id: "toolu_1".into(), name: "f".into(), arguments: String::new(), cache: None },
            ],
            finish: Finish::ToolCalls,
            native_finish: "tool_use".into(),
            usage: Some(Usage { input_tokens: 10, cache_read_tokens: 5, output_tokens: 7, ..Default::default() }),
            extra: Map::new(),
        }
    }

    #[test]
    fn an_answer_is_written_for_an_openai_dialect_caller() {
        let stamp = Stamp {
            id: "msg_1".into(),
            model: "anthropic/claude".into(),
            created: 1_700_000_000,
            cost: Some(json!({ "amount": 0.5 })),
            gateway: None,
        };
        let v = write_response(&neutral_answer(), &stamp);
        assert_eq!(
            v,
            json!({
                "id": "msg_1", "object": "chat.completion", "created": 1_700_000_000u64, "model": "anthropic/claude",
                "choices": [{ "index": 0, "message": {
                    "role": "assistant", "content": "Paris", "reasoning_content": "let me think",
                    "tool_calls": [{ "id": "toolu_1", "type": "function", "function": { "name": "f", "arguments": "{}" } }],
                    "provider_specific_fields": {
                        "thinking_blocks": [
                            { "type": "thinking", "thinking": "let me think", "signature": "sig==" },
                            { "type": "redacted_thinking", "data": "enc" }
                        ],
                        "citations": [{ "url": "https://x.test" }],
                        "web_search_results": [{ "type": "web_search_tool_result", "tool_use_id": "s1", "content": [] }],
                        "provider_blocks": [{ "type": "web_search_tool_result", "tool_use_id": "s1", "content": [] }]
                    }
                }, "finish_reason": "tool_calls" }],
                "usage": {
                    "prompt_tokens": 15, "completion_tokens": 7, "total_tokens": 22,
                    "prompt_tokens_details": { "cached_tokens": 5 }, "completion_tokens_details": { "reasoning_tokens": 0 },
                    "cost": { "amount": 0.5 }
                }
            })
        );

        // What the caller sends back reads as the same reasoning blocks, signature included.
        let message = &v["choices"][0]["message"];
        let back = parse(json!({ "model": "m", "messages": [message.clone()] }));
        assert_eq!(&back.messages[0].content[..2], &neutral_answer().content[..2]);
        assert!(matches!(&back.messages[0].content[2], Block::Provider { family, .. } if family == "anthropic"));

        // Tool calls without text: content is null. No usage: no usage member.
        let only_tool =
            Response { content: vec![neutral_answer().content[4].clone()], usage: None, ..neutral_answer() };
        let v = write_response(&only_tool, &stamp);
        assert!(v["choices"][0]["message"]["content"].is_null() && v.get("usage").is_none());
    }

    fn written(events: &[Event], include_usage: bool, cost: Option<Value>) -> String {
        let stamp = Stamp { id: String::new(), model: "repo/model".into(), created: 42, cost: None, gateway: None };
        let mut w = client_stream(stamp, include_usage);
        let mut out = BytesMut::new();
        for e in events {
            if matches!(e, Event::MessageStop { .. }) {
                w.finalize(cost.clone(), None);
            }
            w.write(e, &mut out);
        }
        String::from_utf8(out.to_vec()).unwrap()
    }

    fn stream_events() -> Vec<Event> {
        vec![
            Event::MessageStart { id: "msg_9".into(), model: "claude".into() },
            Event::Ping,
            Event::ContentStart { index: 0, block: BlockStart::Reasoning },
            Event::ContentDelta { index: 0, delta: Delta::Reasoning("hm ".into()) },
            Event::ContentDelta { index: 0, delta: Delta::Reasoning("ok".into()) },
            Event::ContentDelta { index: 0, delta: Delta::Signature("sig==".into()) },
            Event::ContentStop { index: 0 },
            Event::ContentStart { index: 1, block: BlockStart::Text },
            Event::ContentDelta { index: 1, delta: Delta::Text("Hel".into()) },
            Event::ContentDelta { index: 1, delta: Delta::Text("lo".into()) },
            Event::ContentStop { index: 1 },
            Event::ContentStart { index: 2, block: BlockStart::ToolCall { id: "t1".into(), name: "f".into() } },
            Event::ContentDelta { index: 2, delta: Delta::ToolArguments("{\"a\":".into()) },
            Event::ContentDelta { index: 2, delta: Delta::ToolArguments("1}".into()) },
            Event::ContentStop { index: 2 },
            Event::ContentStart { index: 3, block: BlockStart::ToolCall { id: "t2".into(), name: "g".into() } },
            Event::ContentStop { index: 3 },
            Event::MessageStop {
                finish: Finish::ToolCalls,
                native_finish: "tool_use".into(),
                usage: Some(Usage { input_tokens: 3, output_tokens: 4, ..Default::default() }),
            },
        ]
    }

    #[test]
    fn a_stream_is_written_as_chunks() {
        let text = written(&stream_events(), true, Some(json!({ "amount": 0.25 })));
        let chunks: Vec<Value> = text
            .split("\n\n")
            .filter(|e| !e.is_empty() && *e != "data: [DONE]")
            .map(|e| serde_json::from_str(e.strip_prefix("data: ").unwrap()).unwrap())
            .collect();
        assert!(text.ends_with("data: [DONE]\n\n"));
        let delta = |i: usize| chunks[i]["choices"][0]["delta"].clone();
        assert_eq!(delta(0), json!({ "role": "assistant", "content": "" }));
        assert_eq!(chunks[0]["id"], "msg_9", "the provider's id, since the stamp named none");
        assert_eq!((chunks[0]["model"].as_str(), chunks[0]["created"].as_u64()), (Some("repo/model"), Some(42)));
        assert_eq!(delta(1), json!({ "reasoning_content": "hm " }));
        assert_eq!(
            delta(3),
            json!({ "provider_specific_fields": { "thinking_blocks": [{ "type": "thinking", "thinking": "hm ok", "signature": "sig==" }] } })
        );
        assert_eq!(delta(4), json!({ "content": "Hel" }));
        assert_eq!(
            delta(6),
            json!({ "tool_calls": [{ "index": 0, "id": "t1", "type": "function", "function": { "name": "f", "arguments": "" } }] })
        );
        assert_eq!(delta(7), json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "{\"a\":" } }] }));
        assert_eq!(delta(9)["tool_calls"][0]["index"], 1, "tool calls are numbered on their own");
        let last = chunks.len() - 1;
        assert_eq!(chunks[last - 1]["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(chunks[last]["choices"], json!([]));
        assert_eq!(chunks[last]["usage"]["cost"], json!({ "amount": 0.25 }));
        assert!(chunks[..last].iter().all(|c| c.get("usage").is_none()));

        // Without the caller asking, no usage chunk; the same events give the same bytes.
        let quiet = written(&stream_events(), false, None);
        assert!(!quiet.contains("\"usage\"") && quiet.ends_with("data: [DONE]\n\n"));
        assert_eq!(quiet, written(&stream_events(), false, None));

        // An error ends the stream without the end marker.
        let failed = written(&[Event::Error { kind: "upstream_error".into(), message: "broke".into() }], true, None);
        assert_eq!(
            failed,
            "data: {\"error\":{\"message\":\"broke\",\"type\":\"api_error\",\"param\":null,\"code\":\"upstream_error\"}}\n\n"
        );
    }

    #[test]
    fn provider_blocks_of_a_stream_are_given_whole() {
        let events = vec![
            Event::MessageStart { id: "m".into(), model: "c".into() },
            Event::ContentStart {
                index: 0,
                block: BlockStart::Provider {
                    family: "anthropic".into(),
                    raw: json!({ "type": "server_tool_use", "id": "s1", "name": "web_search", "input": {} }),
                },
            },
            Event::ContentDelta {
                index: 0,
                delta: Delta::Provider(json!({ "type": "input_json_delta", "partial_json": "{\"query\":" })),
            },
            Event::ContentDelta {
                index: 0,
                delta: Delta::Provider(json!({ "type": "input_json_delta", "partial_json": "\"x\"}" })),
            },
            Event::ContentStop { index: 0 },
            Event::ContentStart {
                index: 1,
                block: BlockStart::Provider {
                    family: "anthropic".into(),
                    raw: json!({ "type": "web_search_tool_result", "tool_use_id": "s1", "content": [] }),
                },
            },
            Event::ContentStop { index: 1 },
            Event::ContentStart { index: 2, block: BlockStart::Text },
            Event::ContentDelta { index: 2, delta: Delta::Citation(json!({ "url": "https://x.test" })) },
        ];
        let text = written(&events, false, None);
        assert!(
            text.contains(
                r#""provider_blocks":[{"type":"server_tool_use","id":"s1","name":"web_search","input":{"query":"x"}}]"#
            ),
            "{text}"
        );
        assert!(
            text.contains(
                r#""web_search_results":[{"type":"web_search_tool_result","tool_use_id":"s1","content":[]}]"#
            )
        );
        assert!(text.contains(r#""citations":[{"url":"https://x.test"}]"#));
    }

    fn read(chunks: &[&str]) -> Vec<Event> {
        let mut r = provider_stream(&profile());
        let mut out = Vec::new();
        for c in chunks {
            r.read("", c.as_bytes(), &mut out);
        }
        r.finish(&mut out);
        out
    }

    #[test]
    fn a_provider_stream_is_read_into_neutral_events() {
        let events = read(&[
            r#"{"id":"chatcmpl-1","model":"gpt-x","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}],"usage":null}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"think"}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hi"}}]}"#,
            r#"{"choices":[{"delta":{"content":" there"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"f","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"c2","type":"function","function":{"name":"g","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\":1}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":9,"completion_tokens":4}}"#,
            "[DONE]",
        ]);
        assert_eq!(
            events,
            vec![
                Event::MessageStart { id: "chatcmpl-1".into(), model: "gpt-x".into() },
                Event::ContentStart { index: 0, block: BlockStart::Reasoning },
                Event::ContentDelta { index: 0, delta: Delta::Reasoning("think".into()) },
                Event::ContentStop { index: 0 },
                Event::ContentStart { index: 1, block: BlockStart::Text },
                Event::ContentDelta { index: 1, delta: Delta::Text("Hi".into()) },
                Event::ContentDelta { index: 1, delta: Delta::Text(" there".into()) },
                Event::ContentStop { index: 1 },
                Event::ContentStart { index: 2, block: BlockStart::ToolCall { id: "c1".into(), name: "f".into() } },
                Event::ContentStart { index: 3, block: BlockStart::ToolCall { id: "c2".into(), name: "g".into() } },
                Event::ContentDelta { index: 3, delta: Delta::ToolArguments("{}".into()) },
                Event::ContentDelta { index: 2, delta: Delta::ToolArguments("{\"a\":1}".into()) },
                Event::ContentStop { index: 2 },
                Event::ContentStop { index: 3 },
                Event::MessageStop {
                    finish: Finish::ToolCalls,
                    native_finish: "tool_calls".into(),
                    usage: Some(Usage { input_tokens: 9, output_tokens: 4, ..Default::default() }),
                },
            ]
        );

        // A stream that ends without usage and without [DONE] still stops exactly once.
        let short = read(&[r#"{"id":"x","model":"m","choices":[{"delta":{"content":"a"}}]}"#]);
        assert_eq!(short.iter().filter(|e| matches!(e, Event::MessageStop { .. })).count(), 1);
        assert!(matches!(short.last(), Some(Event::MessageStop { usage: None, finish: Finish::Stop, .. })));
        // An error in the stream ends it; garbage is skipped; an empty stream gives nothing.
        let failed = read(&[
            "not json",
            r#"{"error":{"message":"overloaded"}}"#,
            r#"{"choices":[{"delta":{"content":"late"}}]}"#,
        ]);
        assert_eq!(failed, vec![Event::Error { kind: "upstream_error".into(), message: "overloaded".into() }]);
        assert!(read(&[]).is_empty());
    }

    #[test]
    fn thinking_off_is_not_part_of_this_dialect() {
        // The profile's thinking_off belongs to Anthropic models; it must not leak in here.
        let req = Request {
            params: crate::canonical::Params {
                reasoning: crate::canonical::Reasoning { effort: Some("none".into()), ..Default::default() },
                ..Default::default()
            },
            ..Default::default()
        };
        let p = Profile {
            thinking_off: ThinkingOff::BetweenTools,
            reasoning_efforts: vec!["none".into(), "low".into()],
            ..profile()
        };
        let (v, notes) = build(&req, &p);
        assert_eq!(v["reasoning_effort"], "none");
        assert!(v.get("thinking").is_none() && notes == Notes::default());
    }
}
