//! Google's Gemini API, `generateContent` (concept section 8.2).
//!
//! Provider half: managed mode speaks it to a Gemini provider whose `managed_api` is `native`.
//! Thought signatures travel as signed reasoning blocks (`gw1.gemini.<signature>`) right before
//! the block whose part carried them; written back, a signature goes on the next part, as
//! Gemini attached it.
//!
//! Client half: Gemini's SDKs call the gateway's Gemini interface (its paths name the model and
//! the method; the core reads them). A signature of another origin than Gemini's API reaches
//! such a caller wrapped, as base64 bytes its SDK can hold (see [`signature_out`]).

use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use bytes::BytesMut;
use pacod_gw_events::Usage;
use serde_json::{Map, Value, json};

use crate::canonical::{
    Block, BlockStart, Delta, Event, Finish, ImageSource, Message, Notes, Profile, ReasoningStyle, Request, Response,
    ResponseFormat, Role, SKIP_THOUGHT_SIGNATURE, Stamp, Tool, ToolChoice, sign, signed_by,
};
use crate::error::DialectError;
use crate::sse::write_event;
use crate::util::{arguments_value, object, str_of, strings};
use crate::{ClientStream, ProviderStream};

/// The family name of this dialect, in provider blocks and signatures.
pub const FAMILY: &str = "gemini";

/// The origin of this provider's thought signatures: `gemini`, or `vertex` on Vertex AI.
fn origin(profile: &Profile) -> &str {
    if profile.signature_family.is_empty() { FAMILY } else { &profile.signature_family }
}

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
fn model_parts(blocks: &[Block], origin: &str, notes: &mut Notes) -> Vec<Value> {
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
                if let Some(raw) = signed_by(sig, origin) {
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
                let mut parts = model_parts(&m.content, origin(profile), notes);
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
    // What another dialect's caller may send that Gemini has a place for, and what a Gemini
    // caller sent that has no neutral meaning: Gemini's own members under their names, those of
    // `generationConfig` and `toolConfig` under dotted ones.
    let vertex = origin(profile) == "vertex";
    for (key, value) in &request.extra {
        if key == "seed" {
            config.insert("seed".into(), value.clone());
        } else if PASSED.contains(&key.as_str()) || (vertex && VERTEX_PASSED.contains(&key.as_str())) {
            body.insert(key.clone(), value.clone());
        } else if let Some(member) = key.strip_prefix("generationConfig.")
            && !GOVERNED_CONFIG.contains(&member)
        {
            config.insert(member.to_owned(), value.clone());
        } else if let Some(path) = key.strip_prefix("toolConfig.")
            && TOOL_CONFIG_PASSED.contains(&path)
        {
            set_path(body.entry("toolConfig").or_insert_with(|| json!({})), path, value.clone());
        } else {
            note(notes, key);
        }
    }

    // Gemini's own options (`safetySettings`, `cachedContent`, `serviceTier`, …), last; the
    // parameters the gateway governs stay as it set them.
    let mut options = match request.provider_options.get(FAMILY) {
        Some(Value::Object(o)) => o.iter().map(|(k, v)| (camel(k), v.clone())).collect(),
        _ => Map::new(),
    };
    if let Some(Value::Object(add)) = options.shift_remove("generationConfig") {
        for (k, v) in add {
            let k = camel(&k);
            if GOVERNED_CONFIG.contains(&k.as_str()) {
                note(notes, &format!("provider_options.gemini.generationConfig.{k}"));
            } else {
                config.insert(k, v);
            }
        }
    }
    if !config.is_empty() {
        body.insert("generationConfig".into(), Value::Object(config));
    }
    for (key, value) in options {
        body.insert(key, value);
    }
    Ok(Value::Object(body))
}

/// Members of `generationConfig` the gateway governs (concept section 7.2): set from the
/// template and the caller's parameters, never by a pass-through.
const GOVERNED_CONFIG: &[&str] =
    &["maxOutputTokens", "temperature", "topP", "topK", "presencePenalty", "frequencyPenalty", "thinkingConfig"];

/// Members of a Gemini caller's request that reach a Gemini model as they are.
const PASSED: &[&str] = &["safetySettings", "cachedContent", "serviceTier"];

/// Members of a Gemini caller's `toolConfig` that have no neutral meaning (the mode does).
const TOOL_CONFIG_PASSED: &[&str] = &["functionCallingConfig.allowedFunctionNames", "retrievalConfig"];

/// Members only Vertex AI knows, passed to it alone. `routingConfig` is not among them: it
/// names models the gateway did not choose.
const VERTEX_PASSED: &[&str] = &["labels"];

/// Sets a member at a dotted path, making the objects on the way.
fn set_path(target: &mut Value, path: &str, value: Value) {
    let (head, rest) = match path.split_once('.') {
        Some((h, r)) => (h, Some(r)),
        None => (path, None),
    };
    let Some(obj) = target.as_object_mut() else { return };
    match rest {
        None => drop(obj.insert(head.to_owned(), value)),
        Some(rest) => set_path(obj.entry(head).or_insert_with(|| json!({})), rest, value),
    }
}

/// `max_output_tokens` → `maxOutputTokens`: Google's JSON takes both spellings.
pub(crate) fn camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// `maxOutputTokens` → `max_output_tokens`.
fn snake(camel: &str) -> String {
    let mut out = String::with_capacity(camel.len() + 4);
    for c in camel.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
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
pub fn read_response(body: &[u8], profile: &Profile) -> Result<Response, DialectError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| DialectError::bad_response(format!("the provider's answer is not JSON: {e}")))?;
    let response_id = str_of(&v, "responseId").unwrap_or_default().to_owned();
    let mut content = Vec::new();
    let mut calls = 0;
    let candidate = v.pointer("/candidates/0");
    for part in candidate.and_then(|c| c.pointer("/content/parts")).and_then(Value::as_array).into_iter().flatten() {
        if let Some(sig) = str_of(part, "thoughtSignature") {
            content.push(Block::Reasoning {
                text: String::new(),
                signature: Some(sign(origin(profile), sig)),
                redacted: None,
            });
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
    /// The origin the signatures are labelled with.
    origin: String,
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
        out.push(Event::ContentDelta { index: r, delta: Delta::Signature(sign(&self.origin, raw)) });
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
pub fn provider_stream(profile: &Profile) -> Box<dyn ProviderStream> {
    Box::new(Reader {
        origin: origin(profile).to_owned(),
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

// --- client half: reading a Gemini caller's request -----------------------------------------------

/// Marks a signature the gateway wrapped for a Gemini caller. Gemini's SDKs hold
/// `thoughtSignature` as bytes, written in base64, so a signature of another origin (Vertex
/// AI's `gw1.vertex.…`, Anthropic's) travels as the base64 of this mark and its text. Gemini's
/// own signatures are protocol buffers, whose first byte is never 0x00: they pass as they are.
const WRAPPED: &[u8] = b"\0gw1\0";
/// The same for reasoning a provider gave encrypted (Anthropic's `redacted_thinking`).
const WRAPPED_REDACTED: &[u8] = b"\0gw1r\0";

/// A reasoning block's signature as a Gemini caller receives it.
pub fn signature_out(signature: &str) -> String {
    match signed_by(signature, FAMILY) {
        Some(raw) => raw.to_owned(),
        None => STANDARD.encode([WRAPPED, signature.as_bytes()].concat()),
    }
}

/// Base64 in either alphabet, padded or not: an SDK may write back the bytes it read in the
/// other one.
fn decode_base64(value: &str) -> Option<Vec<u8>> {
    let normal: String = value
        .trim()
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    STANDARD_NO_PAD.decode(normal).ok()
}

/// What a `thoughtSignature` from a Gemini caller stands for: a block the gateway wrapped, or
/// a signature of Gemini's API.
fn reasoning_in(value: &str, text: String) -> Block {
    if let Some(bytes) = decode_base64(value) {
        if let Some(rest) = bytes.strip_prefix(WRAPPED)
            && let Ok(signature) = String::from_utf8(rest.to_vec())
        {
            return Block::Reasoning { text, signature: Some(signature), redacted: None };
        }
        if let Some(rest) = bytes.strip_prefix(WRAPPED_REDACTED)
            && let Ok(data) = String::from_utf8(rest.to_vec())
        {
            return Block::Reasoning { text: String::new(), signature: None, redacted: Some(data) };
        }
    }
    Block::Reasoning { text, signature: Some(sign(FAMILY, value)), redacted: None }
}

/// A member under its camelCase name or its snake_case one: Google's JSON parser takes both.
fn member<'a>(v: &'a Value, camel_name: &str) -> Option<&'a Value> {
    v.get(camel_name).or_else(|| v.get(snake(camel_name).as_str()))
}

fn take_member(m: &mut Map<String, Value>, camel_name: &str) -> Option<Value> {
    let found = m.shift_remove(camel_name);
    found.or_else(|| m.shift_remove(snake(camel_name).as_str()))
}

fn parts_of(content: &Value) -> &[Value] {
    member(content, "parts").and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// Pairs function responses with the calls they answer: by id, or by name and order where the
/// caller gave none (Gemini's API leaves calls without an id).
#[derive(Default)]
struct Pairing {
    open: Vec<(String, String)>,
    made_up: usize,
}

impl Pairing {
    fn made_up(&mut self) -> String {
        self.made_up += 1;
        format!("{MADE_UP_ID}c{}", self.made_up)
    }

    fn call(&mut self, name: &str, id: Option<&str>) -> String {
        let id = id.map_or_else(|| self.made_up(), str::to_owned);
        self.open.push((name.to_owned(), id.clone()));
        id
    }

    fn response(&mut self, name: &str, id: Option<&str>) -> String {
        let at = match id {
            Some(id) => self.open.iter().position(|(_, i)| i == id),
            None => self.open.iter().position(|(n, _)| n == name),
        };
        match (at, id) {
            (Some(at), _) => self.open.remove(at).1,
            (None, Some(id)) => id.to_owned(),
            (None, None) => self.made_up(),
        }
    }
}

/// Thought text that came in pieces (a stream's chunks, kept by an SDK) is one block, and the
/// signature that closes it belongs to it.
fn push_reasoning(blocks: &mut Vec<Block>, incoming: Block) {
    if let (
        Some(Block::Reasoning { text, signature: open @ None, redacted: None }),
        Block::Reasoning { text: more, signature, redacted: None },
    ) = (blocks.last_mut(), &incoming)
    {
        text.push_str(more);
        open.clone_from(signature);
        return;
    }
    blocks.push(incoming);
}

/// The text of a function's answer, and whether it is an error: Gemini's convention is
/// `{"output": …}` or `{"error": …}`; any other object is the answer as it is.
fn function_result(response: &Value) -> (String, bool) {
    let text = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_owned);
    match response.as_object() {
        Some(o) if o.len() == 1 && o.contains_key("error") => (text(&o["error"]), true),
        Some(o) if o.len() == 1 && (o.contains_key("output") || o.contains_key("result")) => {
            (text(o.get("output").or_else(|| o.get("result")).unwrap_or(&Value::Null)), false)
        }
        _ => (response.to_string(), false),
    }
}

fn read_part(part: &Value, pairing: &mut Pairing, blocks: &mut Vec<Block>) -> Result<(), DialectError> {
    let signature = member(part, "thoughtSignature").and_then(Value::as_str);
    if member(part, "thought").and_then(Value::as_bool) == Some(true) {
        let text = member(part, "text").and_then(Value::as_str).unwrap_or_default().to_owned();
        let incoming = match signature {
            Some(s) => reasoning_in(s, text),
            None => Block::Reasoning { text, signature: None, redacted: None },
        };
        push_reasoning(blocks, incoming);
        return Ok(());
    }
    // A signature on another part stands before the block, as the provider half reads it.
    if let Some(s) = signature {
        blocks.push(reasoning_in(s, String::new()));
    }
    if let Some(text) = member(part, "text").and_then(Value::as_str) {
        match blocks.last_mut() {
            Some(Block::Text { text: t, citations: None, cache: None }) => t.push_str(text),
            _ if text.is_empty() => {}
            _ => blocks.push(Block::text(text)),
        }
    } else if let Some(data) = member(part, "inlineData") {
        let mime = member(data, "mimeType").and_then(Value::as_str).unwrap_or_default().to_owned();
        let bytes = member(data, "data").and_then(Value::as_str).unwrap_or_default().to_owned();
        blocks.push(if mime.starts_with("image/") {
            Block::Image { source: ImageSource::Base64 { media_type: mime, data: bytes }, cache: None }
        } else {
            let source = json!({ "type": "base64", "media_type": mime, "data": bytes });
            Block::Document { body: [("source".to_owned(), source)].into_iter().collect(), cache: None }
        });
    } else if let Some(file) = member(part, "fileData") {
        // A file by reference reaches Gemini as it is; another provider reads it as a document URL.
        let source = json!({
            "type": "url",
            "url": member(file, "fileUri").and_then(Value::as_str).unwrap_or_default(),
            "media_type": member(file, "mimeType").and_then(Value::as_str).unwrap_or("application/pdf"),
        });
        blocks.push(Block::Document { body: [("source".to_owned(), source)].into_iter().collect(), cache: None });
    } else if let Some(call) = member(part, "functionCall") {
        let name = member(call, "name").and_then(Value::as_str).unwrap_or_default();
        let id = pairing.call(name, member(call, "id").and_then(Value::as_str).filter(|i| !i.is_empty()));
        blocks.push(Block::ToolCall {
            id,
            name: name.to_owned(),
            arguments: member(call, "args").map_or_else(String::new, Value::to_string),
            cache: None,
        });
    } else if let Some(answer) = member(part, "functionResponse") {
        let name = member(answer, "name").and_then(Value::as_str).unwrap_or_default();
        let id = pairing.response(name, member(answer, "id").and_then(Value::as_str).filter(|i| !i.is_empty()));
        let (text, is_error) = function_result(member(answer, "response").unwrap_or(&Value::Null));
        blocks.push(Block::ToolResult { tool_call_id: id, content: vec![Block::text(text)], is_error, cache: None });
    } else {
        // Code execution and whatever Gemini adds next: only Gemini understands it.
        let mut raw = part.clone();
        if let Some(o) = raw.as_object_mut() {
            o.shift_remove("thoughtSignature");
            o.shift_remove("thought_signature");
        }
        blocks.push(Block::Provider { family: FAMILY.into(), raw });
    }
    Ok(())
}

fn read_contents(contents: &[Value]) -> Result<Vec<Message>, DialectError> {
    let mut messages: Vec<Message> = Vec::new();
    let mut pairing = Pairing::default();
    for content in contents {
        let role = match member(content, "role").and_then(Value::as_str) {
            None | Some("user" | "function" | "tool") => Role::User,
            Some("model") => Role::Assistant,
            Some(other) => {
                return Err(DialectError::invalid(format!("\"{other}\" is not a role of Gemini's API: user or model")));
            }
        };
        // Contents of one role in a row are one turn, as Gemini reads them.
        if messages.last().is_none_or(|m| m.role != role) {
            messages.push(Message { role, content: Vec::new() });
        }
        let blocks = &mut messages.last_mut().expect("a message was just pushed").content;
        for part in parts_of(content) {
            read_part(part, &mut pairing, blocks)?;
        }
    }
    Ok(messages)
}

/// A schema in Gemini's OpenAPI subset as JSON Schema: types in lower case, `nullable` as a
/// null type, counts as numbers. Property names are kept as they are.
pub fn json_schema_of(schema: &Value) -> Value {
    let Some(obj) = schema.as_object() else { return schema.clone() };
    let nullable = obj.get("nullable").and_then(Value::as_bool) == Some(true);
    let mut out = Map::new();
    for (k, v) in obj {
        let key = camel(k);
        match key.as_str() {
            "type" => match v.as_str().map(str::to_ascii_lowercase) {
                Some(t) if t == "type_unspecified" => {}
                Some(t) if nullable => drop(out.insert(key, json!([t, "null"]))),
                Some(t) => drop(out.insert(key, json!(t))),
                None => drop(out.insert(key, v.clone())),
            },
            "nullable" | "propertyOrdering" | "example" => {}
            "properties" => {
                let props = v.as_object().map(|p| p.iter().map(|(n, s)| (n.clone(), json_schema_of(s))).collect());
                out.insert(key, props.map_or_else(|| v.clone(), Value::Object));
            }
            "items" | "additionalProperties" if v.is_object() => drop(out.insert(key, json_schema_of(v))),
            "anyOf" | "oneOf" | "allOf" | "prefixItems" => {
                let list = v.as_array().map(|a| a.iter().map(json_schema_of).collect());
                out.insert(key, list.map_or_else(|| v.clone(), Value::Array));
            }
            // int64 members are strings in Google's JSON.
            "minItems" | "maxItems" | "minLength" | "maxLength" | "minProperties" | "maxProperties" => {
                let n = v.as_str().and_then(|s| s.parse::<u64>().ok()).map_or_else(|| v.clone(), |n| json!(n));
                out.insert(key, n);
            }
            _ => drop(out.insert(key, v.clone())),
        }
    }
    Value::Object(out)
}

fn read_tools(tools: &[Value], req: &mut Request) -> Result<(), DialectError> {
    for tool in tools {
        let Some(obj) = tool.as_object() else {
            return Err(DialectError::invalid("every entry of tools must be an object"));
        };
        for (key, value) in obj {
            let key = camel(key);
            if key != "functionDeclarations" {
                // Google Search, code execution, URL context, …: passed to Gemini as they are.
                req.tools.push(Tool::Provider { family: FAMILY.into(), definition: json!({ key: value }) });
                continue;
            }
            for f in value.as_array().into_iter().flatten() {
                let Some(name) = member(f, "name").and_then(Value::as_str) else {
                    return Err(DialectError::invalid("every function declaration needs a name"));
                };
                let parameters = member(f, "parametersJsonSchema")
                    .cloned()
                    .or_else(|| member(f, "parameters").map(json_schema_of))
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
                req.tools.push(Tool::Function {
                    name: name.to_owned(),
                    description: member(f, "description").and_then(Value::as_str).map(str::to_owned),
                    parameters,
                    strict: None,
                    cache: None,
                });
            }
        }
    }
    Ok(())
}

fn read_tool_config(config: &Value, req: &mut Request) {
    for (key, value) in config.as_object().into_iter().flatten() {
        let key = camel(key);
        if key != "functionCallingConfig" {
            req.extra.insert(format!("toolConfig.{key}"), value.clone());
            continue;
        }
        let allowed = strings(member(value, "allowedFunctionNames").cloned());
        let mode = member(value, "mode").and_then(Value::as_str).map(str::to_ascii_uppercase);
        req.tool_choice = match mode.as_deref() {
            Some("NONE") => Some(ToolChoice::None),
            Some("ANY") if allowed.len() == 1 => Some(ToolChoice::Tool(allowed[0].clone())),
            Some("ANY") => {
                if !allowed.is_empty() {
                    // Several allowed functions: Gemini takes the list, another provider any tool.
                    req.extra.insert("toolConfig.functionCallingConfig.allowedFunctionNames".into(), json!(allowed));
                }
                Some(ToolChoice::Required)
            }
            Some("AUTO" | "VALIDATED") => Some(ToolChoice::Auto),
            _ => None,
        };
    }
}

fn read_generation_config(config: Map<String, Value>, req: &mut Request) {
    let (mut mime, mut schema, mut json_schema) = (None, None, None);
    for (key, value) in config {
        let key = camel(&key);
        let p = &mut req.params;
        match key.as_str() {
            "maxOutputTokens" => p.max_output_tokens = value.as_u64(),
            "temperature" => p.temperature = value.as_f64(),
            "topP" => p.top_p = value.as_f64(),
            "topK" => p.top_k = value.as_f64(),
            "presencePenalty" => p.presence_penalty = value.as_f64(),
            "frequencyPenalty" => p.frequency_penalty = value.as_f64(),
            "stopSequences" => req.stop = strings(Some(value)),
            "seed" => drop(req.extra.insert("seed".into(), value)),
            "candidateCount" if value.as_u64() == Some(1) => {}
            "thinkingConfig" => {
                let r = &mut p.reasoning;
                if let Some(level) = member(&value, "thinkingLevel").and_then(Value::as_str)
                    && !level.eq_ignore_ascii_case("THINKING_LEVEL_UNSPECIFIED")
                {
                    r.effort = Some(level.to_ascii_lowercase());
                }
                match member(&value, "thinkingBudget").and_then(Value::as_i64) {
                    Some(0) => r.effort = Some("none".into()),
                    Some(b) if b > 0 => r.budget_tokens = Some(b as u64),
                    // -1: the model decides.
                    _ => {}
                }
                if let Some(include) = member(&value, "includeThoughts").and_then(Value::as_bool) {
                    r.summary = Some(include);
                }
            }
            "responseMimeType" => mime = value.as_str().map(str::to_owned),
            "responseSchema" => schema = Some(value),
            "responseJsonSchema" => json_schema = Some(value),
            _ => drop(req.extra.insert(format!("generationConfig.{key}"), value)),
        }
    }
    match mime.as_deref() {
        None | Some("application/json" | "text/plain") => {
            req.response_format = match (json_schema, schema) {
                (Some(s), _) => Some(ResponseFormat::JsonSchema { name: None, schema: s, strict: None }),
                (None, Some(s)) => {
                    Some(ResponseFormat::JsonSchema { name: None, schema: json_schema_of(&s), strict: None })
                }
                (None, None) if mime.as_deref() == Some("application/json") => Some(ResponseFormat::JsonObject),
                (None, None) => None,
            };
        }
        // Enumerations (`text/x.enum`) and whatever else only Gemini knows.
        Some(other) => {
            req.extra.insert("generationConfig.responseMimeType".into(), json!(other));
            for (name, v) in [("responseSchema", schema), ("responseJsonSchema", json_schema)] {
                if let Some(v) = v {
                    req.extra.insert(format!("generationConfig.{name}"), v);
                }
            }
        }
    }
}

/// Client half: reads a `generateContent` request, or the members of a `countTokens` one. The
/// model and whether to stream are in the path; the core sets them.
pub fn parse_request(body: &[u8]) -> Result<Request, DialectError> {
    let mut top = object(body)?;
    let mut req = Request { stream_usage: true, ..Default::default() };
    req.gateway = top.shift_remove("gateway");
    // `countTokens` names the model in its `generateContentRequest`; the path decides.
    top.shift_remove("model");
    let contents = match take_member(&mut top, "contents") {
        Some(Value::Array(c)) => c,
        _ => return Err(DialectError::invalid("contents is required and must be a list")),
    };
    req.messages = read_contents(&contents)?;
    req.system = match take_member(&mut top, "systemInstruction") {
        Some(Value::String(s)) if !s.is_empty() => vec![Block::text(s)],
        Some(content @ Value::Object(_)) => parts_of(&content)
            .iter()
            .filter_map(|p| member(p, "text").and_then(Value::as_str))
            .filter(|t| !t.is_empty())
            .map(Block::text)
            .collect(),
        _ => Vec::new(),
    };
    if let Some(Value::Array(tools)) = take_member(&mut top, "tools") {
        read_tools(&tools, &mut req)?;
    }
    if let Some(config) = take_member(&mut top, "toolConfig") {
        read_tool_config(&config, &mut req);
    }
    if let Some(Value::Object(config)) = take_member(&mut top, "generationConfig") {
        read_generation_config(config, &mut req);
    }
    for (key, value) in top {
        req.extra.insert(camel(&key), value);
    }
    Ok(req)
}

// --- client half: writing an answer ----------------------------------------------------------------

/// Gemini's own words for why a candidate ended: given to a Gemini caller as the provider said them.
const OWN_FINISH_REASONS: &[&str] = &[
    "STOP",
    "MAX_TOKENS",
    "SAFETY",
    "RECITATION",
    "LANGUAGE",
    "OTHER",
    "BLOCKLIST",
    "PROHIBITED_CONTENT",
    "SPII",
    "MALFORMED_FUNCTION_CALL",
    "IMAGE_SAFETY",
    "UNEXPECTED_TOOL_CALL",
    "TOO_MANY_TOOL_CALLS",
];

fn finish_reason(finish: Finish, native: &str) -> &str {
    if OWN_FINISH_REASONS.contains(&native) {
        return native;
    }
    match finish {
        Finish::Length => "MAX_TOKENS",
        Finish::ContentFilter => "SAFETY",
        // Gemini says STOP when it calls a function too.
        Finish::Stop | Finish::ToolCalls | Finish::Paused => "STOP",
    }
}

/// `usageMetadata`, the inverse of how the provider half reads it: the prompt includes cached
/// tokens, and the thoughts are counted apart from the candidates.
pub fn usage_metadata(u: &Usage) -> Value {
    let prompt = u.input_tokens + u.cache_read_tokens + u.cache_write_tokens;
    let thoughts = u.reasoning_tokens;
    let candidates = u.output_tokens.saturating_sub(thoughts);
    let mut m = json!({
        "promptTokenCount": prompt,
        "candidatesTokenCount": candidates,
        "totalTokenCount": prompt + candidates + thoughts,
    });
    if u.cache_read_tokens > 0 {
        m["cachedContentTokenCount"] = json!(u.cache_read_tokens);
    }
    if thoughts > 0 {
        m["thoughtsTokenCount"] = json!(thoughts);
    }
    m
}

fn call_part(id: &str, name: &str, arguments: &str) -> Value {
    let mut call = json!({ "name": name, "args": arguments_value(arguments).unwrap_or_else(|_| json!({})) });
    if !id.is_empty() && !id.starts_with(MADE_UP_ID) {
        call["id"] = json!(id);
    }
    json!({ "functionCall": call })
}

/// The parts of an answer. A signature of its own goes on the part after it, as Gemini put it;
/// one that closes reasoning text (Anthropic's) on that text's part.
fn answer_parts(blocks: &[Block]) -> Vec<Value> {
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
            Block::Reasoning { redacted: Some(data), .. } => {
                let wrapped = STANDARD.encode([WRAPPED_REDACTED, data.as_bytes()].concat());
                push(&mut parts, json!({ "text": "", "thought": true, "thoughtSignature": wrapped }), &mut pending);
            }
            Block::Reasoning { text, signature, .. } if !text.is_empty() => {
                let mut part = json!({ "text": text, "thought": true });
                if let Some(sig) = signature {
                    part["thoughtSignature"] = json!(signature_out(sig));
                }
                push(&mut parts, part, &mut pending);
            }
            Block::Reasoning { signature: Some(sig), .. } => pending = Some(signature_out(sig)),
            Block::Reasoning { .. } => {}
            Block::Text { text, .. } => push(&mut parts, json!({ "text": text }), &mut pending),
            Block::ToolCall { id, name, arguments, .. } => {
                push(&mut parts, call_part(id, name, arguments), &mut pending)
            }
            Block::Provider { family, raw } if family == FAMILY => push(&mut parts, raw.clone(), &mut pending),
            Block::Provider { .. } | Block::Image { .. } | Block::Document { .. } | Block::ToolResult { .. } => {}
        }
    }
    // A signature after everything: on an empty text part, as Gemini ends a turn.
    if let Some(sig) = pending {
        parts.push(json!({ "text": "", "thoughtSignature": sig }));
    }
    parts
}

/// Client half: writes a `GenerateContentResponse`.
pub fn write_response(response: &Response, stamp: &Stamp) -> Value {
    let mut candidate = json!({
        "content": { "parts": answer_parts(&response.content), "role": "model" },
        "finishReason": finish_reason(response.finish, &response.native_finish),
        "index": 0,
    });
    for key in ["groundingMetadata", "citationMetadata", "urlContextMetadata", "safetyRatings"] {
        if let Some(x) = response.extra.get(key) {
            candidate[key] = x.clone();
        }
    }
    let mut out = json!({ "candidates": [candidate] });
    if let Some(feedback) = response.extra.get("promptFeedback") {
        out["promptFeedback"] = feedback.clone();
    }
    if let Some(u) = &response.usage {
        out["usageMetadata"] = usage_metadata(u);
    }
    out["modelVersion"] = json!(stamp.model);
    out["responseId"] = json!(stamp.id);
    out
}

/// Writes a stream of `GenerateContentResponse` chunks: server-sent events (`alt=sse`, what the
/// SDKs ask for) or the elements of one JSON array.
struct ClientWriter {
    stamp: Stamp,
    sse: bool,
    /// A chunk went out (the array's next element needs a comma).
    written: bool,
    closed: bool,
    /// A signature waiting for the part it belongs to.
    pending: Option<String>,
    /// The tool call being collected: Gemini sends each call whole. Index, id, name, arguments.
    call: Option<(u32, String, String, String)>,
    /// The reasoning block whose text went out: a signature on it closes that text.
    thinking: Option<u32>,
}

impl ClientWriter {
    fn emit(&mut self, out: &mut BytesMut, chunk: &Value) {
        let data = chunk.to_string();
        if self.sse {
            write_event(out, "", data.as_bytes());
        } else {
            out.extend_from_slice(if self.written { b",\r\n" } else { b"[" });
            out.extend_from_slice(data.as_bytes());
        }
        self.written = true;
    }

    fn part(&mut self, out: &mut BytesMut, mut part: Value) {
        if let Some(sig) = self.pending.take() {
            part["thoughtSignature"] = json!(sig);
        }
        let chunk = json!({
            "candidates": [{ "content": { "parts": [part], "role": "model" }, "index": 0 }],
            "modelVersion": self.stamp.model,
            "responseId": self.stamp.id,
        });
        self.emit(out, &chunk);
    }

    fn close(&mut self, out: &mut BytesMut) {
        if !self.sse && !self.closed {
            if !self.written {
                out.extend_from_slice(b"[");
            }
            out.extend_from_slice(b"]");
        }
        self.closed = true;
    }
}

impl ClientStream for ClientWriter {
    fn finalize(&mut self, _cost: Option<Value>, _gateway: Option<Value>) {
        // Gemini's shape has no place for either; the cost of a stream is in the request log.
    }

    fn write(&mut self, event: &Event, out: &mut BytesMut) {
        if self.closed {
            return;
        }
        match event {
            Event::MessageStart { id, model } => {
                if self.stamp.id.is_empty() {
                    self.stamp.id = id.clone();
                }
                if self.stamp.model.is_empty() {
                    self.stamp.model = model.clone();
                }
            }
            Event::ContentStart { index, block } => match block {
                BlockStart::ToolCall { id, name } => {
                    self.call = Some((*index, id.clone(), name.clone(), String::new()))
                }
                BlockStart::Provider { family, raw } if family == FAMILY => self.part(out, raw.clone()),
                BlockStart::Text | BlockStart::Reasoning | BlockStart::Provider { .. } => {}
            },
            Event::ContentDelta { index, delta } => match delta {
                Delta::Text(t) if !t.is_empty() => self.part(out, json!({ "text": t })),
                Delta::Reasoning(t) if !t.is_empty() => {
                    self.thinking = Some(*index);
                    self.part(out, json!({ "text": t, "thought": true }));
                }
                Delta::ToolArguments(a) => {
                    if let Some((_, _, _, args)) = self.call.as_mut() {
                        args.push_str(a);
                    }
                }
                // Anthropic signs the reasoning text it just sent; Gemini a part that follows.
                Delta::Signature(s) if self.thinking == Some(*index) => {
                    self.part(out, json!({ "text": "", "thought": true, "thoughtSignature": signature_out(s) }));
                }
                Delta::Signature(s) => self.pending = Some(signature_out(s)),
                Delta::Text(_) | Delta::Reasoning(_) | Delta::Citation(_) | Delta::Provider(_) => {}
            },
            Event::ContentStop { index } => {
                if self.thinking == Some(*index) {
                    self.thinking = None;
                }
                if let Some((at, id, name, args)) = self.call.take_if(|c| c.0 == *index) {
                    let _ = at;
                    self.part(out, call_part(&id, &name, &args));
                }
            }
            Event::MessageStop { finish, native_finish, usage } => {
                let mut last = json!({ "text": "" });
                if let Some(sig) = self.pending.take() {
                    last["thoughtSignature"] = json!(sig);
                }
                let mut chunk = json!({
                    "candidates": [{
                        "content": { "parts": [last], "role": "model" },
                        "finishReason": finish_reason(*finish, native_finish),
                        "index": 0,
                    }],
                });
                if let Some(u) = usage {
                    chunk["usageMetadata"] = usage_metadata(u);
                }
                chunk["modelVersion"] = json!(self.stamp.model);
                chunk["responseId"] = json!(self.stamp.id);
                self.emit(out, &chunk);
                self.close(out);
            }
            Event::Ping => {}
            Event::Error { kind, message } => {
                let (code, status) =
                    if kind == "upstream_timeout" { (504, "DEADLINE_EXCEEDED") } else { (500, "INTERNAL") };
                self.emit(out, &json!({ "error": { "code": code, "message": message, "status": status } }));
                self.close(out);
            }
        }
    }

    fn end(&mut self, out: &mut BytesMut) {
        self.close(out);
    }
}

/// Client half: a writer of Gemini's stream, as server-sent events (`sse`) or one JSON array.
pub fn client_stream(stamp: Stamp, sse: bool) -> Box<dyn ClientStream> {
    Box::new(ClientWriter { stamp, sse, written: false, closed: false, pending: None, call: None, thinking: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{Params, Reasoning};

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

    // --- client half ---

    fn stamp() -> Stamp {
        Stamp { id: "resp-1".into(), model: "gemini/flash".into(), ..Default::default() }
    }

    #[test]
    fn a_gemini_request_is_read_into_neutral_terms() {
        let body = json!({
            "system_instruction": { "parts": [{ "text": "be brief" }] },
            "contents": [
                { "role": "user", "parts": [{ "text": "weather?" }, { "inline_data": { "mime_type": "image/png", "data": "AAAA" } }] },
                { "role": "model", "parts": [
                    { "text": "Let me ", "thought": true }, { "text": "check.", "thought": true },
                    { "functionCall": { "name": "get_weather", "args": { "city": "Paris" } }, "thoughtSignature": "U0lH" },
                    { "functionCall": { "name": "get_weather", "args": { "city": "Rome" } } } ] },
                { "role": "user", "parts": [
                    { "functionResponse": { "name": "get_weather", "response": { "output": "sunny" } } },
                    { "functionResponse": { "name": "get_weather", "response": { "error": "no data" } } } ] }
            ],
            "tools": [
                { "functionDeclarations": [{ "name": "get_weather", "description": "d",
                    "parameters": { "type": "OBJECT", "properties": { "city": { "type": "STRING", "nullable": true }, "days": { "type": "ARRAY", "items": { "type": "INTEGER" }, "minItems": "1" } }, "required": ["city"], "propertyOrdering": ["city"] } }] },
                { "google_search": {} }
            ],
            "toolConfig": { "functionCallingConfig": { "mode": "ANY", "allowedFunctionNames": ["get_weather"] } },
            "generationConfig": {
                "maxOutputTokens": 100, "temperature": 0.5, "stop_sequences": ["END"], "seed": 7,
                "thinking_config": { "thinkingLevel": "LOW", "includeThoughts": true },
                "responseMimeType": "application/json", "responseSchema": { "type": "OBJECT" },
                "responseModalities": ["TEXT"]
            },
            "safetySettings": [{ "category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_NONE" }],
            "gateway": { "tags": { "team": "a" } }
        });
        let req = parse_request(body.to_string().as_bytes()).unwrap();
        assert_eq!(req.system, vec![Block::text("be brief")]);
        assert!(
            matches!(&req.messages[0].content[1], Block::Image { source: ImageSource::Base64 { media_type, .. }, .. } if media_type == "image/png")
        );
        let model = &req.messages[1].content;
        assert!(
            matches!(&model[0], Block::Reasoning { text, signature: None, .. } if text == "Let me check."),
            "{model:?}"
        );
        assert!(matches!(&model[1], Block::Reasoning { signature: Some(s), .. } if s == "gw1.gemini.U0lH"));
        // Calls without ids get made-up ones; the answers are paired with them by name and order.
        let ids: Vec<&str> = model
            .iter()
            .filter_map(|b| match b {
                Block::ToolCall { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["gm_c1", "gm_c2"]);
        let results = &req.messages[2].content;
        assert!(
            matches!(&results[0], Block::ToolResult { tool_call_id, is_error: false, content, .. } if tool_call_id == "gm_c1" && content[0] == Block::text("sunny"))
        );
        assert!(
            matches!(&results[1], Block::ToolResult { tool_call_id, is_error: true, .. } if tool_call_id == "gm_c2")
        );
        // Gemini's schema becomes JSON Schema.
        let Tool::Function { parameters, .. } = &req.tools[0] else { panic!() };
        assert_eq!(
            parameters,
            &json!({ "type": "object", "properties": { "city": { "type": ["string", "null"] }, "days": { "type": "array", "items": { "type": "integer" }, "minItems": 1 } }, "required": ["city"] })
        );
        assert!(
            matches!(&req.tools[1], Tool::Provider { definition, .. } if definition == &json!({ "googleSearch": {} }))
        );
        assert_eq!(req.tool_choice, Some(ToolChoice::Tool("get_weather".into())));
        let p = &req.params;
        assert_eq!(
            (p.max_output_tokens, p.temperature, req.stop.as_slice()),
            (Some(100), Some(0.5), &["END".to_owned()][..])
        );
        assert_eq!((p.reasoning.effort.as_deref(), p.reasoning.summary), (Some("low"), Some(true)));
        assert_eq!(
            req.response_format,
            Some(ResponseFormat::JsonSchema { name: None, schema: json!({ "type": "object" }), strict: None })
        );
        assert_eq!(
            req.extra.keys().collect::<Vec<_>>(),
            ["seed", "generationConfig.responseModalities", "safetySettings"]
        );
        assert_eq!(req.gateway, Some(json!({ "tags": { "team": "a" } })));
        // A budget of 0 switches thinking off; contents are required.
        let off =
            parse_request(br#"{"contents":[],"generationConfig":{"thinkingConfig":{"thinkingBudget":0}}}"#).unwrap();
        assert_eq!(off.params.reasoning.effort.as_deref(), Some("none"));
        assert!(parse_request(br#"{"generationConfig":{}}"#).is_err());
        assert!(parse_request(br#"{"contents":[{"role":"system","parts":[]}]}"#).is_err());
    }

    #[test]
    fn signatures_reach_gemini_callers_as_base64_and_come_back() {
        // Gemini's own as it is; another origin wrapped.
        assert_eq!(signature_out("gw1.gemini.U0lH"), "U0lH");
        let wrapped = signature_out("gw1.vertex.QUJD");
        assert!(STANDARD.decode(&wrapped).unwrap().starts_with(WRAPPED));
        assert_eq!(
            reasoning_in(&wrapped, String::new()),
            Block::Reasoning { text: String::new(), signature: Some("gw1.vertex.QUJD".into()), redacted: None }
        );
        // An SDK may write the bytes back in the URL-safe alphabet, without padding.
        let anthropic = "EqQBCkYIBxgCKkDrL+/x==";
        let url_safe = signature_out(anthropic).replace('+', "-").replace('/', "_").trim_end_matches('=').to_owned();
        assert!(
            matches!(reasoning_in(&url_safe, String::new()), Block::Reasoning { signature: Some(s), .. } if s == anthropic)
        );
        // Claude's thinking keeps its text and its signature together through a Gemini caller.
        let response = Response {
            id: "msg_1".into(),
            model: "claude".into(),
            content: vec![
                Block::Reasoning { text: "Let me think.".into(), signature: Some(anthropic.into()), redacted: None },
                Block::Reasoning { text: String::new(), signature: None, redacted: Some("ZW5j".into()) },
                Block::ToolCall {
                    id: "toolu_1".into(),
                    name: "look".into(),
                    arguments: r#"{"q":1}"#.into(),
                    cache: None,
                },
            ],
            finish: Finish::ToolCalls,
            native_finish: "tool_use".into(),
            usage: None,
            extra: Map::new(),
        };
        let written = write_response(&response, &stamp());
        let content = &written["candidates"][0]["content"];
        assert_eq!(content["parts"][2]["functionCall"]["id"], "toolu_1");
        let back = parse_request(
            json!({ "contents": [{ "role": "user", "parts": [{ "text": "q" }] }, content] }).to_string().as_bytes(),
        )
        .unwrap();
        assert_eq!(back.messages[1].content, response.content);
    }

    #[test]
    fn no_recorded_gemini_signature_looks_wrapped() {
        // Wrapping relies on Gemini's signatures never starting with 0x00 (a protocol buffer's
        // first byte is a field tag). Every signature the recordings hold, in bodies and in
        // streams kept as text.
        fn walk(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(o) => {
                    for (k, x) in o {
                        match (k.as_str(), x) {
                            ("thoughtSignature" | "thought_signature", Value::String(s)) => out.push(s.clone()),
                            _ => walk(x, out),
                        }
                    }
                }
                Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                Value::String(s) if s.contains("ignature") => {
                    for line in s.lines() {
                        if let Ok(x) = serde_json::from_str::<Value>(line.strip_prefix("data: ").unwrap_or(line)) {
                            walk(&x, out);
                        }
                    }
                }
                _ => {}
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pacod-gw-testkit/fixtures");
        let mut seen = 0;
        for dir in ["gemini", "gemini-openai", "vertex"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir)) else { continue };
            for entry in entries.flatten() {
                let Ok(v) = serde_json::from_slice::<Value>(&std::fs::read(entry.path()).unwrap()) else { continue };
                let mut found = Vec::new();
                walk(&v, &mut found);
                for value in found.iter().filter(|s| s.as_str() != SKIP_THOUGHT_SIGNATURE) {
                    let bytes = decode_base64(value).unwrap_or_else(|| panic!("{value} in {:?}", entry.path()));
                    assert_ne!(bytes.first(), Some(&0), "{:?}", entry.path());
                    seen += 1;
                }
            }
        }
        assert!(seen > 10, "the recordings hold signatures: {seen}");
    }

    #[test]
    fn an_answer_is_written_in_geminis_shape() {
        let r = read_response(ANSWER.as_bytes(), &profile()).unwrap();
        let w = write_response(&r, &stamp());
        let parts = &w["candidates"][0]["content"]["parts"];
        assert_eq!(parts[0], json!({ "text": "Thinking.", "thought": true }));
        assert_eq!(parts[1]["thoughtSignature"], "U0lH", "Gemini's own signature, on the call it came with");
        assert!(parts[1]["functionCall"].get("id").is_none(), "a made-up id is not shown");
        assert_eq!(parts[2]["functionCall"]["id"], "real-id");
        assert_eq!(parts[3]["executableCode"]["code"], "1+1");
        assert_eq!(
            (w["candidates"][0]["finishReason"].as_str(), w["responseId"].as_str()),
            (Some("STOP"), Some("resp-1"))
        );
        assert_eq!(w["candidates"][0]["groundingMetadata"]["webSearchQueries"][0], "x");
        // usageMetadata reads back as the same usage.
        let u = r.usage.unwrap();
        assert_eq!(usage_of(&w["usageMetadata"]), u);
        assert_eq!(w["usageMetadata"]["totalTokenCount"], 105 + 10 + 7);
        let lengthy = Response { finish: Finish::Length, native_finish: "length".into(), ..r };
        assert_eq!(write_response(&lengthy, &stamp())["candidates"][0]["finishReason"], "MAX_TOKENS");
    }

    fn stream_of(events: &[Event], sse: bool) -> String {
        let mut w = client_stream(stamp(), sse);
        let mut out = BytesMut::new();
        for e in events {
            w.write(e, &mut out);
        }
        w.end(&mut out);
        String::from_utf8(out.to_vec()).unwrap()
    }

    #[test]
    fn a_stream_is_written_as_events_or_as_an_array() {
        let events = vec![
            Event::MessageStart { id: "m1".into(), model: "x".into() },
            Event::ContentStart { index: 0, block: BlockStart::Reasoning },
            Event::ContentDelta { index: 0, delta: Delta::Reasoning("hm".into()) },
            Event::ContentDelta { index: 0, delta: Delta::Signature("c2ln".into()) },
            Event::ContentStop { index: 0 },
            Event::ContentStart { index: 1, block: BlockStart::Text },
            Event::ContentDelta { index: 1, delta: Delta::Text("Hel".into()) },
            Event::ContentDelta { index: 1, delta: Delta::Text("lo".into()) },
            Event::ContentStop { index: 1 },
            Event::ContentStart { index: 2, block: BlockStart::Reasoning },
            Event::ContentDelta { index: 2, delta: Delta::Signature("gw1.gemini.U0lH".into()) },
            Event::ContentStop { index: 2 },
            Event::ContentStart { index: 3, block: BlockStart::ToolCall { id: "call_1".into(), name: "look".into() } },
            Event::ContentDelta { index: 3, delta: Delta::ToolArguments(r#"{"q""#.into()) },
            Event::ContentDelta { index: 3, delta: Delta::ToolArguments(":1}".into()) },
            Event::ContentStop { index: 3 },
            Event::MessageStop {
                finish: Finish::ToolCalls,
                native_finish: "tool_calls".into(),
                usage: Some(Usage { input_tokens: 5, output_tokens: 3, ..Default::default() }),
            },
        ];
        let sse = stream_of(&events, true);
        let chunks: Vec<Value> =
            sse.lines().filter_map(|l| l.strip_prefix("data: ")).map(|d| serde_json::from_str(d).unwrap()).collect();
        let parts: Vec<&Value> = chunks.iter().map(|c| &c["candidates"][0]["content"]["parts"][0]).collect();
        assert_eq!(parts[0], &json!({ "text": "hm", "thought": true }));
        assert!(
            parts[1]["thoughtSignature"].as_str().is_some_and(|s| s != "c2ln"),
            "Anthropic's signature, wrapped, closes the thought"
        );
        assert_eq!((parts[2]["text"].as_str(), parts[3]["text"].as_str()), (Some("Hel"), Some("lo")));
        assert_eq!(
            parts[4]["functionCall"],
            json!({ "name": "look", "args": { "q": 1 }, "id": "call_1" }),
            "a call goes whole"
        );
        assert_eq!(parts[4]["thoughtSignature"], "U0lH");
        let last = chunks.last().unwrap();
        assert_eq!(
            (last["candidates"][0]["finishReason"].as_str(), last["usageMetadata"]["promptTokenCount"].as_u64()),
            (Some("STOP"), Some(5))
        );
        assert!(chunks.iter().all(|c| c["responseId"] == "resp-1"));
        // The same as one JSON array.
        let array: Vec<Value> = serde_json::from_str(&stream_of(&events, false)).unwrap();
        assert_eq!(array, chunks);
        // A stream that breaks ends with Google's error, and the array is closed.
        let broken: Vec<Value> = serde_json::from_str(&stream_of(
            &[
                events[0].clone(),
                events[5].clone(),
                events[6].clone(),
                Event::Error { kind: "upstream_timeout".into(), message: "slow".into() },
            ],
            false,
        ))
        .unwrap();
        assert_eq!(broken.last().unwrap()["error"]["status"], "DEADLINE_EXCEEDED");
        assert_eq!(stream_of(&[], false), "[]");
    }

    #[test]
    fn pass_throughs_never_set_what_the_gateway_governs() {
        let mut req = parse_request(
            json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
                    "generationConfig": { "maxOutputTokens": 50, "responseModalities": ["TEXT"] },
                    "labels": { "team": "a" }, "routingConfig": { "manualMode": { "modelName": "gemini-pro" } },
                    "safetySettings": [] })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        // An OpenAI-dialect caller could name these too.
        req.extra.insert("generationConfig.maxOutputTokens".into(), json!(99999));
        req.provider_options.insert(
            FAMILY.into(),
            json!({ "generation_config": { "max_output_tokens": 99999, "topK": 3, "candidateCount": 1 }, "cached_content": "cachedContents/x" }),
        );
        let (v, notes) = build(&req, &profile());
        let config = &v["generationConfig"];
        assert_eq!((config["maxOutputTokens"].as_u64(), config["topK"].as_u64()), (Some(50), None));
        assert_eq!(
            (config["responseModalities"][0].as_str(), config["candidateCount"].as_u64()),
            (Some("TEXT"), Some(1))
        );
        assert_eq!(v["cachedContent"], "cachedContents/x");
        assert!(v.get("safetySettings").is_some() && v.get("labels").is_none() && v.get("routingConfig").is_none());
        assert!(v.get("generation_config").is_none() && v.get("cached_content").is_none());
        for dropped in [
            "labels",
            "routingConfig",
            "generationConfig.maxOutputTokens",
            "provider_options.gemini.generationConfig.maxOutputTokens",
            "provider_options.gemini.generationConfig.topK",
        ] {
            assert!(notes.dropped.iter().any(|d| d == dropped), "{dropped}: {notes:?}");
        }
        // Labels reach Vertex AI.
        let vertex = Profile { signature_family: "vertex".into(), ..profile() };
        assert_eq!(build(&req, &vertex).0["labels"]["team"], "a");
    }
}
