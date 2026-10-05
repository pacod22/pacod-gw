//! The gateway's own dialect (concept section 9.2): provider-neutral, with content as typed
//! blocks, one vocabulary for reasoning and caching, and everything that concerns the gateway
//! in a `gateway` object.
//!
//! Only a client half exists: no provider speaks this dialect. It maps onto the neutral
//! model almost one to one, and because it is the gateway's own it is strict: a member it
//! does not know is an error, not something to carry along.

use bytes::BytesMut;
use pacod_gw_events::Usage;
use serde_json::{Map, Value, json};

use crate::ClientStream;
use crate::canonical::{
    Block, BlockStart, Cache, Delta, Event, ImageSource, Message, Request, Response, ResponseFormat, Role, Stamp, Tool,
    ToolChoice,
};
use crate::error::DialectError;
use crate::sse::write_event;
use crate::util::{arguments_text, object, str_of, strings, take_bool, take_f64, take_string, take_u64};

/// `cache: {"ttl": "5m"}` on a block.
fn cache_of(b: &Value) -> Option<Cache> {
    let c = b.get("cache")?.as_object()?;
    Some(Cache { ttl: c.get("ttl").and_then(Value::as_str).map(str::to_owned) })
}

fn cache_json(c: &Cache) -> Value {
    match &c.ttl {
        Some(ttl) => json!({ "ttl": ttl }),
        None => json!({}),
    }
}

fn read_block(b: &Value) -> Result<Block, DialectError> {
    let cache = cache_of(b);
    Ok(match str_of(b, "type") {
        Some("text") => Block::Text {
            text: str_of(b, "text").unwrap_or_default().to_owned(),
            citations: b.get("citations").filter(|c| !c.is_null()).cloned(),
            cache,
        },
        Some("image") => {
            let source = b.get("source").cloned().unwrap_or(Value::Null);
            let source = match str_of(&source, "type") {
                Some("base64") => ImageSource::Base64 {
                    media_type: str_of(&source, "media_type").unwrap_or_default().to_owned(),
                    data: str_of(&source, "data").unwrap_or_default().to_owned(),
                },
                Some("url") => ImageSource::Url { url: str_of(&source, "url").unwrap_or_default().to_owned() },
                _ => return Err(DialectError::invalid("an image block needs a source of type base64 or url")),
            };
            Block::Image { source, cache }
        }
        Some("document") => {
            let mut body = b.as_object().cloned().unwrap_or_default();
            body.shift_remove("type");
            body.shift_remove("cache");
            Block::Document { body, cache }
        }
        Some("tool_call") => Block::ToolCall {
            id: str_of(b, "id").unwrap_or_default().to_owned(),
            name: str_of(b, "name").unwrap_or_default().to_owned(),
            arguments: arguments_text(b.get("arguments")),
            cache,
        },
        Some("tool_result") => Block::ToolResult {
            tool_call_id: str_of(b, "tool_call_id").unwrap_or_default().to_owned(),
            content: read_content(b.get("content"))?,
            is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            cache,
        },
        Some("reasoning") => Block::Reasoning {
            text: str_of(b, "text").unwrap_or_default().to_owned(),
            signature: str_of(b, "signature").filter(|s| !s.is_empty()).map(str::to_owned),
            redacted: str_of(b, "redacted").map(str::to_owned),
        },
        // Blocks only the serving provider understands, given back as they were received.
        Some("server_tool_call" | "server_tool_result") => Block::Provider {
            family: str_of(b, "provider").unwrap_or_default().to_owned(),
            raw: b.get("payload").cloned().unwrap_or(Value::Null),
        },
        Some("provider") => Block::Provider {
            family: str_of(b, "provider").unwrap_or_default().to_owned(),
            raw: b.get("block").cloned().unwrap_or(Value::Null),
        },
        Some(other) => return Err(DialectError::invalid(format!("\"{other}\" is not a content block type"))),
        None => return Err(DialectError::invalid("every content block needs a type")),
    })
}

fn read_content(content: Option<&Value>) -> Result<Vec<Block>, DialectError> {
    match content {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) if s.is_empty() => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![Block::text(s.clone())]),
        Some(Value::Array(blocks)) => blocks.iter().map(read_block).collect(),
        Some(_) => Err(DialectError::invalid("content must be a string or a list of blocks")),
    }
}

/// Reads a native request.
pub fn parse_request(body: &[u8]) -> Result<Request, DialectError> {
    let mut top = object(body)?;
    // The native stream always carries usage on its last event.
    let mut req =
        Request { model: take_string(&mut top, "model").unwrap_or_default(), stream_usage: true, ..Default::default() };

    req.system = read_content(top.shift_remove("system").as_ref())?;
    match top.shift_remove("messages") {
        Some(Value::Array(messages)) => {
            for m in &messages {
                let role = match str_of(m, "role") {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    Some("tool") => Role::Tool,
                    Some(other) => return Err(DialectError::invalid(format!("\"{other}\" is not a message role"))),
                    None => return Err(DialectError::invalid("every message needs a role")),
                };
                req.messages.push(Message { role, content: read_content(m.get("content"))? });
            }
        }
        _ => return Err(DialectError::invalid("messages is required and must be a list")),
    }

    for tool in top.shift_remove("tools").and_then(|v| v.as_array().cloned()).unwrap_or_default() {
        match (str_of(&tool, "provider"), tool.get("definition")) {
            (Some(provider), Some(definition)) => {
                req.tools.push(Tool::Provider { family: provider.to_owned(), definition: definition.clone() });
            }
            _ => {
                let name = str_of(&tool, "name").filter(|n| !n.is_empty());
                let Some(name) = name else { return Err(DialectError::invalid("a function tool needs a name")) };
                req.tools.push(Tool::Function {
                    name: name.to_owned(),
                    description: str_of(&tool, "description").map(str::to_owned),
                    parameters: tool
                        .get("parameters")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                    strict: tool.get("strict").and_then(Value::as_bool),
                    cache: cache_of(&tool),
                });
            }
        }
    }
    req.tool_choice = match top.shift_remove("tool_choice") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(match s.as_str() {
            "auto" => ToolChoice::Auto,
            "none" => ToolChoice::None,
            "required" => ToolChoice::Required,
            other => return Err(DialectError::invalid(format!("\"{other}\" is not a tool_choice"))),
        }),
        Some(o) => match str_of(&o, "tool") {
            Some(name) => Some(ToolChoice::Tool(name.to_owned())),
            None => return Err(DialectError::invalid("tool_choice must be auto, none, required or {\"tool\": name}")),
        },
    };
    req.response_format = match top.shift_remove("response_format") {
        None | Some(Value::Null) => None,
        Some(f) => Some(match str_of(&f, "type") {
            Some("text") => ResponseFormat::Text,
            Some("json_schema") => ResponseFormat::JsonSchema {
                name: str_of(&f, "name").map(str::to_owned),
                schema: f
                    .get("schema")
                    .cloned()
                    .ok_or_else(|| DialectError::invalid("response_format of type json_schema needs a schema"))?,
                strict: f.get("strict").and_then(Value::as_bool),
            },
            _ => return Err(DialectError::invalid("response_format.type must be text or json_schema")),
        }),
    };

    req.stop = strings(top.shift_remove("stop"));
    req.stream = take_bool(&mut top, "stream").unwrap_or(false);
    let p = &mut req.params;
    p.max_output_tokens = take_u64(&mut top, "max_output_tokens");
    p.temperature = take_f64(&mut top, "temperature");
    p.top_p = take_f64(&mut top, "top_p");
    p.top_k = take_f64(&mut top, "top_k");
    p.frequency_penalty = take_f64(&mut top, "frequency_penalty");
    p.presence_penalty = take_f64(&mut top, "presence_penalty");
    if let Some(r) = top.shift_remove("reasoning").filter(|r| !r.is_null()) {
        p.reasoning.effort = str_of(&r, "effort").map(str::to_owned);
        p.reasoning.budget_tokens = r.get("budget_tokens").and_then(Value::as_u64);
        p.reasoning.summary = r.get("summary").and_then(Value::as_bool);
    }
    if let Some(Value::Object(options)) = top.shift_remove("provider_options") {
        req.provider_options = options;
    }
    req.gateway = top.shift_remove("gateway");

    // This dialect is the gateway's own: what it does not know is a mistake in the request.
    if let Some(unknown) = top.keys().next() {
        return Err(DialectError::invalid(format!("\"{unknown}\" is not a field of this interface")));
    }
    Ok(req)
}

// --- writing -------------------------------------------------------------------------------------

fn write_block(b: &Block) -> Value {
    let with_cache = |mut v: Value, cache: &Option<Cache>| {
        if let Some(c) = cache {
            v["cache"] = cache_json(c);
        }
        v
    };
    match b {
        Block::Text { text, citations, cache } => {
            let mut v = json!({ "type": "text", "text": text });
            if let Some(c) = citations {
                v["citations"] = c.clone();
            }
            with_cache(v, cache)
        }
        Block::Image { source, cache } => {
            let source = match source {
                ImageSource::Base64 { media_type, data } => {
                    json!({ "type": "base64", "media_type": media_type, "data": data })
                }
                ImageSource::Url { url } => json!({ "type": "url", "url": url }),
            };
            with_cache(json!({ "type": "image", "source": source }), cache)
        }
        Block::Document { body, cache } => {
            let mut v = Map::new();
            v.insert("type".into(), json!("document"));
            for (k, value) in body {
                v.insert(k.clone(), value.clone());
            }
            with_cache(Value::Object(v), cache)
        }
        // Arguments are an object here; text that is not JSON is passed as the text it is.
        Block::ToolCall { id, name, arguments, cache } => {
            let arguments = crate::util::arguments_value(arguments).unwrap_or_else(|_| json!(arguments));
            with_cache(json!({ "type": "tool_call", "id": id, "name": name, "arguments": arguments }), cache)
        }
        Block::ToolResult { tool_call_id, content, is_error, cache } => {
            let content: Vec<Value> = content.iter().map(write_block).collect();
            with_cache(
                json!({ "type": "tool_result", "tool_call_id": tool_call_id, "content": content, "is_error": is_error }),
                cache,
            )
        }
        Block::Reasoning { text, signature, redacted } => {
            let mut v = json!({ "type": "reasoning", "text": text });
            if let Some(s) = signature {
                v["signature"] = json!(s);
            }
            if let Some(r) = redacted {
                v["redacted"] = json!(r);
            }
            v
        }
        Block::Provider { family, raw } => json!({ "type": "provider", "provider": family, "block": raw }),
    }
}

fn usage_json(u: &Usage, cost: Option<&Value>) -> Value {
    let mut v = json!({
        "input_tokens": u.input_tokens,
        "cache_read_tokens": u.cache_read_tokens,
        "cache_write_tokens": u.cache_write_tokens,
        "output_tokens": u.output_tokens,
        "reasoning_tokens": u.reasoning_tokens,
        "source": "provider",
    });
    if let Some(c) = cost {
        v["cost"] = c.clone();
    }
    v
}

/// Writes a complete native response.
pub fn write_response(response: &Response, stamp: &Stamp) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(stamp.id));
    out.insert("model".into(), json!(stamp.model));
    out.insert("content".into(), Value::Array(response.content.iter().map(write_block).collect()));
    out.insert("finish_reason".into(), json!(response.finish.as_str()));
    out.insert("native_finish_reason".into(), json!(response.native_finish));
    if let Some(u) = &response.usage {
        out.insert("usage".into(), usage_json(u, stamp.cost.as_ref()));
    }
    if let Some(g) = &stamp.gateway {
        out.insert("gateway".into(), g.clone());
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
    fn finalize(&mut self, cost: Option<Value>, gateway: Option<Value>) {
        self.stamp.cost = cost;
        self.stamp.gateway = gateway;
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
                Self::event(out, "message.start", json!({ "id": self.stamp.id, "model": self.stamp.model }));
            }
            Event::ContentStart { index, block } => {
                let data = match block {
                    BlockStart::Text => json!({ "index": index, "type": "text" }),
                    BlockStart::Reasoning => json!({ "index": index, "type": "reasoning" }),
                    BlockStart::ToolCall { id, name } => {
                        json!({ "index": index, "type": "tool_call", "id": id, "name": name })
                    }
                    BlockStart::Provider { family, raw } => {
                        json!({ "index": index, "type": "provider", "provider": family, "block": raw })
                    }
                };
                Self::event(out, "content.start", data);
            }
            Event::ContentDelta { index, delta } => {
                let delta = match delta {
                    Delta::Text(t) => json!({ "type": "text", "text": t }),
                    Delta::Reasoning(t) => json!({ "type": "reasoning", "text": t }),
                    Delta::ToolArguments(a) => json!({ "type": "tool_arguments", "arguments": a }),
                    Delta::Signature(s) => json!({ "type": "signature", "signature": s }),
                    Delta::Citation(c) => json!({ "type": "citation", "citation": c }),
                    Delta::Provider(raw) => json!({ "type": "provider", "delta": raw }),
                };
                Self::event(out, "content.delta", json!({ "index": index, "delta": delta }));
            }
            Event::ContentStop { index } => Self::event(out, "content.stop", json!({ "index": index })),
            Event::MessageStop { finish, native_finish, usage } => {
                let mut data = Map::new();
                data.insert("finish_reason".into(), json!(finish.as_str()));
                data.insert("native_finish_reason".into(), json!(native_finish));
                if let Some(u) = usage {
                    data.insert("usage".into(), usage_json(u, self.stamp.cost.as_ref()));
                }
                if let Some(g) = &self.stamp.gateway {
                    data.insert("gateway".into(), g.clone());
                }
                Self::event(out, "message.stop", Value::Object(data));
            }
            Event::Ping => Self::event(out, "ping", json!({})),
            Event::Error { kind, message } => {
                Self::event(out, "error", json!({ "error": { "type": kind, "message": message } }));
            }
        }
    }
}

/// A writer for a native response stream. Usage is always on its last event.
pub fn client_stream(stamp: Stamp, _include_usage: bool) -> Box<dyn ClientStream> {
    Box::new(Writer { stamp })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{Finish, Notes, Profile, ReasoningStyle};
    use crate::{anthropic, openai};

    /// The request of concept section 9.2.
    fn concept_request() -> Value {
        json!({
            "model": "@synthesizer",
            "system": [{ "type": "text", "text": "You are a careful analyst.", "cache": { "ttl": "5m" } }],
            "messages": [
                { "role": "user", "content": [
                    { "type": "text", "text": "Compare the two offers." },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } }
                ] }
            ],
            "max_output_tokens": 4000,
            "reasoning": { "effort": "high", "summary": true },
            "stream": false,
            "gateway": { "end_user": "u_18342", "session_id": "s_77", "trace_id": "turn_9f3",
                         "tags": { "purpose": "synthesizer", "site": "pacod" }, "timeout_ms": 180000 }
        })
    }

    #[test]
    fn the_concepts_request_is_read() {
        let req = parse_request(concept_request().to_string().as_bytes()).unwrap();
        assert_eq!(req.model, "@synthesizer");
        assert_eq!(
            req.system,
            [Block::Text {
                text: "You are a careful analyst.".into(),
                citations: None,
                cache: Some(Cache { ttl: Some("5m".into()) })
            }]
        );
        assert_eq!(req.messages[0].content.len(), 2);
        assert_eq!(req.params.max_output_tokens, Some(4000));
        assert_eq!((req.params.reasoning.effort.as_deref(), req.params.reasoning.summary), (Some("high"), Some(true)));
        assert_eq!(req.gateway.as_ref().unwrap()["end_user"], "u_18342");
        assert!(!req.stream && req.stream_usage);
        assert!(req.extra.is_empty());

        // The same request for an Anthropic model and for an OpenAI-compatible one.
        let claude = Profile {
            family: "anthropic".into(),
            upstream_model: "claude-sonnet-5-5".into(),
            reasoning: ReasoningStyle::Adaptive,
            cache_markers: true,
            ..Default::default()
        };
        let v = anthropic::build_request(&req, &claude, &mut Notes::default()).unwrap();
        assert_eq!(
            v,
            json!({
                "model": "claude-sonnet-5-5", "max_tokens": 4000,
                "system": [{ "type": "text", "text": "You are a careful analyst.", "cache_control": { "type": "ephemeral", "ttl": "5m" } }],
                "messages": [{ "role": "user", "content": [
                    { "type": "text", "text": "Compare the two offers." },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } }
                ] }],
                "thinking": { "type": "adaptive", "display": "summarized" },
                "output_config": { "effort": "high" }
            })
        );
        let gpt = Profile { upstream_model: "gpt-x".into(), max_completion_tokens: true, ..Default::default() };
        let mut notes = Notes::default();
        let v = openai::build_request(&req, &gpt, &mut notes).unwrap();
        assert_eq!(
            v,
            json!({
                "model": "gpt-x",
                "messages": [
                    { "role": "system", "content": "You are a careful analyst." },
                    { "role": "user", "content": [
                        { "type": "text", "text": "Compare the two offers." },
                        { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }
                    ] }
                ],
                "max_completion_tokens": 4000,
                "reasoning_effort": "high"
            })
        );
        assert_eq!(notes.dropped, ["cache_control"], "the marker has no meaning there, and that is said");
    }

    #[test]
    fn every_block_type_is_read_and_written_back() {
        let blocks = json!([
            { "type": "text", "text": "t", "citations": [{ "url": "u" }], "cache": {} },
            { "type": "image", "source": { "type": "url", "url": "https://x.test/i.png" } },
            { "type": "document", "source": { "type": "text", "data": "d" }, "title": "T" },
            { "type": "tool_call", "id": "c1", "name": "f", "arguments": { "a": 1 } },
            { "type": "tool_result", "tool_call_id": "c1", "content": [{ "type": "text", "text": "r" }], "is_error": true },
            { "type": "reasoning", "text": "hm", "signature": "sig==" },
            { "type": "reasoning", "text": "", "redacted": "enc" },
            { "type": "provider", "provider": "anthropic", "block": { "type": "server_tool_use", "id": "s" } }
        ]);
        let req = parse_request(
            json!({ "model": "m", "messages": [{ "role": "assistant", "content": blocks.clone() }] })
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        let content = &req.messages[0].content;
        assert_eq!(content.len(), 8);
        assert_eq!(
            content[3],
            Block::ToolCall { id: "c1".into(), name: "f".into(), arguments: "{\"a\":1}".into(), cache: None }
        );
        let back: Vec<Value> = content.iter().map(write_block).collect();
        assert_eq!(Value::Array(back), blocks);

        // Server-tool blocks are accepted in their own spelling and come back as provider blocks.
        let server = parse_request(
            json!({ "model": "m", "messages": [{ "role": "assistant", "content": [
                { "type": "server_tool_call", "provider": "anthropic", "payload": { "type": "server_tool_use" } },
                { "type": "server_tool_result", "payload": { "type": "web_search_tool_result" } }
            ] }] })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert!(
            matches!(&server.messages[0].content[1], Block::Provider { family, raw } if family.is_empty() && raw["type"] == "web_search_tool_result")
        );
    }

    #[test]
    fn tools_choices_and_formats() {
        let req = parse_request(
            json!({
                "model": "m", "messages": [{ "role": "tool", "content": [{ "type": "tool_result", "tool_call_id": "c", "content": "ok" }] }],
                "tools": [
                    { "name": "look", "description": "d", "parameters": { "type": "object" }, "strict": true },
                    { "provider": "anthropic", "definition": { "type": "web_search_20260209", "name": "web_search" } }
                ],
                "tool_choice": { "tool": "look" },
                "response_format": { "type": "json_schema", "name": "out", "schema": { "type": "object" }, "strict": true },
                "stop": ["END"], "stream": true, "temperature": 0.1, "top_p": 0.9, "top_k": 5,
                "frequency_penalty": 0.2, "presence_penalty": 0.3,
                "reasoning": { "budget_tokens": 2048 },
                "provider_options": { "anthropic": { "service_tier": "auto" } }
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(req.messages[0].role, Role::Tool);
        assert!(matches!(&req.tools[1], Tool::Provider { family, .. } if family == "anthropic"));
        assert_eq!(req.tool_choice, Some(ToolChoice::Tool("look".into())));
        assert!(
            matches!(&req.response_format, Some(ResponseFormat::JsonSchema { name: Some(n), strict: Some(true), .. }) if n == "out")
        );
        assert_eq!(
            (req.params.top_k, req.params.presence_penalty, req.params.reasoning.budget_tokens),
            (Some(5.0), Some(0.3), Some(2048))
        );
        assert_eq!(req.provider_options["anthropic"]["service_tier"], "auto");
        assert!(req.stream);
    }

    #[test]
    fn the_gateways_own_dialect_is_strict() {
        for (body, why) in [
            (json!({ "model": "m", "messages": [], "max_tokens": 5 }), "\"max_tokens\" is not a field"),
            (json!({ "model": "m", "messages": [], "stream_options": {} }), "\"stream_options\" is not a field"),
            (json!({ "model": "m" }), "messages is required"),
            (json!({ "model": "m", "messages": [{ "role": "system", "content": "x" }] }), "not a message role"),
            (
                json!({ "model": "m", "messages": [{ "role": "user", "content": [{ "type": "tool_use" }] }] }),
                "not a content block type",
            ),
            (json!({ "model": "m", "messages": [{ "role": "user", "content": [{ "text": "x" }] }] }), "needs a type"),
            (
                json!({ "model": "m", "messages": [{ "role": "user", "content": [{ "type": "image", "source": { "type": "file" } }] }] }),
                "base64 or url",
            ),
            (json!({ "model": "m", "messages": [], "tools": [{ "description": "nameless" }] }), "needs a name"),
            (json!({ "model": "m", "messages": [], "tool_choice": "maybe" }), "not a tool_choice"),
            (json!({ "model": "m", "messages": [], "tool_choice": {} }), "tool_choice must be"),
            (
                json!({ "model": "m", "messages": [], "response_format": { "type": "json_object" } }),
                "text or json_schema",
            ),
            (json!({ "model": "m", "messages": [], "response_format": { "type": "json_schema" } }), "needs a schema"),
        ] {
            match parse_request(body.to_string().as_bytes()) {
                Err(DialectError::InvalidRequest(m)) => assert!(m.contains(why), "{body}: {m}"),
                other => panic!("{body}: expected an invalid request, got {other:?}"),
            }
        }
        assert!(parse_request(b"[1,2]").is_err() && parse_request(b"").is_err());
    }

    fn answer() -> Response {
        Response {
            id: "msg_1".into(),
            model: "claude-sonnet-5-5-20260101".into(),
            content: vec![
                Block::Reasoning {
                    text: "Summary of the reasoning.".into(),
                    signature: Some("sig==".into()),
                    redacted: None,
                },
                Block::text("Offer B is cheaper over three years because ..."),
            ],
            finish: Finish::Stop,
            native_finish: "end_turn".into(),
            usage: Some(Usage { input_tokens: 812, cache_read_tokens: 6100, output_tokens: 420, ..Default::default() }),
            extra: Map::new(),
        }
    }

    #[test]
    fn the_concepts_response_is_written() {
        let stamp = Stamp {
            id: "gw_01JC8Z4T".into(),
            model: "anthropic/claude-sonnet-5-5".into(),
            created: 0,
            cost: Some(json!({ "amount": 0.007044, "currency": "USD", "price_version": 37 })),
            gateway: Some(
                json!({ "request_id": "gw_01JC8Z4T", "workspace": "pacod-prod", "dropped_params": ["temperature"] }),
            ),
        };
        assert_eq!(
            write_response(&answer(), &stamp),
            json!({
                "id": "gw_01JC8Z4T",
                "model": "anthropic/claude-sonnet-5-5",
                "content": [
                    { "type": "reasoning", "text": "Summary of the reasoning.", "signature": "sig==" },
                    { "type": "text", "text": "Offer B is cheaper over three years because ..." }
                ],
                "finish_reason": "stop",
                "native_finish_reason": "end_turn",
                "usage": {
                    "input_tokens": 812, "cache_read_tokens": 6100, "cache_write_tokens": 0, "output_tokens": 420, "reasoning_tokens": 0,
                    "source": "provider",
                    "cost": { "amount": 0.007044, "currency": "USD", "price_version": 37 }
                },
                "gateway": { "request_id": "gw_01JC8Z4T", "workspace": "pacod-prod", "dropped_params": ["temperature"] }
            })
        );
        let bare =
            write_response(&Response { usage: None, ..answer() }, &Stamp { id: "x".into(), ..Default::default() });
        assert!(bare.get("usage").is_none() && bare.get("gateway").is_none());
    }

    #[test]
    fn a_stream_ends_with_exactly_one_stop_that_carries_usage_and_the_gateway_block() {
        let events = vec![
            Event::MessageStart { id: "msg_1".into(), model: "claude".into() },
            Event::Ping,
            Event::ContentStart { index: 0, block: BlockStart::Reasoning },
            Event::ContentDelta { index: 0, delta: Delta::Reasoning("hm".into()) },
            Event::ContentDelta { index: 0, delta: Delta::Signature("sig==".into()) },
            Event::ContentStop { index: 0 },
            Event::ContentStart { index: 1, block: BlockStart::ToolCall { id: "c1".into(), name: "f".into() } },
            Event::ContentDelta { index: 1, delta: Delta::ToolArguments("{}".into()) },
            Event::ContentStop { index: 1 },
            Event::MessageStop { finish: Finish::ToolCalls, native_finish: "tool_use".into(), usage: answer().usage },
        ];
        let mut w =
            client_stream(Stamp { id: "gw_1".into(), model: "anthropic/claude".into(), ..Default::default() }, true);
        let mut out = BytesMut::new();
        for e in &events {
            if matches!(e, Event::MessageStop { .. }) {
                w.finalize(Some(json!({ "amount": 0.007044 })), Some(json!({ "request_id": "gw_1" })));
            }
            w.write(e, &mut out);
        }
        let text = String::from_utf8(out.to_vec()).unwrap();
        let names: Vec<&str> = text.lines().filter_map(|l| l.strip_prefix("event: ")).collect();
        assert_eq!(
            names,
            [
                "message.start",
                "ping",
                "content.start",
                "content.delta",
                "content.delta",
                "content.stop",
                "content.start",
                "content.delta",
                "content.stop",
                "message.stop"
            ]
        );
        assert!(text.contains(r#"data: {"id":"gw_1","model":"anthropic/claude"}"#));
        assert!(text.contains(r#"data: {"index":1,"type":"tool_call","id":"c1","name":"f"}"#));
        assert!(text.contains(r#"data: {"index":0,"delta":{"type":"signature","signature":"sig=="}}"#));
        let last = text.trim_end().lines().last().unwrap().strip_prefix("data: ").unwrap();
        let stop: Value = serde_json::from_str(last).unwrap();
        assert_eq!(
            (stop["finish_reason"].as_str(), stop["native_finish_reason"].as_str()),
            (Some("tool_calls"), Some("tool_use"))
        );
        assert_eq!(
            (stop["usage"]["cost"]["amount"].as_f64(), stop["gateway"]["request_id"].as_str()),
            (Some(0.007044), Some("gw_1"))
        );

        let mut out = BytesMut::new();
        client_stream(Stamp::default(), true)
            .write(&Event::Error { kind: "upstream_error".into(), message: "broke".into() }, &mut out);
        assert_eq!(
            &out[..],
            b"event: error\ndata: {\"error\":{\"type\":\"upstream_error\",\"message\":\"broke\"}}\n\n"
        );
    }
}
