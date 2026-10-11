//! Google's Gemini API, `generateContent` (concept section 8.2): the provider half only.
//!
//! Managed mode speaks it to a Gemini provider whose `managed_api` is `native`. Every such call
//! is translated: callers use the OpenAI-compatible, Anthropic or native interface, and Gemini's
//! own SDKs go through proxy mode. Thought signatures travel as signed reasoning blocks
//! (`gw1.gemini.<signature>`) right before the block whose part carried them; written back,
//! a signature goes on the next part, as Gemini attached it.

use std::collections::HashMap;

use pacod_gw_events::Usage;
use serde_json::{Map, Value, json};

use crate::ProviderStream;
use crate::canonical::{
    Block, BlockStart, Delta, Event, Finish, ImageSource, Notes, Profile, ReasoningStyle, Request, Response,
    ResponseFormat, Role, SKIP_THOUGHT_SIGNATURE, Tool, ToolChoice, sign, signed_by,
};
use crate::error::DialectError;
use crate::util::{arguments_value, str_of};

/// The family name of this dialect, in provider blocks and signatures.
pub const FAMILY: &str = "gemini";

/// The prefix of a tool-call id the gateway made up for a call Gemini gave none. Such an id
/// never goes back to Gemini.
const MADE_UP_ID: &str = "gm_";

fn note(notes: &mut Notes, name: &str) {
    if !notes.dropped.iter().any(|d| d == name) {
        notes.dropped.push(name.to_owned());
    }
}

// --- writing the request -----------------------------------------------------------------------

/// One part of a content, as Gemini takes it.
fn image_part(source: &ImageSource) -> Result<Value, DialectError> {
    match source {
        ImageSource::Base64 { media_type, data } => {
            Ok(json!({ "inlineData": { "mimeType": media_type, "data": data } }))
        }
        ImageSource::Url { .. } => {
            Err(DialectError::unsupported("Gemini's API does not fetch image URLs; send the image inline (base64)"))
        }
    }
}

fn document_part(body: &Map<String, Value>) -> Result<Value, DialectError> {
    let source = body.get("source").cloned().unwrap_or(Value::Null);
    match str_of(&source, "type") {
        Some("base64") => Ok(json!({ "inlineData": {
            "mimeType": str_of(&source, "media_type").unwrap_or("application/pdf"),
            "data": str_of(&source, "data").unwrap_or_default(),
        } })),
        Some("text") => Ok(json!({ "text": str_of(&source, "data").unwrap_or_default() })),
        Some("url") => Ok(json!({ "fileData": {
            "mimeType": str_of(&source, "media_type").unwrap_or("application/pdf"),
            "fileUri": str_of(&source, "url").unwrap_or_default(),
        } })),
        _ => Err(DialectError::unsupported("this document's source cannot be sent to Gemini")),
    }
}

/// The text of a tool result, for `functionResponse.response`.
fn result_text(content: &[Block], notes: &mut Notes) -> String {
    let mut out = String::new();
    for b in content {
        match b {
            Block::Text { text, .. } => out.push_str(text),
            _ => note(notes, "tool_result.image"),
        }
    }
    out
}

/// The parts of one assistant turn: thought text is not given back; a signature goes on the
/// part after it, or on the last part when none follows.
fn model_parts(blocks: &[Block], notes: &mut Notes) -> Vec<Value> {
    let mut parts: Vec<Value> = Vec::new();
    let mut pending: Option<String> = None;
    let push = |parts: &mut Vec<Value>, mut part: Value, pending: &mut Option<String>| {
        if let Some(sig) = pending.take() {
            part["thoughtSignature"] = json!(sig);
        }
        parts.push(part);
    };
    for b in blocks {
        match b {
            Block::Reasoning { signature: Some(sig), .. } => {
                if let Some(raw) = signed_by(sig, FAMILY) {
                    pending = Some(raw.to_owned());
                }
            }
            Block::Reasoning { .. } => {}
            Block::Text { text, .. } if !text.is_empty() => push(&mut parts, json!({ "text": text }), &mut pending),
            Block::Text { .. } => {}
            Block::ToolCall { id, name, arguments, .. } => {
                let mut call =
                    json!({ "name": name, "args": arguments_value(arguments).unwrap_or_else(|_| json!({})) });
                if !id.is_empty() && !id.starts_with(MADE_UP_ID) {
                    call["id"] = json!(id);
                }
                push(&mut parts, json!({ "functionCall": call }), &mut pending);
            }
            Block::Provider { family, raw } if family == FAMILY => push(&mut parts, raw.clone(), &mut pending),
            Block::Provider { .. } => note(notes, "provider_blocks"),
            Block::Image { source: ImageSource::Base64 { media_type, data }, .. } => {
                push(&mut parts, json!({ "inlineData": { "mimeType": media_type, "data": data } }), &mut pending)
            }
            Block::Image { .. } | Block::Document { .. } | Block::ToolResult { .. } => {}
        }
    }
    // A signature after the last part (Gemini puts one on a final, empty text part).
    if let (Some(sig), Some(last)) = (pending, parts.last_mut()) {
        last["thoughtSignature"] = json!(sig);
    }
    parts
}

/// Writes a `generateContent` request.
pub fn build_request(request: &Request, profile: &Profile, notes: &mut Notes) -> Result<Value, DialectError> {
    let mut body = Map::new();

    // The function names of earlier calls, by id: a result names its function.
    let mut names: HashMap<&str, &str> = HashMap::new();
    for m in &request.messages {
        for b in &m.content {
            if let Block::ToolCall { id, name, .. } = b {
                names.insert(id.as_str(), name.as_str());
            }
        }
    }
    // Gemini 3 checks the signatures of the current turn: after the last user message that says
    // something other than tool results.
    let turn_start = request
        .messages
        .iter()
        .rposition(|m| m.role == Role::User && m.content.iter().any(|b| !matches!(b, Block::ToolResult { .. })))
        .map_or(0, |i| i + 1);

    let mut contents: Vec<(String, Vec<Value>)> = Vec::new();
    let mut skipped = false;
    for (position, m) in request.messages.iter().enumerate() {
        let (role, parts) = match m.role {
            Role::Assistant => {
                let mut parts = model_parts(&m.content, notes);
                // A call of the current turn whose signature the caller lost: Google's placeholder.
                if position >= turn_start
                    && let Some(first) = parts.iter_mut().find(|p| p.get("functionCall").is_some())
                    && first.get("thoughtSignature").is_none()
                {
                    first["thoughtSignature"] = json!(SKIP_THOUGHT_SIGNATURE);
                    if !skipped {
                        skipped = true;
                        notes.adjusted.push(
                            "a tool call of the current turn came back without its thought signature; Gemini was sent its placeholder"
                                .into(),
                        );
                    }
                }
                ("model", parts)
            }
            Role::User | Role::Tool => {
                let mut parts = Vec::new();
                for b in &m.content {
                    match b {
                        Block::Text { text, cache, .. } => {
                            if cache.is_some() {
                                note(notes, "cache_control");
                            }
                            if !text.is_empty() {
                                parts.push(json!({ "text": text }));
                            }
                        }
                        Block::Image { source, .. } => parts.push(image_part(source)?),
                        Block::Document { body, .. } => parts.push(document_part(body)?),
                        Block::ToolResult { tool_call_id, content, is_error, .. } => {
                            let text = result_text(content, notes);
                            let name = names.get(tool_call_id.as_str()).copied().unwrap_or_default();
                            let mut response = json!({ "name": name, "response": if *is_error { json!({ "error": text }) } else { json!({ "result": text }) } });
                            if !tool_call_id.is_empty() && !tool_call_id.starts_with(MADE_UP_ID) {
                                response["id"] = json!(tool_call_id);
                            }
                            parts.push(json!({ "functionResponse": response }));
                        }
                        Block::Reasoning { .. } | Block::ToolCall { .. } => {}
                        Block::Provider { family, raw } if family == FAMILY => parts.push(raw.clone()),
                        Block::Provider { .. } => note(notes, "provider_blocks"),
                    }
                }
                ("user", parts)
            }
        };
        if parts.is_empty() {
            continue;
        }
        // Turns of one role in a row are one content (the results of parallel calls must be).
        match contents.last_mut() {
            Some((last, existing)) if *last == role => existing.extend(parts),
            _ => contents.push((role.to_owned(), parts)),
        }
    }
    body.insert(
        "contents".into(),
        Value::Array(contents.into_iter().map(|(role, parts)| json!({ "role": role, "parts": parts })).collect()),
    );

    let system: Vec<Value> = request
        .system
        .iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } if !text.is_empty() => Some(json!({ "text": text })),
            _ => None,
        })
        .collect();
    if !system.is_empty() {
        body.insert("systemInstruction".into(), json!({ "parts": system }));
    }

    // Tools: the caller's functions, and Gemini's own tools passed as they are.
    let mut declarations = Vec::new();
    let mut tools = Vec::new();
    for t in &request.tools {
        match t {
            Tool::Function { name, description, parameters, cache, .. } => {
                if cache.is_some() {
                    note(notes, "cache_control");
                }
                let mut f = json!({ "name": name, "parametersJsonSchema": parameters });
                if let Some(d) = description {
                    f["description"] = json!(d);
                }
                declarations.push(f);
            }
            Tool::Provider { family, definition } if family == FAMILY => tools.push(definition.clone()),
            Tool::Provider { .. } => {
                return Err(DialectError::unsupported("this provider tool is not available on a Gemini model"));
            }
        }
    }
    if !declarations.is_empty() {
        tools.insert(0, json!({ "functionDeclarations": declarations }));
    }
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    let calling = match &request.tool_choice {
        None => None,
        Some(ToolChoice::Auto) => Some(json!({ "mode": "AUTO" })),
        Some(ToolChoice::None) => Some(json!({ "mode": "NONE" })),
        Some(ToolChoice::Required) => Some(json!({ "mode": "ANY" })),
        Some(ToolChoice::Tool(name)) => Some(json!({ "mode": "ANY", "allowedFunctionNames": [name] })),
    };
    if let Some(c) = calling {
        body.insert("toolConfig".into(), json!({ "functionCallingConfig": c }));
    }

    // Generation settings.
    let p = &request.params;
    let mut config = Map::new();
    if let Some(v) = p.max_output_tokens {
        config.insert("maxOutputTokens".into(), json!(v));
    }
    for (name, value) in [
        ("temperature", p.temperature),
        ("topP", p.top_p),
        ("topK", p.top_k),
        ("presencePenalty", p.presence_penalty),
        ("frequencyPenalty", p.frequency_penalty),
    ] {
        if let Some(v) = value {
            config.insert(name.into(), json!(v));
        }
    }
    if !request.stop.is_empty() {
        config.insert("stopSequences".into(), json!(request.stop));
    }
    match &request.response_format {
        None | Some(ResponseFormat::Text) => {}
        Some(ResponseFormat::JsonObject) => drop(config.insert("responseMimeType".into(), json!("application/json"))),
        Some(ResponseFormat::JsonSchema { schema, .. }) => {
            config.insert("responseMimeType".into(), json!("application/json"));
            config.insert("responseJsonSchema".into(), schema.clone());
        }
    }
    if let Some(thinking) = thinking_config(request, profile, notes) {
        config.insert("thinkingConfig".into(), thinking);
    }
    // What an OpenAI-dialect caller may send that Gemini has a place for.
    for (key, value) in &request.extra {
        match key.as_str() {
            "seed" => drop(config.insert("seed".into(), value.clone())),
            _ => note(notes, key),
        }
    }
    if !config.is_empty() {
        body.insert("generationConfig".into(), Value::Object(config));
    }

    // Gemini's own options (`safetySettings`, `cachedContent`, `serviceTier`, …), last.
    if let Some(Value::Object(options)) = request.provider_options.get(FAMILY) {
        for (key, value) in options {
            if key == "generationConfig"
                && let (Some(Value::Object(have)), Value::Object(add)) = (body.get_mut(key), value)
            {
                for (k, v) in add {
                    have.insert(k.clone(), v.clone());
                }
                continue;
            }
            body.insert(key.clone(), value.clone());
        }
    }
    Ok(Value::Object(body))
}

/// `thinkingConfig`: a level for Gemini 3, a token budget for budget-style models (2.5), and
/// the thought summaries when the caller wants reasoning back.
fn thinking_config(request: &Request, profile: &Profile, notes: &mut Notes) -> Option<Value> {
    let r = &request.params.reasoning;
    let mut config = Map::new();
    match profile.reasoning {
        ReasoningStyle::None => {
            if r.effort.is_some() || r.budget_tokens.is_some() {
                note(notes, "reasoning.effort");
            }
            return None;
        }
        ReasoningStyle::Budget => {
            let budget = match (r.budget_tokens, r.effort.as_deref()) {
                (Some(b), _) => Some(b),
                (None, Some("none")) => Some(0),
                (None, Some("minimal" | "low")) => Some(1024),
                (None, Some("medium")) => Some(8192),
                (None, Some(_)) => Some(24576),
                (None, None) => None,
            };
            if let Some(b) = budget {
                config.insert("thinkingBudget".into(), json!(b));
            }
        }
        _ => {
            if let Some(effort) = r.effort.as_deref() {
                let level = if effort == "none" || !profile.reasoning_efforts.iter().any(|e| e == effort) {
                    // Gemini 3 cannot stop thinking: the lowest level the model takes.
                    let low = profile.reasoning_efforts.iter().find(|e| e.as_str() != "none").cloned();
                    if let Some(l) = &low {
                        notes.adjusted.push(format!(
                            "the reasoning effort {effort} is not available on this model; {l} was used"
                        ));
                    }
                    low
                } else {
                    Some(effort.to_owned())
                };
                if let Some(l) = level {
                    config.insert("thinkingLevel".into(), json!(l));
                }
            }
        }
    }
    let wants_text = r.summary.unwrap_or(r.effort.as_deref().is_some_and(|e| e != "none") || r.budget_tokens.is_some());
    if wants_text {
        config.insert("includeThoughts".into(), json!(true));
    }
    (!config.is_empty()).then(|| Value::Object(config))
}

// --- reading the answer ------------------------------------------------------------------------

fn finish_of(word: &str, called: bool) -> Finish {
    match word {
        "MAX_TOKENS" => Finish::Length,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "IMAGE_SAFETY" | "LANGUAGE" => {
            Finish::ContentFilter
        }
        // Gemini says STOP when it calls a function too.
        _ if called => Finish::ToolCalls,
        _ => Finish::Stop,
    }
}

/// Usage from `usageMetadata`: the prompt includes cached tokens and the tool-use prompt is
/// input; thoughts are output, reported apart from the candidates.
fn usage_of(u: &Value) -> Usage {
    let n = |key: &str| u.get(key).and_then(Value::as_u64).unwrap_or(0);
    let cached = n("cachedContentTokenCount");
    Usage {
        input_tokens: (n("promptTokenCount") + n("toolUsePromptTokenCount")).saturating_sub(cached),
        cache_read_tokens: cached,
        output_tokens: n("candidatesTokenCount") + n("thoughtsTokenCount"),
        reasoning_tokens: n("thoughtsTokenCount"),
        ..Default::default()
    }
}

/// A made-up id for a call Gemini gave none: stable for the same answer.
fn made_up_id(response_id: &str, n: usize) -> String {
    format!("{MADE_UP_ID}{response_id}_{n}")
}

/// Reads a `GenerateContentResponse`.
pub fn read_response(body: &[u8], _profile: &Profile) -> Result<Response, DialectError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| DialectError::bad_response(format!("the provider's answer is not JSON: {e}")))?;
    let response_id = str_of(&v, "responseId").unwrap_or_default().to_owned();
    let mut content = Vec::new();
    let mut calls = 0;
    let candidate = v.pointer("/candidates/0");
    for part in candidate.and_then(|c| c.pointer("/content/parts")).and_then(Value::as_array).into_iter().flatten() {
        if let Some(sig) = str_of(part, "thoughtSignature") {
            content.push(Block::Reasoning { text: String::new(), signature: Some(sign(FAMILY, sig)), redacted: None });
        }
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            let text = str_of(part, "text").unwrap_or_default();
            match content.last_mut() {
                Some(Block::Reasoning { text: t, signature: None, redacted: None }) => t.push_str(text),
                _ => content.push(Block::Reasoning { text: text.to_owned(), signature: None, redacted: None }),
            }
        } else if let Some(text) = str_of(part, "text") {
            match content.last_mut() {
                Some(Block::Text { text: t, .. }) => t.push_str(text),
                _ if text.is_empty() => {}
                _ => content.push(Block::text(text)),
            }
        } else if let Some(call) = part.get("functionCall") {
            calls += 1;
            content.push(Block::ToolCall {
                id: str_of(call, "id").map_or_else(|| made_up_id(&response_id, calls), str::to_owned),
                name: str_of(call, "name").unwrap_or_default().to_owned(),
                arguments: call.get("args").map_or_else(String::new, Value::to_string),
                cache: None,
            });
        } else {
            let mut raw = part.clone();
            if let Some(o) = raw.as_object_mut() {
                o.remove("thoughtSignature");
            }
            content.push(Block::Provider { family: FAMILY.into(), raw });
        }
    }
    let native = candidate.and_then(|c| str_of(c, "finishReason")).unwrap_or_default().to_owned();
    let blocked = v.pointer("/promptFeedback/blockReason").and_then(Value::as_str);
    let finish = match blocked {
        Some(_) if candidate.is_none() => Finish::ContentFilter,
        _ => finish_of(&native, calls > 0),
    };
    let mut extra = Map::new();
    for key in ["groundingMetadata", "citationMetadata", "urlContextMetadata", "safetyRatings"] {
        if let Some(x) = candidate.and_then(|c| c.get(key)) {
            extra.insert(key.into(), x.clone());
        }
    }
    if let Some(f) = v.get("promptFeedback") {
        extra.insert("promptFeedback".into(), f.clone());
    }
    Ok(Response {
        id: response_id,
        model: str_of(&v, "modelVersion").unwrap_or_default().to_owned(),
        content,
        finish,
        native_finish: blocked.filter(|_| candidate.is_none()).map_or(native, str::to_owned),
        usage: v.get("usageMetadata").filter(|u| u.is_object()).map(usage_of),
        extra,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Text(u32),
    Reasoning(u32),
}

struct Reader {
    started: bool,
    next: u32,
    open: Option<Open>,
    response_id: String,
    calls: usize,
    finish: Option<String>,
    usage: Option<Usage>,
    stopped: bool,
}

impl Reader {
    fn close(&mut self, out: &mut Vec<Event>) {
        if let Some(Open::Text(index) | Open::Reasoning(index)) = self.open.take() {
            out.push(Event::ContentStop { index });
        }
    }

    fn start(&mut self, out: &mut Vec<Event>, block: BlockStart) -> u32 {
        self.close(out);
        let index = self.next;
        self.next += 1;
        out.push(Event::ContentStart { index, block });
        index
    }

    fn signature(&mut self, out: &mut Vec<Event>, raw: &str) {
        let r = self.start(out, BlockStart::Reasoning);
        out.push(Event::ContentDelta { index: r, delta: Delta::Signature(sign(FAMILY, raw)) });
        out.push(Event::ContentStop { index: r });
    }

    fn stop(&mut self, out: &mut Vec<Event>) {
        if self.stopped || !self.started {
            return;
        }
        self.stopped = true;
        self.close(out);
        let native = self.finish.clone().unwrap_or_default();
        out.push(Event::MessageStop {
            finish: finish_of(&native, self.calls > 0),
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
            self.response_id = str_of(&v, "responseId").unwrap_or_default().to_owned();
            out.push(Event::MessageStart {
                id: self.response_id.clone(),
                model: str_of(&v, "modelVersion").unwrap_or_default().to_owned(),
            });
        }
        // Every chunk carries usage; the last one is complete.
        if let Some(u) = v.get("usageMetadata").filter(|u| u.is_object()) {
            self.usage = Some(usage_of(u));
        }
        let candidate = v.pointer("/candidates/0");
        for part in candidate.and_then(|c| c.pointer("/content/parts")).and_then(Value::as_array).into_iter().flatten()
        {
            if let Some(sig) = str_of(part, "thoughtSignature") {
                self.signature(out, sig);
            }
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                let text = str_of(part, "text").unwrap_or_default();
                let index = match self.open {
                    Some(Open::Reasoning(i)) => i,
                    _ => {
                        let i = self.start(out, BlockStart::Reasoning);
                        self.open = Some(Open::Reasoning(i));
                        i
                    }
                };
                if !text.is_empty() {
                    out.push(Event::ContentDelta { index, delta: Delta::Reasoning(text.to_owned()) });
                }
            } else if let Some(text) = str_of(part, "text") {
                if text.is_empty() {
                    continue;
                }
                let index = match self.open {
                    Some(Open::Text(i)) => i,
                    _ => {
                        let i = self.start(out, BlockStart::Text);
                        self.open = Some(Open::Text(i));
                        i
                    }
                };
                out.push(Event::ContentDelta { index, delta: Delta::Text(text.to_owned()) });
            } else if let Some(call) = part.get("functionCall") {
                // A function call arrives whole.
                self.calls += 1;
                let id = str_of(call, "id").map_or_else(|| made_up_id(&self.response_id, self.calls), str::to_owned);
                let index = self
                    .start(out, BlockStart::ToolCall { id, name: str_of(call, "name").unwrap_or_default().to_owned() });
                let args = call.get("args").map_or_else(|| "{}".to_owned(), Value::to_string);
                out.push(Event::ContentDelta { index, delta: Delta::ToolArguments(args) });
                out.push(Event::ContentStop { index });
            } else {
                let mut raw = part.clone();
                if let Some(o) = raw.as_object_mut() {
                    o.remove("thoughtSignature");
                }
                let index = self.start(out, BlockStart::Provider { family: FAMILY.into(), raw });
                out.push(Event::ContentStop { index });
            }
        }
        if let Some(word) = candidate.and_then(|c| str_of(c, "finishReason")) {
            self.finish = Some(word.to_owned());
        } else if candidate.is_none() && v.pointer("/promptFeedback/blockReason").is_some() {
            self.finish = Some("SAFETY".into());
        }
    }

    fn finish(&mut self, out: &mut Vec<Event>) {
        // Gemini's stream has no end marker: it ends with the connection.
        self.stop(out);
    }
}

/// A reader of `streamGenerateContent?alt=sse` events.
pub fn provider_stream(_profile: &Profile) -> Box<dyn ProviderStream> {
    Box::new(Reader {
        started: false,
        next: 0,
        open: None,
        response_id: String::new(),
        calls: 0,
        finish: None,
        usage: None,
        stopped: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{Message, Params, Reasoning, Stamp};

    fn profile() -> Profile {
        Profile {
            family: FAMILY.into(),
            upstream_model: "gemini-3.8-flash".into(),
            reasoning: ReasoningStyle::AlwaysOn,
            reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
            ..Default::default()
        }
    }

    fn build(req: &Request, p: &Profile) -> (Value, Notes) {
        let mut notes = Notes::default();
        let v = build_request(req, p, &mut notes).unwrap();
        (v, notes)
    }

    fn user(blocks: Vec<Block>) -> Message {
        Message { role: Role::User, content: blocks }
    }

    #[test]
    fn a_request_is_written_in_geminis_terms() {
        let req = Request {
            system: vec![Block::text("be brief")],
            messages: vec![
                user(vec![
                    Block::text("what is this?"),
                    Block::Image {
                        source: ImageSource::Base64 { media_type: "image/png".into(), data: "AAAA".into() },
                        cache: None,
                    },
                ]),
                Message {
                    role: Role::Assistant,
                    content: vec![
                        Block::Reasoning { text: "hidden".into(), signature: None, redacted: None },
                        Block::Reasoning { text: String::new(), signature: Some(sign(FAMILY, "U0lH")), redacted: None },
                        Block::ToolCall {
                            id: "gm_r1_1".into(),
                            name: "look".into(),
                            arguments: r#"{"q":1}"#.into(),
                            cache: None,
                        },
                        Block::ToolCall {
                            id: "real-id".into(),
                            name: "find".into(),
                            arguments: String::new(),
                            cache: None,
                        },
                    ],
                },
                Message {
                    role: Role::Tool,
                    content: vec![
                        Block::ToolResult {
                            tool_call_id: "gm_r1_1".into(),
                            content: vec![Block::text("a")],
                            is_error: false,
                            cache: None,
                        },
                        Block::ToolResult {
                            tool_call_id: "real-id".into(),
                            content: vec![Block::text("b")],
                            is_error: true,
                            cache: None,
                        },
                    ],
                },
                user(vec![Block::text("and now?")]),
            ],
            tools: vec![
                Tool::Function {
                    name: "look".into(),
                    description: Some("d".into()),
                    parameters: json!({ "type": "object" }),
                    strict: None,
                    cache: None,
                },
                Tool::Provider { family: FAMILY.into(), definition: json!({ "googleSearch": {} }) },
            ],
            tool_choice: Some(ToolChoice::Tool("look".into())),
            response_format: Some(ResponseFormat::JsonSchema {
                name: None,
                schema: json!({ "type": "object" }),
                strict: None,
            }),
            stop: vec!["END".into()],
            params: Params {
                max_output_tokens: Some(100),
                temperature: Some(1.0),
                reasoning: Reasoning { effort: Some("high".into()), ..Default::default() },
                ..Default::default()
            },
            ..Default::default()
        };
        let (v, notes) = build(&req, &profile());
        assert_eq!(v["systemInstruction"], json!({ "parts": [{ "text": "be brief" }] }));
        let contents = v["contents"].as_array().unwrap();
        // The tool results and the next user text are one user content.
        assert_eq!(contents.len(), 3, "{v}");
        assert_eq!(contents[0]["parts"][1], json!({ "inlineData": { "mimeType": "image/png", "data": "AAAA" } }));
        let model = &contents[1];
        assert_eq!(model["role"], "model");
        assert_eq!(model["parts"].as_array().unwrap().len(), 2, "thought text is not sent back: {model}");
        assert_eq!(
            model["parts"][0],
            json!({ "functionCall": { "name": "look", "args": { "q": 1 } }, "thoughtSignature": "U0lH" })
        );
        assert_eq!(model["parts"][1], json!({ "functionCall": { "name": "find", "args": {}, "id": "real-id" } }));
        let results = &contents[2]["parts"];
        assert_eq!(results[0], json!({ "functionResponse": { "name": "look", "response": { "result": "a" } } }));
        assert_eq!(
            results[1],
            json!({ "functionResponse": { "name": "find", "response": { "error": "b" }, "id": "real-id" } })
        );
        assert_eq!(results[2], json!({ "text": "and now?" }));
        assert_eq!(v["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"], json!({ "type": "object" }));
        assert_eq!(v["tools"][1], json!({ "googleSearch": {} }));
        assert_eq!(
            v["toolConfig"],
            json!({ "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": ["look"] } })
        );
        let config = &v["generationConfig"];
        assert_eq!((config["maxOutputTokens"].as_u64(), config["stopSequences"].clone()), (Some(100), json!(["END"])));
        assert_eq!(config["responseMimeType"], "application/json");
        assert_eq!(config["thinkingConfig"], json!({ "thinkingLevel": "high", "includeThoughts": true }));
        assert!(notes.adjusted.is_empty() && notes.dropped.is_empty(), "{notes:?}");
    }

    #[test]
    fn reasoning_controls_follow_the_model() {
        let with = |effort: Option<&str>, budget: Option<u64>| Request {
            messages: vec![user(vec![Block::text("hi")])],
            params: Params {
                reasoning: Reasoning { effort: effort.map(str::to_owned), budget_tokens: budget, summary: None },
                ..Default::default()
            },
            ..Default::default()
        };
        // Gemini 3 cannot stop thinking: the lowest level, said.
        let (v, notes) = build(&with(Some("none"), None), &profile());
        assert_eq!(v["generationConfig"]["thinkingConfig"]["thinkingLevel"], "low");
        assert_eq!(notes.adjusted.len(), 1);
        // A budget-style model (Gemini 2.5).
        let budget = Profile { reasoning: ReasoningStyle::Budget, ..profile() };
        let (v, _) = build(&with(Some("none"), None), &budget);
        assert_eq!(v["generationConfig"]["thinkingConfig"], json!({ "thinkingBudget": 0 }));
        let (v, _) = build(&with(None, Some(2048)), &budget);
        assert_eq!(v["generationConfig"]["thinkingConfig"], json!({ "thinkingBudget": 2048, "includeThoughts": true }));
        // Nothing asked: nothing sent.
        let (v, _) = build(&with(None, None), &profile());
        assert!(v.get("generationConfig").is_none(), "{v}");
        // Gemini's own options are merged last, a generationConfig into the gateway's.
        let mut req = with(Some("low"), None);
        req.provider_options.insert(
            FAMILY.into(),
            json!({ "safetySettings": [{ "category": "x" }], "generationConfig": { "mediaResolution": "LOW" } }),
        );
        let (v, _) = build(&req, &profile());
        assert_eq!(v["safetySettings"][0]["category"], "x");
        assert_eq!(v["generationConfig"]["mediaResolution"], "LOW");
        assert_eq!(v["generationConfig"]["thinkingConfig"]["thinkingLevel"], "low");
    }

    #[test]
    fn a_lost_signature_of_the_current_turn_gets_the_placeholder_and_urls_are_refused() {
        let req = Request {
            messages: vec![
                user(vec![Block::text("q")]),
                Message {
                    role: Role::Assistant,
                    content: vec![Block::ToolCall {
                        id: "c".into(),
                        name: "f".into(),
                        arguments: "{}".into(),
                        cache: None,
                    }],
                },
                Message {
                    role: Role::Tool,
                    content: vec![Block::ToolResult {
                        tool_call_id: "c".into(),
                        content: vec![],
                        is_error: false,
                        cache: None,
                    }],
                },
            ],
            ..Default::default()
        };
        let (v, notes) = build(&req, &profile());
        assert_eq!(v["contents"][1]["parts"][0]["thoughtSignature"], SKIP_THOUGHT_SIGNATURE);
        assert_eq!(notes.adjusted.len(), 1);
        let url = Request {
            messages: vec![user(vec![Block::Image {
                source: ImageSource::Url { url: "https://x/i.png".into() },
                cache: None,
            }])],
            ..Default::default()
        };
        let mut notes = Notes::default();
        assert!(build_request(&url, &profile(), &mut notes).is_err());
    }

    const ANSWER: &str = r#"{"responseId":"r1","modelVersion":"gemini-3.8-flash","candidates":[{"index":0,"finishReason":"STOP",
        "content":{"role":"model","parts":[
            {"text":"Think","thought":true},{"text":"ing.","thought":true},
            {"functionCall":{"name":"look","args":{"q":1}},"thoughtSignature":"U0lH"},
            {"functionCall":{"name":"find","args":{},"id":"real-id"}},
            {"executableCode":{"language":"PYTHON","code":"1+1"}}]},
        "groundingMetadata":{"webSearchQueries":["x"]}}],
        "usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":40,"toolUsePromptTokenCount":5,"candidatesTokenCount":10,"thoughtsTokenCount":7,"serviceTier":"STANDARD"}}"#;

    #[test]
    fn an_answer_is_read_into_neutral_terms() {
        let r = read_response(ANSWER.as_bytes(), &profile()).unwrap();
        assert_eq!((r.id.as_str(), r.model.as_str(), r.finish), ("r1", "gemini-3.8-flash", Finish::ToolCalls));
        assert!(matches!(&r.content[0], Block::Reasoning { text, signature: None, .. } if text == "Thinking."));
        assert!(matches!(&r.content[1], Block::Reasoning { signature: Some(s), .. } if s == "gw1.gemini.U0lH"));
        assert!(
            matches!(&r.content[2], Block::ToolCall { id, arguments, .. } if id == "gm_r1_1" && arguments == r#"{"q":1}"#)
        );
        assert!(matches!(&r.content[3], Block::ToolCall { id, .. } if id == "real-id"));
        assert!(matches!(&r.content[4], Block::Provider { family, .. } if family == FAMILY));
        let u = r.usage.unwrap();
        assert_eq!((u.input_tokens, u.cache_read_tokens, u.output_tokens, u.reasoning_tokens), (65, 40, 17, 7));
        assert_eq!(r.extra["groundingMetadata"]["webSearchQueries"][0], "x");
        let blocked = read_response(
            br#"{"promptFeedback":{"blockReason":"SAFETY"},"usageMetadata":{"promptTokenCount":3}}"#,
            &profile(),
        )
        .unwrap();
        assert_eq!((blocked.finish, blocked.native_finish.as_str()), (Finish::ContentFilter, "SAFETY"));
    }

    #[test]
    fn a_signature_goes_round_through_an_anthropic_caller() {
        let r = read_response(ANSWER.as_bytes(), &profile()).unwrap();
        let stamp = Stamp { id: "x".into(), model: "m".into(), created: 0, cost: None, gateway: None, template: None };
        let written = crate::anthropic::write_response(&r, &stamp);
        let mut back = crate::anthropic::parse_request(
            json!({ "model": "m", "max_tokens": 10, "messages": [
                { "role": "user", "content": "q" },
                { "role": "assistant", "content": written["content"] },
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "gm_r1_1", "content": "1" },
                    { "type": "tool_result", "tool_use_id": "real-id", "content": "2" } ] } ] })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        back.params = Params::default();
        let (v, notes) = build(&back, &profile());
        let parts = &v["contents"][1]["parts"];
        assert_eq!(parts[0]["thoughtSignature"], "U0lH", "{v}");
        assert!(parts[0]["functionCall"].get("id").is_none(), "a made-up id does not go back");
        assert_eq!(v["contents"][2]["parts"][0]["functionResponse"]["name"], "look");
        assert!(notes.adjusted.is_empty(), "{notes:?}");
    }

    #[test]
    fn a_stream_is_read_into_neutral_events() {
        let chunks = [
            r#"{"responseId":"r2","modelVersion":"gemini-3.8-flash","candidates":[{"content":{"role":"model","parts":[{"text":"hmm","thought":true}]}}],"usageMetadata":{"promptTokenCount":9}}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hel"}]}}],"usageMetadata":{"promptTokenCount":9}}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"lo"}]}}],"usageMetadata":{"promptTokenCount":9}}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"look","args":{}},"thoughtSignature":"U0lH"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":4,"thoughtsTokenCount":2}}"#,
        ];
        let mut reader = provider_stream(&profile());
        let mut events = Vec::new();
        for c in chunks {
            reader.read("", c.as_bytes(), &mut events);
        }
        reader.finish(&mut events);
        assert!(matches!(&events[0], Event::MessageStart { id, .. } if id == "r2"));
        let starts: Vec<&BlockStart> = events
            .iter()
            .filter_map(|e| match e {
                Event::ContentStart { block, .. } => Some(block),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 4, "thought, text, signature, call: {events:?}");
        assert!(matches!(starts[3], BlockStart::ToolCall { id, .. } if id == "gm_r2_1"));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::ContentDelta { delta: Delta::Signature(s), .. } if s == "gw1.gemini.U0lH"))
        );
        let Some(Event::MessageStop { finish, usage: Some(u), .. }) = events.last() else { panic!("{events:?}") };
        assert_eq!((*finish, u.output_tokens, u.reasoning_tokens), (Finish::ToolCalls, 6, 2));
        // Opens and stops alternate: never two blocks at once.
        let mut open = false;
        for e in &events {
            match e {
                Event::ContentStart { .. } => {
                    assert!(!open, "{events:?}");
                    open = true;
                }
                Event::ContentStop { .. } => open = false,
                _ => {}
            }
        }
        // An error in the stream ends it.
        let mut reader = provider_stream(&profile());
        let mut events = Vec::new();
        reader.read("", br#"{"error":{"code":500,"message":"internal","status":"INTERNAL"}}"#, &mut events);
        assert!(matches!(&events[..], [Event::Error { .. }]));
    }
}
