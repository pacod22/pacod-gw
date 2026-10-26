//! OpenAI's Responses API (`/v1/responses`), stateless (concept section 9.1, ING-05).
//!
//! Client half: `parse_request`, `write_response`, `client_stream`. A Responses caller is
//! served by a model at Anthropic, Gemini or a chat-completions endpoint through these.
//!
//! Provider half: `read_response`, `provider_stream`. They read what a provider that serves the
//! Responses API answered a caller of the same API, for the response cache. The gateway never
//! translates another dialect's caller to `/responses`.
//!
//! What reaches these halves is stateless: `store`, `previous_response_id`, `conversation`,
//! `background`, a stored `prompt` and item references are refused ([`stateful`]), and a
//! translated answer says `store: false`. Where a workspace keeps responses, the core expands a
//! call that continues one into the call its caller could have sent without it, and
//! [`stamp`]s the answer with what it kept. Reasoning crosses turns in `encrypted_content`:
//! OpenAI's own as OpenAI wrote it, any other family's labelled (`gw1.<family>.<raw>`), so that
//! each provider is given back only its own.

use std::collections::BTreeMap;

use bytes::BytesMut;
use pacod_gw_events::Usage;
use serde_json::{Map, Value, json};

use crate::assemble::Assembler;
use crate::canonical::{
    Block, BlockStart, Delta, Event, Finish, Message, Profile, Request, Response, ResponseFormat, Role,
    SIGNATURE_PREFIX, Stamp, Tool, ToolChoice, sign, signed_by,
};
use crate::error::DialectError;
use crate::sse::write_event;
use crate::util::{arguments_text, image_from_url, object, str_of, take_bool, take_f64, take_string, take_u64};
use crate::{ClientStream, ProviderStream};

/// The family name of blocks and signatures that only the Responses API understands.
pub const FAMILY: &str = "openai_responses";
/// OpenAI's own reasoning, labelled in neutral terms.
const OPENAI: &str = "openai";
/// Anthropic's signatures are unlabelled in neutral terms; on this dialect they carry a label.
const ANTHROPIC: &str = "anthropic";
/// Anthropic's redacted thinking, which has no signature of its own.
const ANTHROPIC_REDACTED: &str = "anthropic_redacted";

/// The request members that ask the provider to keep or reuse state, with why each is
/// refused. `None` when the request is stateless.
pub fn stateful(body: &Value) -> Option<String> {
    if body.get("store").is_some_and(|v| v != &Value::Bool(false) && !v.is_null()) {
        return Some("store: this gateway keeps no responses; send store: false or leave it out".into());
    }
    if body.get("background").is_some_and(|v| v == &Value::Bool(true)) {
        return Some("background: this gateway does not run responses in the background".into());
    }
    reuses_state(body)
}

/// The request members that reuse what the provider keeps for the credential: an earlier
/// response, a conversation, a stored prompt, an item. On a credential several workspaces
/// share, they would reach another workspace's.
pub fn reuses_state(body: &Value) -> Option<String> {
    let set = |key: &str| body.get(key).is_some_and(|v| !v.is_null());
    if set("previous_response_id") {
        return Some(
            "previous_response_id: this gateway keeps no responses; send the conversation in input instead".into(),
        );
    }
    if set("conversation") {
        return Some(
            "conversation: this gateway keeps no conversations; send the conversation in input instead".into(),
        );
    }
    if set("prompt") {
        return Some("prompt: stored prompts live at the provider; send the instructions and input instead".into());
    }
    let referenced = body
        .get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|i| str_of(i, "type") == Some("item_reference")));
    if referenced {
        return Some("item_reference: this gateway keeps no items; send the item itself".into());
    }
    None
}

// --- reasoning across turns ----------------------------------------------------------------------

/// A reasoning signature in neutral terms, from the `encrypted_content` of an item.
fn signature_in(encrypted: &str) -> (Option<String>, Option<String>) {
    if let Some(raw) = signed_by(encrypted, ANTHROPIC_REDACTED) {
        return (None, Some(raw.to_owned()));
    }
    if let Some(raw) = signed_by(encrypted, ANTHROPIC) {
        return (Some(raw.to_owned()), None);
    }
    if encrypted.starts_with(SIGNATURE_PREFIX) {
        return (Some(encrypted.to_owned()), None);
    }
    // Unlabelled: OpenAI's own, as it wrote it.
    (Some(sign(OPENAI, encrypted)), None)
}

/// The `encrypted_content` of a reasoning item for a neutral signature: OpenAI's unlabelled,
/// everything else labelled with its family.
fn encrypted_out(signature: Option<&str>, redacted: Option<&str>) -> Option<String> {
    if let Some(data) = redacted {
        return Some(sign(ANTHROPIC_REDACTED, data));
    }
    let sig = signature?;
    if let Some(raw) = signed_by(sig, OPENAI) {
        return Some(raw.to_owned());
    }
    if sig.starts_with(SIGNATURE_PREFIX) { Some(sig.to_owned()) } else { Some(sign(ANTHROPIC, sig)) }
}

// --- client half: reading a caller's request -----------------------------------------------------

fn unsupported_item(kind: &str) -> DialectError {
    DialectError::unsupported(format!(
        "the input item type {kind} needs OpenAI's Responses API; this model is served through another API"
    ))
}

/// A content part of a message.
fn parse_part(part: &Value) -> Result<Option<Block>, DialectError> {
    Ok(Some(match str_of(part, "type") {
        Some("input_text" | "output_text" | "text") => Block::Text {
            text: str_of(part, "text").unwrap_or_default().to_owned(),
            citations: part.get("annotations").filter(|a| a.as_array().is_some_and(|a| !a.is_empty())).cloned(),
            cache: None,
        },
        Some("refusal") => Block::text(str_of(part, "refusal").unwrap_or_default()),
        Some("input_image") => match (str_of(part, "image_url"), str_of(part, "file_id")) {
            (Some(url), _) if !url.is_empty() => Block::Image { source: image_from_url(url), cache: None },
            (_, Some(_)) => {
                return Err(DialectError::unsupported(
                    "an input_image with a file_id names a file at OpenAI, which this model cannot read; send image_url",
                ));
            }
            _ => return Err(DialectError::invalid("an input_image needs an image_url")),
        },
        Some("input_file") => {
            let mut body = Map::new();
            match (str_of(part, "file_data"), str_of(part, "file_url"), str_of(part, "file_id")) {
                (Some(data), _, _) => {
                    let (media_type, data) = match crate::util::parse_data_url(data) {
                        Some(parsed) => parsed,
                        // The SDKs also send the bare base64 of a PDF.
                        None => ("application/pdf".to_owned(), data.to_owned()),
                    };
                    body.insert("source".into(), json!({ "type": "base64", "media_type": media_type, "data": data }));
                }
                (None, Some(url), _) => {
                    body.insert("source".into(), json!({ "type": "url", "url": url }));
                }
                (None, None, Some(_)) => {
                    return Err(DialectError::unsupported(
                        "an input_file with a file_id names a file at OpenAI, which this model cannot read; send file_data",
                    ));
                }
                _ => return Err(DialectError::invalid("an input_file needs file_data or file_url")),
            }
            if let Some(name) = str_of(part, "filename") {
                body.insert("title".into(), json!(name));
            }
            Block::Document { body, cache: None }
        }
        Some("input_audio") => return Err(DialectError::unsupported("audio input is not supported")),
        Some(other) => return Err(DialectError::invalid(format!("\"{other}\" is not a content part of a message"))),
        None => return Err(DialectError::invalid("every content part needs a type")),
    }))
}

fn parse_content(content: Option<&Value>) -> Result<Vec<Block>, DialectError> {
    match content {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) if s.is_empty() => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![Block::text(s.clone())]),
        Some(Value::Array(parts)) => {
            let mut out = Vec::with_capacity(parts.len());
            for p in parts {
                out.extend(parse_part(p)?);
            }
            Ok(out)
        }
        Some(_) => Err(DialectError::invalid("content must be a string or a list of parts")),
    }
}

/// Appends blocks to the conversation's last assistant message, or opens one: a turn of the
/// Responses API is a run of items, and the neutral model keeps a turn in one message.
fn assistant(req: &mut Request, blocks: Vec<Block>) {
    match req.messages.last_mut() {
        Some(Message { role: Role::Assistant, content }) => content.extend(blocks),
        _ => req.messages.push(Message { role: Role::Assistant, content: blocks }),
    }
}

fn parse_item(item: &Value, req: &mut Request) -> Result<(), DialectError> {
    let kind = str_of(item, "type").or_else(|| item.get("role").map(|_| "message"));
    match kind {
        Some("message") => match str_of(item, "role") {
            Some("system" | "developer") => req.system.extend(parse_content(item.get("content"))?),
            Some("user") => {
                req.messages.push(Message { role: Role::User, content: parse_content(item.get("content"))? })
            }
            Some("assistant") => assistant(req, parse_content(item.get("content"))?),
            Some(other) => return Err(DialectError::invalid(format!("\"{other}\" is not a message role"))),
            None => return Err(DialectError::invalid("every message needs a role")),
        },
        Some("function_call") => assistant(
            req,
            vec![Block::ToolCall {
                id: str_of(item, "call_id").unwrap_or_default().to_owned(),
                name: str_of(item, "name").unwrap_or_default().to_owned(),
                arguments: arguments_text(item.get("arguments")),
                cache: None,
            }],
        ),
        Some("function_call_output") => {
            let result = Block::ToolResult {
                tool_call_id: str_of(item, "call_id").unwrap_or_default().to_owned(),
                content: parse_content(item.get("output"))?,
                is_error: false,
                cache: None,
            };
            req.messages.push(Message { role: Role::Tool, content: vec![result] });
        }
        Some("reasoning") => {
            let text = item
                .get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|s| str_of(s, "text"))
                .collect::<Vec<_>>()
                .join("\n\n");
            let (signature, redacted) = match str_of(item, "encrypted_content") {
                Some(e) if !e.is_empty() => signature_in(e),
                _ => (None, None),
            };
            assistant(req, vec![Block::Reasoning { text, signature, redacted }]);
        }
        Some(other) => return Err(unsupported_item(other)),
        None => return Err(DialectError::invalid("every input item needs a type")),
    }
    Ok(())
}

fn parse_tools(v: Option<Value>) -> Result<Vec<Tool>, DialectError> {
    let mut out = Vec::new();
    for tool in v.and_then(|v| v.as_array().cloned()).unwrap_or_default() {
        match str_of(&tool, "type") {
            Some("function") => out.push(Tool::Function {
                name: str_of(&tool, "name").unwrap_or_default().to_owned(),
                description: str_of(&tool, "description").map(str::to_owned),
                parameters: tool
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                strict: tool.get("strict").and_then(Value::as_bool),
                cache: None,
            }),
            Some(other) => {
                return Err(DialectError::unsupported(format!(
                    "the tool type {other} needs OpenAI's Responses API; this model is served through another API"
                )));
            }
            None => return Err(DialectError::invalid("every tool needs a type")),
        }
    }
    Ok(out)
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
        Some(o) => match (str_of(&o, "type"), str_of(&o, "name")) {
            (Some("function"), Some(name)) => Some(ToolChoice::Tool(name.to_owned())),
            (Some(other), _) => {
                return Err(DialectError::unsupported(format!(
                    "the tool_choice {other} needs OpenAI's Responses API; this model is served through another API"
                )));
            }
            _ => return Err(DialectError::invalid("tool_choice must name a function")),
        },
    })
}

/// `text.format`, and what else `text` holds (`verbosity`), which has no neutral meaning.
fn parse_text(v: Option<Value>, extra: &mut Map<String, Value>) -> Result<Option<ResponseFormat>, DialectError> {
    let Some(Value::Object(mut text)) = v else { return Ok(None) };
    let format = match text.shift_remove("format") {
        None | Some(Value::Null) => None,
        Some(f) => Some(match str_of(&f, "type") {
            Some("text") => ResponseFormat::Text,
            Some("json_object") => ResponseFormat::JsonObject,
            Some("json_schema") => ResponseFormat::JsonSchema {
                name: str_of(&f, "name").map(str::to_owned),
                schema: f.get("schema").cloned().ok_or_else(|| DialectError::invalid("text.format needs a schema"))?,
                strict: f.get("strict").and_then(Value::as_bool),
            },
            _ => return Err(DialectError::invalid("text.format.type must be text, json_object or json_schema")),
        }),
    };
    for (k, v) in text {
        extra.insert(format!("text.{k}"), v);
    }
    Ok(format)
}

/// Client half: reads a Responses request. A request that asks for state is refused first,
/// by [`stateful`]; what remains of it here is left out.
pub fn parse_request(body: &[u8]) -> Result<Request, DialectError> {
    let mut top = object(body)?;
    if let Some(why) = stateful(&Value::Object(top.clone())) {
        return Err(DialectError::invalid(why));
    }
    // Responses always reports usage at the end of a stream.
    let mut req =
        Request { model: take_string(&mut top, "model").unwrap_or_default(), stream_usage: true, ..Default::default() };
    if let Some(i) = take_string(&mut top, "instructions").filter(|i| !i.is_empty()) {
        req.system.push(Block::text(i));
    }
    match top.shift_remove("input") {
        Some(Value::String(s)) => req.messages.push(Message { role: Role::User, content: vec![Block::text(s)] }),
        Some(Value::Array(items)) => {
            for item in &items {
                parse_item(item, &mut req)?;
            }
        }
        _ => return Err(DialectError::invalid("input is required: a string or a list of items")),
    }
    req.tools = parse_tools(top.shift_remove("tools"))?;
    req.tool_choice = parse_tool_choice(top.shift_remove("tool_choice"))?;
    let mut extra = Map::new();
    req.response_format = parse_text(top.shift_remove("text"), &mut extra)?;
    req.stream = take_bool(&mut top, "stream").unwrap_or(false);

    let p = &mut req.params;
    p.max_output_tokens = take_u64(&mut top, "max_output_tokens");
    p.temperature = take_f64(&mut top, "temperature");
    p.top_p = take_f64(&mut top, "top_p");
    if let Some(Value::Object(reasoning)) = top.shift_remove("reasoning") {
        p.reasoning.effort = reasoning.get("effort").and_then(Value::as_str).map(str::to_owned);
        if reasoning.get("summary").and_then(Value::as_str).is_some_and(|s| s != "none") {
            p.reasoning.summary = Some(true);
        }
    }
    req.user =
        take_string(&mut top, "safety_identifier").or_else(|| take_string(&mut top, "user")).filter(|u| !u.is_empty());
    top.shift_remove("user");
    req.gateway = top.shift_remove("gateway");
    // Stateless already (`stateful` let them through): nothing to forward.
    for key in ["store", "background", "previous_response_id", "conversation", "prompt", "include", "stream_options"] {
        top.shift_remove(key);
    }
    extra.extend(top);
    req.extra = extra;
    Ok(req)
}

// --- client half: writing an answer --------------------------------------------------------------

/// The id of the response, and the base of its items' ids: OpenAI's ids start with `resp_`.
fn ids(stamp: &Stamp) -> (String, String) {
    let base = stamp.id.strip_prefix("resp_").unwrap_or(&stamp.id).to_owned();
    (format!("resp_{base}"), base)
}

fn status_of(finish: Finish) -> (&'static str, Value) {
    match finish {
        Finish::Length => ("incomplete", json!({ "reason": "max_output_tokens" })),
        Finish::ContentFilter => ("incomplete", json!({ "reason": "content_filter" })),
        Finish::Stop | Finish::ToolCalls | Finish::Paused => ("completed", Value::Null),
    }
}

/// Usage in the Responses API's shape: `input_tokens` counts cached tokens too.
pub fn usage_value(u: &Usage, cost: Option<&Value>) -> Value {
    let input = u.prompt_total();
    let mut out = json!({
        "input_tokens": input,
        "input_tokens_details": { "cached_tokens": u.cache_read_tokens },
        "output_tokens": u.output_tokens,
        "output_tokens_details": { "reasoning_tokens": u.reasoning_tokens },
        "total_tokens": input + u.output_tokens,
    });
    if let Some(c) = cost {
        out["cost"] = c.clone();
    }
    out
}

/// The request members a response repeats, as the caller sent them, with OpenAI's defaults.
fn echoed(stamp: &Stamp, key: &str, default: Value) -> Value {
    stamp.echo.as_ref().and_then(|e| e.get(key)).cloned().unwrap_or(default)
}

/// The response object without its output: what a stream's first and last events carry.
fn envelope(stamp: &Stamp, status: &str, created: u64) -> Map<String, Value> {
    let (id, _) = ids(stamp);
    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    out.insert("object".into(), json!("response"));
    out.insert("created_at".into(), json!(created));
    out.insert("status".into(), json!(status));
    out.insert("background".into(), json!(false));
    out.insert("error".into(), Value::Null);
    out.insert("incomplete_details".into(), Value::Null);
    out.insert("instructions".into(), echoed(stamp, "instructions", Value::Null));
    out.insert("max_output_tokens".into(), echoed(stamp, "max_output_tokens", Value::Null));
    out.insert("model".into(), json!(stamp.model));
    out.insert("output".into(), json!([]));
    out.insert("parallel_tool_calls".into(), echoed(stamp, "parallel_tool_calls", json!(true)));
    out.insert("previous_response_id".into(), Value::Null);
    out.insert("reasoning".into(), echoed(stamp, "reasoning", json!({ "effort": null, "summary": null })));
    out.insert("store".into(), json!(false));
    out.insert("temperature".into(), echoed(stamp, "temperature", Value::Null));
    out.insert("text".into(), echoed(stamp, "text", json!({ "format": { "type": "text" } })));
    out.insert("tool_choice".into(), echoed(stamp, "tool_choice", json!("auto")));
    out.insert("tools".into(), echoed(stamp, "tools", json!([])));
    out.insert("top_p".into(), echoed(stamp, "top_p", Value::Null));
    out.insert("truncation".into(), echoed(stamp, "truncation", json!("disabled")));
    out.insert("usage".into(), Value::Null);
    out.insert("metadata".into(), echoed(stamp, "metadata", json!({})));
    out
}

/// The items of an answer, one per block that the Responses API can show. Ids derive from the
/// response's id and the item's place, so the same answer always gives the same bytes.
fn items(content: &[Block], base: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for block in content {
        let n = out.len();
        match block {
            Block::Text { text, citations, .. } => {
                out.push(message_item(base, n, text, citations.as_ref(), "completed"))
            }
            Block::Reasoning { text, signature, redacted } => {
                out.push(reasoning_item(base, n, text, signature.as_deref(), redacted.as_deref()));
            }
            Block::ToolCall { id, name, arguments, .. } => {
                out.push(call_item(base, n, id, name, arguments, "completed"))
            }
            Block::Provider { family, raw } if family == FAMILY => out.push(raw.clone()),
            Block::Provider { .. } | Block::Image { .. } | Block::Document { .. } | Block::ToolResult { .. } => {}
        }
    }
    out
}

/// Whether a block becomes an item of the answer (see [`items`]).
fn shown(block: &BlockStart) -> bool {
    match block {
        BlockStart::Provider { family, .. } => family == FAMILY,
        BlockStart::Text | BlockStart::Reasoning | BlockStart::ToolCall { .. } => true,
    }
}

fn text_part(text: &str, citations: Option<&Value>) -> Value {
    json!({ "type": "output_text", "text": text, "annotations": citations.cloned().unwrap_or_else(|| json!([])), "logprobs": [] })
}

fn message_item(base: &str, n: usize, text: &str, citations: Option<&Value>, status: &str) -> Value {
    json!({
        "id": format!("msg_{base}_{n}"),
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": if status == "completed" { json!([text_part(text, citations)]) } else { json!([]) },
    })
}

fn reasoning_item(base: &str, n: usize, text: &str, signature: Option<&str>, redacted: Option<&str>) -> Value {
    let mut item = json!({
        "id": format!("rs_{base}_{n}"),
        "type": "reasoning",
        "summary": if text.is_empty() { json!([]) } else { json!([{ "type": "summary_text", "text": text }]) },
    });
    if let Some(e) = encrypted_out(signature, redacted) {
        item["encrypted_content"] = json!(e);
    }
    item
}

fn call_item(base: &str, n: usize, call_id: &str, name: &str, arguments: &str, status: &str) -> Value {
    json!({
        "id": format!("fc_{base}_{n}"),
        "type": "function_call",
        "status": status,
        "arguments": if arguments.is_empty() && status == "completed" { "{}" } else { arguments },
        "call_id": call_id,
        "name": name,
    })
}

/// Client half: writes a `response` object.
pub fn write_response(response: &Response, stamp: &Stamp) -> Value {
    let (_, base) = ids(stamp);
    let (status, incomplete) = status_of(response.finish);
    let mut out = envelope(stamp, status, stamp.created);
    out.insert("incomplete_details".into(), incomplete);
    out.insert("output".into(), Value::Array(items(&response.content, &base)));
    if let Some(u) = &response.usage {
        out.insert("usage".into(), usage_value(u, stamp.cost.as_ref()));
    }
    Value::Object(out)
}

/// An item of the stream that is open, by the neutral block's index.
enum Open {
    Text { item: usize, text: String, annotations: Vec<Value> },
    Reasoning { item: usize, text: String, signature: Option<String>, part: bool },
    Tool { item: usize, id: String, name: String, arguments: String },
    Hidden,
}

struct Writer {
    stamp: Stamp,
    sequence: u64,
    /// Items opened so far: the output's indexes.
    items: usize,
    open: BTreeMap<u32, Open>,
    /// The whole answer, for the closing event.
    whole: Assembler,
}

impl Writer {
    fn emit(&mut self, out: &mut BytesMut, kind: &str, mut body: Value) {
        body["type"] = json!(kind);
        body["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        // The type first, as OpenAI writes it.
        let mut ordered = Map::new();
        ordered.insert("type".into(), json!(kind));
        if let Value::Object(rest) = body {
            for (k, v) in rest {
                if k != "type" {
                    ordered.insert(k, v);
                }
            }
        }
        write_event(out, kind, Value::Object(ordered).to_string().as_bytes());
    }

    fn base(&self) -> String {
        ids(&self.stamp).1
    }

    fn item_id(&self, prefix: &str, item: usize) -> String {
        format!("{prefix}_{}_{item}", self.base())
    }
}

impl ClientStream for Writer {
    fn finalize(&mut self, cost: Option<Value>, _gateway: Option<Value>) {
        self.stamp.cost = cost;
    }

    fn write(&mut self, event: &Event, out: &mut BytesMut) {
        self.whole.push(event);
        match event {
            Event::MessageStart { id, model } => {
                if self.stamp.id.is_empty() {
                    self.stamp.id = id.clone();
                }
                if self.stamp.model.is_empty() {
                    self.stamp.model = model.clone();
                }
                let response = Value::Object(envelope(&self.stamp, "in_progress", self.stamp.created));
                self.emit(out, "response.created", json!({ "response": response.clone() }));
                self.emit(out, "response.in_progress", json!({ "response": response }));
            }
            Event::ContentStart { index, block } => {
                if !shown(block) {
                    self.open.insert(*index, Open::Hidden);
                    return;
                }
                let item = self.items;
                self.items += 1;
                let base = self.base();
                let open = match block {
                    BlockStart::Text => {
                        let added = message_item(&base, item, "", None, "in_progress");
                        self.emit(out, "response.output_item.added", json!({ "output_index": item, "item": added }));
                        let part = json!({ "type": "output_text", "text": "", "annotations": [], "logprobs": [] });
                        let item_id = self.item_id("msg", item);
                        self.emit(
                            out,
                            "response.content_part.added",
                            json!({ "item_id": item_id, "output_index": item, "content_index": 0, "part": part }),
                        );
                        Open::Text { item, text: String::new(), annotations: Vec::new() }
                    }
                    BlockStart::Reasoning => {
                        let added = json!({ "id": self.item_id("rs", item), "type": "reasoning", "summary": [] });
                        self.emit(out, "response.output_item.added", json!({ "output_index": item, "item": added }));
                        Open::Reasoning { item, text: String::new(), signature: None, part: false }
                    }
                    BlockStart::ToolCall { id, name } => {
                        let added = call_item(&base, item, id, name, "", "in_progress");
                        self.emit(out, "response.output_item.added", json!({ "output_index": item, "item": added }));
                        Open::Tool { item, id: id.clone(), name: name.clone(), arguments: String::new() }
                    }
                    BlockStart::Provider { raw, .. } => {
                        self.emit(out, "response.output_item.added", json!({ "output_index": item, "item": raw }));
                        Open::Hidden
                    }
                };
                self.open.insert(*index, open);
            }
            Event::ContentDelta { index, delta } => {
                let base = self.base();
                let Some(open) = self.open.get_mut(index) else { return };
                let emitted: Option<(&str, Value)> = match (open, delta) {
                    (Open::Text { item, text, .. }, Delta::Text(t)) => {
                        text.push_str(t);
                        Some((
                            "response.output_text.delta",
                            json!({ "item_id": format!("msg_{base}_{item}"), "output_index": *item, "content_index": 0, "delta": t, "logprobs": [] }),
                        ))
                    }
                    (Open::Text { item, annotations, .. }, Delta::Citation(c)) => {
                        annotations.push(c.clone());
                        Some((
                            "response.output_text.annotation.added",
                            json!({ "item_id": format!("msg_{base}_{item}"), "output_index": *item, "content_index": 0,
                                    "annotation_index": annotations.len() - 1, "annotation": c }),
                        ))
                    }
                    (Open::Reasoning { item, text, part, .. }, Delta::Reasoning(t)) => {
                        let item = *item;
                        let first = !*part;
                        *part = true;
                        text.push_str(t);
                        if first {
                            let id = format!("rs_{base}_{item}");
                            self.emit(
                                out,
                                "response.reasoning_summary_part.added",
                                json!({ "item_id": id, "output_index": item, "summary_index": 0,
                                        "part": { "type": "summary_text", "text": "" } }),
                            );
                        }
                        Some((
                            "response.reasoning_summary_text.delta",
                            json!({ "item_id": format!("rs_{base}_{item}"), "output_index": item, "summary_index": 0, "delta": t }),
                        ))
                    }
                    (Open::Reasoning { signature, .. }, Delta::Signature(s)) => {
                        *signature = Some(s.clone());
                        None
                    }
                    (Open::Tool { item, arguments, .. }, Delta::ToolArguments(a)) => {
                        arguments.push_str(a);
                        Some((
                            "response.function_call_arguments.delta",
                            json!({ "item_id": format!("fc_{base}_{item}"), "output_index": *item, "delta": a }),
                        ))
                    }
                    _ => None,
                };
                if let Some((kind, body)) = emitted {
                    self.emit(out, kind, body);
                }
            }
            Event::ContentStop { index } => {
                let base = self.base();
                match self.open.remove(index) {
                    Some(Open::Text { item, text, annotations }) => {
                        let id = format!("msg_{base}_{item}");
                        let citations = (!annotations.is_empty()).then(|| Value::Array(annotations));
                        self.emit(
                            out,
                            "response.output_text.done",
                            json!({ "item_id": id, "output_index": item, "content_index": 0, "text": text, "logprobs": [] }),
                        );
                        let part = text_part(&text, citations.as_ref());
                        self.emit(
                            out,
                            "response.content_part.done",
                            json!({ "item_id": id, "output_index": item, "content_index": 0, "part": part }),
                        );
                        let done = message_item(&base, item, &text, citations.as_ref(), "completed");
                        self.emit(out, "response.output_item.done", json!({ "output_index": item, "item": done }));
                    }
                    Some(Open::Reasoning { item, text, signature, part }) => {
                        let id = format!("rs_{base}_{item}");
                        if part {
                            self.emit(
                                out,
                                "response.reasoning_summary_text.done",
                                json!({ "item_id": id, "output_index": item, "summary_index": 0, "text": text }),
                            );
                            self.emit(
                                out,
                                "response.reasoning_summary_part.done",
                                json!({ "item_id": id, "output_index": item, "summary_index": 0,
                                        "part": { "type": "summary_text", "text": text } }),
                            );
                        }
                        let done = reasoning_item(&base, item, &text, signature.as_deref(), None);
                        self.emit(out, "response.output_item.done", json!({ "output_index": item, "item": done }));
                    }
                    Some(Open::Tool { item, id, name, arguments }) => {
                        let item_id = format!("fc_{base}_{item}");
                        let arguments = if arguments.is_empty() { "{}".to_owned() } else { arguments };
                        self.emit(
                            out,
                            "response.function_call_arguments.done",
                            json!({ "item_id": item_id, "output_index": item, "arguments": arguments }),
                        );
                        let done = call_item(&base, item, &id, &name, &arguments, "completed");
                        self.emit(out, "response.output_item.done", json!({ "output_index": item, "item": done }));
                    }
                    Some(Open::Hidden) | None => {}
                }
            }
            Event::MessageStop { .. } => {
                let whole = std::mem::take(&mut self.whole).finish();
                let Some(response) = whole else { return };
                let body = write_response(&response, &self.stamp);
                let kind = if body["status"] == "incomplete" { "response.incomplete" } else { "response.completed" };
                self.emit(out, kind, json!({ "response": body }));
            }
            Event::Ping => {}
            Event::Error { kind, message } => {
                self.emit(out, "error", json!({ "code": kind, "message": message, "param": null }));
            }
        }
    }
}

/// Client half: a writer of the Responses API's stream events. Usage is always on the last.
pub fn client_stream(stamp: Stamp) -> Box<dyn ClientStream> {
    Box::new(Writer { stamp, sequence: 0, items: 0, open: BTreeMap::new(), whole: Assembler::new() })
}

// --- provider half: reading the provider's answer ------------------------------------------------

/// Usage as the Responses API reports it.
pub fn read_usage(u: &Value) -> Option<Usage> {
    let input = u.get("input_tokens")?.as_u64()?;
    let cached = u.pointer("/input_tokens_details/cached_tokens").and_then(Value::as_u64).unwrap_or(0);
    Some(Usage {
        input_tokens: input.saturating_sub(cached),
        cache_read_tokens: cached,
        output_tokens: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        reasoning_tokens: u.pointer("/output_tokens_details/reasoning_tokens").and_then(Value::as_u64).unwrap_or(0),
        ..Default::default()
    })
}

/// Why a response ended, in neutral terms and the provider's words.
fn finish_of(response: &Value, called: bool) -> (Finish, String) {
    let status = str_of(response, "status").unwrap_or("completed");
    let reason = response.pointer("/incomplete_details/reason").and_then(Value::as_str);
    match (status, reason) {
        ("incomplete", Some("max_output_tokens")) => (Finish::Length, "max_output_tokens".into()),
        ("incomplete", Some("content_filter")) => (Finish::ContentFilter, "content_filter".into()),
        ("incomplete", Some(other)) => (Finish::Length, other.to_owned()),
        _ if called => (Finish::ToolCalls, status.to_owned()),
        _ => (Finish::Stop, status.to_owned()),
    }
}

fn block_of(item: &Value, out: &mut Vec<Block>) {
    match str_of(item, "type") {
        Some("message") => {
            for part in item.get("content").and_then(Value::as_array).into_iter().flatten() {
                let text = str_of(part, "text").or_else(|| str_of(part, "refusal")).unwrap_or_default();
                let citations =
                    part.get("annotations").filter(|a| a.as_array().is_some_and(|a| !a.is_empty())).cloned();
                out.push(Block::Text { text: text.to_owned(), citations, cache: None });
            }
        }
        Some("reasoning") => {
            let text = item
                .get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|s| str_of(s, "text"))
                .collect::<Vec<_>>()
                .join("\n\n");
            let (signature, redacted) = match str_of(item, "encrypted_content") {
                Some(e) if !e.is_empty() => signature_in(e),
                _ => (None, None),
            };
            out.push(Block::Reasoning { text, signature, redacted });
        }
        Some("function_call") => out.push(Block::ToolCall {
            id: str_of(item, "call_id").unwrap_or_default().to_owned(),
            name: str_of(item, "name").unwrap_or_default().to_owned(),
            arguments: arguments_text(item.get("arguments")),
            cache: None,
        }),
        // A built-in tool's call or result, which only this API understands.
        _ => out.push(Block::Provider { family: FAMILY.into(), raw: item.clone() }),
    }
}

/// Provider half: reads a whole `response` object.
pub fn read_response(body: &[u8], _profile: &Profile) -> Result<Response, DialectError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| DialectError::BadProviderResponse(format!("the answer is not JSON: {e}")))?;
    let mut content = Vec::new();
    for item in v.get("output").and_then(Value::as_array).into_iter().flatten() {
        block_of(item, &mut content);
    }
    let called = content.iter().any(|b| matches!(b, Block::ToolCall { .. }));
    let (finish, native_finish) = finish_of(&v, called);
    Ok(Response {
        id: str_of(&v, "id").unwrap_or_default().to_owned(),
        model: str_of(&v, "model").unwrap_or_default().to_owned(),
        content,
        finish,
        native_finish,
        usage: v.get("usage").and_then(read_usage),
        extra: Map::new(),
    })
}

#[derive(Default)]
struct Reader {
    /// Neutral indexes by the provider's output index and content index; an item's own
    /// block (reasoning, a call) has the content index `u64::MAX`.
    blocks: BTreeMap<(u64, u64), u32>,
    next: u32,
    called: bool,
    ended: bool,
}

const ITEM: u64 = u64::MAX;

impl Reader {
    fn open(&mut self, key: (u64, u64), block: BlockStart, out: &mut Vec<Event>) {
        let index = self.next;
        self.next += 1;
        self.blocks.insert(key, index);
        out.push(Event::ContentStart { index, block });
    }

    fn delta(&self, key: (u64, u64), delta: Delta, out: &mut Vec<Event>) {
        if let Some(index) = self.blocks.get(&key) {
            out.push(Event::ContentDelta { index: *index, delta });
        }
    }

    fn close(&mut self, key: (u64, u64), out: &mut Vec<Event>) {
        if let Some(index) = self.blocks.remove(&key) {
            out.push(Event::ContentStop { index });
        }
    }
}

impl ProviderStream for Reader {
    fn read(&mut self, _event: &str, data: &[u8], out: &mut Vec<Event>) {
        let Ok(v) = serde_json::from_slice::<Value>(data) else { return };
        let at = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let output = at("output_index");
        let text = |k: &str| str_of(&v, k).unwrap_or_default().to_owned();
        match str_of(&v, "type").unwrap_or_default() {
            "response.created" => {
                let r = v.get("response").cloned().unwrap_or(Value::Null);
                out.push(Event::MessageStart {
                    id: str_of(&r, "id").unwrap_or_default().to_owned(),
                    model: str_of(&r, "model").unwrap_or_default().to_owned(),
                });
            }
            "response.output_item.added" => {
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                match str_of(&item, "type") {
                    Some("reasoning") => self.open((output, ITEM), BlockStart::Reasoning, out),
                    Some("function_call") => {
                        self.called = true;
                        let block = BlockStart::ToolCall {
                            id: str_of(&item, "call_id").unwrap_or_default().to_owned(),
                            name: str_of(&item, "name").unwrap_or_default().to_owned(),
                        };
                        self.open((output, ITEM), block, out);
                    }
                    // A message's parts open on their own; other items once they are done.
                    _ => {}
                }
            }
            "response.content_part.added" => self.open((output, at("content_index")), BlockStart::Text, out),
            "response.output_text.delta" => self.delta((output, at("content_index")), Delta::Text(text("delta")), out),
            "response.output_text.annotation.added" => {
                let a = v.get("annotation").cloned().unwrap_or(Value::Null);
                self.delta((output, at("content_index")), Delta::Citation(a), out);
            }
            "response.content_part.done" => self.close((output, at("content_index")), out),
            "response.reasoning_summary_text.delta" => {
                self.delta((output, ITEM), Delta::Reasoning(text("delta")), out);
            }
            "response.reasoning_summary_part.added" if at("summary_index") > 0 => {
                self.delta((output, ITEM), Delta::Reasoning("\n\n".into()), out);
            }
            "response.function_call_arguments.delta" => {
                self.delta((output, ITEM), Delta::ToolArguments(text("delta")), out);
            }
            "response.output_item.done" => {
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                match str_of(&item, "type") {
                    Some("reasoning") => {
                        if let Some(e) = str_of(&item, "encrypted_content").filter(|e| !e.is_empty())
                            && let (Some(sig), _) = signature_in(e)
                        {
                            self.delta((output, ITEM), Delta::Signature(sig), out);
                        }
                        self.close((output, ITEM), out);
                    }
                    Some("function_call") => self.close((output, ITEM), out),
                    Some("message") => {}
                    _ => {
                        self.open((output, ITEM), BlockStart::Provider { family: FAMILY.into(), raw: item }, out);
                        self.close((output, ITEM), out);
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                let r = v.get("response").cloned().unwrap_or(Value::Null);
                let (finish, native_finish) = finish_of(&r, self.called);
                for (_, index) in std::mem::take(&mut self.blocks) {
                    out.push(Event::ContentStop { index });
                }
                self.ended = true;
                out.push(Event::MessageStop { finish, native_finish, usage: r.get("usage").and_then(read_usage) });
            }
            "response.failed" | "error" => {
                let error = v.pointer("/response/error").cloned().unwrap_or_else(|| v.clone());
                self.ended = true;
                out.push(Event::Error {
                    kind: str_of(&error, "code").unwrap_or("api_error").to_owned(),
                    message: str_of(&error, "message").unwrap_or("the provider's response failed").to_owned(),
                });
            }
            _ => {}
        }
    }

    fn finish(&mut self, out: &mut Vec<Event>) {
        if !self.ended {
            out.push(Event::Error { kind: "stream_ended".into(), message: "the provider's stream ended early".into() });
        }
    }
}

/// Provider half: a reader of the Responses API's stream events.
pub fn provider_stream(_profile: &Profile) -> Box<dyn ProviderStream> {
    Box::new(Reader::default())
}

/// A request's `input` as a list of items: a string is one user message. `None` when it is
/// neither.
pub fn input_items(input: &Value) -> Option<Vec<Value>> {
    match input {
        Value::String(s) => Some(vec![json!({ "type": "message", "role": "user", "content": s })]),
        Value::Array(items) => Some(items.clone()),
        _ => None,
    }
}

/// An input item as OpenAI lists it: with an id (`<prefix>_<base>_in<n>` where the caller sent
/// none), a `type` and a `status`, and a message's text as a content part.
pub fn listed_item(mut item: Value, base: &str, n: usize) -> Value {
    let Some(o) = item.as_object_mut() else { return item };
    let kind = o.get("type").and_then(Value::as_str).unwrap_or("message").to_owned();
    o.entry("type").or_insert_with(|| json!(kind));
    if !o.get("id").is_some_and(Value::is_string) {
        let prefix = match kind.as_str() {
            "message" => "msg",
            "function_call" => "fc",
            "function_call_output" => "fco",
            "reasoning" => "rs",
            _ => "item",
        };
        o.insert("id".into(), json!(format!("{prefix}_{base}_in{n}")));
    }
    if kind == "message" {
        if let Some(Value::String(text)) = o.get("content").cloned() {
            let part =
                if o.get("role").and_then(Value::as_str) == Some("assistant") { "output_text" } else { "input_text" };
            o.insert("content".into(), json!([{ "type": part, "text": text }]));
        }
        o.entry("status").or_insert_with(|| json!("completed"));
    }
    item
}

/// Puts on a response object what the gateway did with it: its own id, whether it was kept
/// (`store`), and the response it continues. Every other member stays where it is.
pub fn stamp(response: &mut Value, id: &str, store: bool, previous_response_id: Option<&str>) {
    let Some(o) = response.as_object_mut() else { return };
    o.insert("id".into(), json!(id));
    o.insert("store".into(), json!(store));
    o.insert("previous_response_id".into(), json!(previous_response_id));
}

/// The request members a response repeats, read from a request (see [`Stamp::echo`]).
pub fn echo_of(body: &[u8]) -> Option<Value> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let mut out = Map::new();
    for key in [
        "instructions",
        "max_output_tokens",
        "metadata",
        "parallel_tool_calls",
        "reasoning",
        "temperature",
        "text",
        "tool_choice",
        "tools",
        "top_p",
        "truncation",
    ] {
        if let Some(value) = v.get(key).filter(|x| !x.is_null()) {
            out.insert(key.into(), value.clone());
        }
    }
    Some(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::ImageSource;

    fn stamp() -> Stamp {
        Stamp { id: "msg_01ABC".into(), model: "claude-x".into(), created: 1_700_000_000, ..Default::default() }
    }

    #[test]
    fn state_is_refused_with_a_reason() {
        for (body, word) in [
            (json!({ "store": true }), "store"),
            (json!({ "previous_response_id": "resp_1" }), "previous_response_id"),
            (json!({ "conversation": "conv_1" }), "conversation"),
            (json!({ "background": true }), "background"),
            (json!({ "prompt": { "id": "pmpt_1" } }), "prompt"),
            (json!({ "input": [{ "type": "item_reference", "id": "msg_1" }] }), "item_reference"),
        ] {
            let why = stateful(&body).unwrap_or_else(|| panic!("{body} was let through"));
            assert!(why.starts_with(word), "{why}");
        }
        for body in [json!({ "store": false }), json!({ "background": false }), json!({ "previous_response_id": null })]
        {
            assert_eq!(stateful(&body), None, "{body}");
        }
    }

    #[test]
    fn a_conversation_of_items_reads_into_neutral_messages() {
        let body = json!({
            "model": "m",
            "instructions": "Be brief.",
            "input": [
                { "role": "developer", "content": "Answer in English." },
                { "role": "user", "content": [{ "type": "input_text", "text": "Weather?" },
                                              { "type": "input_image", "image_url": "data:image/png;base64,AAAA" }] },
                { "type": "reasoning", "id": "rs_1", "summary": [{ "type": "summary_text", "text": "Think." }],
                  "encrypted_content": "OPAQUE" },
                { "type": "function_call", "call_id": "call_1", "name": "weather", "arguments": "{\"city\":\"Bern\"}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "sunny" },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Sunny." }] },
                { "role": "user", "content": "Thanks" }
            ],
            "tools": [{ "type": "function", "name": "weather", "parameters": { "type": "object" }, "strict": true }],
            "tool_choice": { "type": "function", "name": "weather" },
            "text": { "format": { "type": "json_schema", "name": "out", "schema": { "type": "object" } }, "verbosity": "low" },
            "max_output_tokens": 100,
            "reasoning": { "effort": "low", "summary": "auto" },
            "safety_identifier": "u-1",
            "store": false,
            "metadata": { "k": "v" }
        });
        let req = parse_request(body.to_string().as_bytes()).unwrap();
        assert_eq!(req.system, vec![Block::text("Be brief."), Block::text("Answer in English.")]);
        assert_eq!(req.messages.len(), 5);
        assert_eq!(req.messages[0].role, Role::User);
        assert!(matches!(req.messages[0].content[1], Block::Image { source: ImageSource::Base64 { .. }, .. }));
        // The reasoning and the call open one assistant turn.
        assert_eq!(req.messages[1].role, Role::Assistant);
        assert_eq!(
            req.messages[1].content[0],
            Block::Reasoning { text: "Think.".into(), signature: Some(sign(OPENAI, "OPAQUE")), redacted: None }
        );
        assert!(
            matches!(&req.messages[1].content[1], Block::ToolCall { id, name, .. } if id == "call_1" && name == "weather")
        );
        assert_eq!(req.messages[2].role, Role::Tool);
        assert_eq!((req.messages[3].role, req.messages[4].role), (Role::Assistant, Role::User));
        assert_eq!(req.tool_choice, Some(ToolChoice::Tool("weather".into())));
        assert!(matches!(req.response_format, Some(ResponseFormat::JsonSchema { .. })));
        assert_eq!(req.params.max_output_tokens, Some(100));
        assert_eq!((req.params.reasoning.effort.as_deref(), req.params.reasoning.summary), (Some("low"), Some(true)));
        assert_eq!(req.user.as_deref(), Some("u-1"));
        assert!(req.stream_usage, "usage is always reported");
        // What has no neutral meaning is carried, to be forwarded or reported.
        assert_eq!(req.extra.keys().collect::<Vec<_>>(), ["text.verbosity", "metadata"]);
    }

    #[test]
    fn built_in_tools_and_their_items_fail_loudly() {
        for body in [
            json!({ "input": "hi", "tools": [{ "type": "web_search" }] }),
            json!({ "input": [{ "type": "web_search_call", "id": "ws_1" }] }),
            json!({ "input": [{ "type": "computer_call_output", "call_id": "c" }] }),
            json!({ "input": "hi", "tool_choice": { "type": "file_search" } }),
            json!({ "input": [{ "role": "user", "content": [{ "type": "input_file", "file_id": "file-1" }] }] }),
        ] {
            let e = parse_request(body.to_string().as_bytes()).unwrap_err();
            assert!(matches!(e, DialectError::Unsupported(_)), "{body}: {e:?}");
        }
        assert!(matches!(
            parse_request(br#"{"input":"hi","store":true}"#).unwrap_err(),
            DialectError::InvalidRequest(_)
        ));
    }

    #[test]
    fn signatures_carry_their_family_across_the_dialect() {
        // Anthropic's travels labelled and comes back unlabelled, as the neutral model keeps it.
        let out = encrypted_out(Some("EqQB"), None).unwrap();
        assert_eq!(out, "gw1.anthropic.EqQB");
        assert_eq!(signature_in(&out), (Some("EqQB".into()), None));
        // OpenAI's own goes out unlabelled.
        assert_eq!(encrypted_out(Some(&sign(OPENAI, "gAAA")), None).as_deref(), Some("gAAA"));
        // Gemini's keeps its label; redacted thinking has one of its own.
        assert_eq!(signature_in("gw1.gemini.U0lH"), (Some("gw1.gemini.U0lH".into()), None));
        let redacted = encrypted_out(None, Some("DATA")).unwrap();
        assert_eq!(signature_in(&redacted), (None, Some("DATA".into())));
    }

    fn answer() -> Response {
        Response {
            id: "msg_01ABC".into(),
            model: "claude-x".into(),
            content: vec![
                Block::Reasoning { text: "Hmm.".into(), signature: Some("SIG".into()), redacted: None },
                Block::text("Calling."),
                Block::ToolCall {
                    id: "toolu_1".into(),
                    name: "weather".into(),
                    arguments: "{\"c\":1}".into(),
                    cache: None,
                },
            ],
            finish: Finish::ToolCalls,
            native_finish: "tool_use".into(),
            usage: Some(Usage {
                input_tokens: 10,
                cache_read_tokens: 5,
                output_tokens: 7,
                reasoning_tokens: 2,
                ..Default::default()
            }),
            extra: Map::new(),
        }
    }

    #[test]
    fn an_answer_is_written_as_a_response_object() {
        let mut s = stamp();
        s.cost = Some(json!({ "amount": 0.01, "currency": "USD" }));
        s.echo = Some(json!({ "instructions": "Be brief.", "tools": [{ "type": "function", "name": "weather" }] }));
        let v = write_response(&answer(), &s);
        assert_eq!(v["id"], "resp_msg_01ABC");
        assert_eq!(
            (v["object"].as_str(), v["status"].as_str(), v["store"].as_bool()),
            (Some("response"), Some("completed"), Some(false))
        );
        assert_eq!(v["instructions"], "Be brief.");
        assert_eq!(v["tools"][0]["name"], "weather");
        let output = v["output"].as_array().unwrap();
        assert_eq!(output.len(), 3);
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["encrypted_content"], "gw1.anthropic.SIG");
        assert_eq!(output[1]["content"][0]["text"], "Calling.");
        assert_eq!(
            (output[2]["call_id"].as_str(), output[2]["id"].as_str()),
            (Some("toolu_1"), Some("fc_msg_01ABC_2"))
        );
        assert_eq!(v["usage"]["input_tokens"], 15);
        assert_eq!(v["usage"]["input_tokens_details"]["cached_tokens"], 5);
        assert_eq!(v["usage"]["cost"]["amount"], 0.01);
        // The same answer, the same bytes.
        assert_eq!(v.to_string(), write_response(&answer(), &s).to_string());
        // Cut short: incomplete, with the reason.
        let mut short = answer();
        short.finish = Finish::Length;
        let v = write_response(&short, &s);
        assert_eq!(
            (v["status"].as_str(), v["incomplete_details"]["reason"].as_str()),
            (Some("incomplete"), Some("max_output_tokens"))
        );
    }

    #[test]
    fn a_written_answer_reads_back_the_same() {
        let v = write_response(&answer(), &stamp());
        let back = read_response(v.to_string().as_bytes(), &Profile::default()).unwrap();
        assert_eq!(back.content, answer().content);
        assert_eq!(back.finish, Finish::ToolCalls);
        assert_eq!(back.usage, answer().usage);
    }

    fn stream_of(response: &Response, s: Stamp) -> String {
        let mut w = client_stream(s);
        let mut out = BytesMut::new();
        for e in crate::assemble::events_of(response) {
            if matches!(e, Event::MessageStop { .. }) {
                w.finalize(Some(json!({ "amount": 0.01 })), None);
            }
            w.write(&e, &mut out);
        }
        w.end(&mut out);
        String::from_utf8(out.to_vec()).unwrap()
    }

    #[test]
    fn a_stream_has_the_events_the_sdks_read_and_ends_with_the_whole_response() {
        let text = stream_of(&answer(), stamp());
        let mut framer = crate::sse::SseFramer::new();
        let mut events = Vec::new();
        framer.feed(text.as_bytes(), |e| events.push(e));
        let kinds: Vec<&str> = events.iter().map(|e| e.event.as_str()).collect();
        assert_eq!(kinds[..2], ["response.created", "response.in_progress"]);
        for expected in [
            "response.output_item.added",
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_text.delta",
            "response.reasoning_summary_text.done",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.done",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.output_item.done",
        ] {
            assert!(kinds.contains(&expected), "{expected} is missing: {kinds:?}");
        }
        assert_eq!(kinds.last(), Some(&"response.completed"));
        let numbers: Vec<u64> = events
            .iter()
            .map(|e| serde_json::from_slice::<Value>(&e.data).unwrap()["sequence_number"].as_u64().unwrap())
            .collect();
        assert_eq!(numbers, (0..events.len() as u64).collect::<Vec<_>>());
        let last: Value = serde_json::from_slice(&events.last().unwrap().data).unwrap();
        assert_eq!(last["response"]["usage"]["cost"]["amount"], 0.01);
        assert_eq!(last["response"]["output"], write_response(&answer(), &stamp())["output"]);
    }

    #[test]
    fn a_providers_stream_reads_into_neutral_events() {
        // What the client half writes is what OpenAI's stream looks like, ids apart.
        let text = stream_of(&answer(), stamp());
        let mut framer = crate::sse::SseFramer::new();
        let mut reader = provider_stream(&Profile::default());
        let mut events = Vec::new();
        framer.feed(text.as_bytes(), |e| reader.read(&e.event, &e.data, &mut events));
        reader.finish(&mut events);
        let mut whole = Assembler::new();
        for e in &events {
            whole.push(e);
        }
        let back = whole.finish().expect("a complete stream");
        assert_eq!(back.content, answer().content);
        assert_eq!((back.finish, back.usage), (Finish::ToolCalls, answer().usage));
    }

    #[test]
    fn input_is_read_as_items_and_a_kept_answer_is_stamped() {
        assert_eq!(
            input_items(&json!("hi")),
            Some(vec![json!({ "type": "message", "role": "user", "content": "hi" })])
        );
        let items = json!([{ "role": "user", "content": "a" }, { "type": "function_call_output", "call_id": "c" }]);
        assert_eq!(input_items(&items).map(|i| i.len()), Some(2));
        assert_eq!(input_items(&json!(3)), None);

        let mut response = json!({ "id": "resp_provider", "object": "response", "store": false, "output": [] });
        super::stamp(&mut response, "resp_gw", true, Some("resp_before"));
        assert_eq!(
            response,
            json!({ "id": "resp_gw", "object": "response", "store": true, "output": [], "previous_response_id": "resp_before" })
        );
        // The members keep their places: the id stays first.
        assert_eq!(response.as_object().unwrap().keys().next().map(String::as_str), Some("id"));
        super::stamp(&mut response, "resp_gw", false, None);
        assert_eq!((response["store"].as_bool(), response["previous_response_id"].is_null()), (Some(false), true));
    }

    #[test]
    fn an_input_item_is_listed_with_an_id_a_type_and_parts() {
        let easy = listed_item(json!({ "role": "user", "content": "hi" }), "abc", 0);
        assert_eq!(
            easy,
            json!({ "role": "user", "content": [{ "type": "input_text", "text": "hi" }], "type": "message",
                    "id": "msg_abc_in0", "status": "completed" })
        );
        let output = listed_item(json!({ "type": "function_call_output", "call_id": "c", "output": "x" }), "abc", 2);
        assert_eq!(output["id"], "fco_abc_in2");
        // An id the caller sent is kept.
        assert_eq!(
            listed_item(json!({ "type": "message", "id": "msg_1", "role": "user", "content": [] }), "abc", 1)["id"],
            "msg_1"
        );
    }

    #[test]
    fn echo_takes_what_a_response_repeats() {
        let echo = echo_of(br#"{"model":"m","input":"hi","instructions":"x","temperature":0.2,"tools":[]}"#).unwrap();
        assert_eq!(echo, json!({ "instructions": "x", "temperature": 0.2, "tools": [] }));
    }
}
