//! Reads usage out of provider responses without touching them (concept section 4.2).
//!
//! The relay hands every response chunk to the client first and feeds a copy to a tap. The
//! tap frames the stream, parses only what accounting needs (token counts, the model that
//! answered, the finish reason) and never produces output.

use std::sync::{Arc, OnceLock};

use bytes::BytesMut;
use pacod_gw_dialects::canonical::{
    DEFAULT_CACHE_WRITE_TOKENS_AT, DEFAULT_CACHED_TOKENS_AT, DEFAULT_REASONING_TOKENS_AT, owned,
};
use pacod_gw_dialects::{add_hidden_reasoning, usage_from_openai};
use pacod_gw_events::Usage;
use serde::Deserialize;
use serde_json::Value;

use crate::model::Family;
use crate::sse::{LineFramer, SseFramer};

/// Non-streamed bodies larger than this are not parsed for usage.
const WHOLE_BODY_CAP: usize = 8 << 20;

/// Where an OpenAI-compatible provider reports cached, written and reasoning tokens inside
/// `usage`, as dotted paths tried in order. From the provider's quirk profile (concept
/// section 8.2).
#[derive(Debug, Clone, PartialEq)]
pub struct UsageFields {
    pub cached: Vec<String>,
    pub reasoning: Vec<String>,
    pub cache_write: Vec<String>,
    /// Reasoning is counted only in `total_tokens` (Gemini's compatible endpoint).
    pub reasoning_outside_completion: bool,
}

impl Default for UsageFields {
    fn default() -> Self {
        Self {
            cached: owned(DEFAULT_CACHED_TOKENS_AT),
            reasoning: owned(DEFAULT_REASONING_TOKENS_AT),
            cache_write: owned(DEFAULT_CACHE_WRITE_TOKENS_AT),
            reasoning_outside_completion: false,
        }
    }
}

/// The places OpenAI and DeepSeek use, for a tap that knows no provider.
pub fn default_usage_fields() -> Arc<UsageFields> {
    static DEFAULT: OnceLock<Arc<UsageFields>> = OnceLock::new();
    DEFAULT.get_or_init(Default::default).clone()
}

/// The response shape a tap understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    OpenAiChat,
    OpenAiResponses,
    OpenAiEmbeddings,
    Anthropic,
    Gemini,
    /// Gemini's `embedContent` and `batchEmbedContents` (no usage, recorded) and Vertex AI's
    /// `predict` (a token count per input).
    GeminiEmbed,
    Ollama,
    /// TypeSafe's System One API: one JSON answer with `model` and `usage.input_tokens`.
    SystemOne,
    /// OpenAI's image endpoints: `usage` on the answer (gpt-image) or on the stream's closing
    /// event; DALL·E reports none.
    OpenAiImages,
    /// OpenAI's speech-to-text endpoints: tokens (gpt-4o-transcribe) or seconds of audio
    /// (`usage.seconds`, `verbose_json`'s `duration`, the last subtitle cue).
    OpenAiTranscription,
    /// Rerank: Cohere's search units, or Jina's and Voyage's tokens.
    Rerank,
}

/// Paths whose request is a form, not JSON: the gateway reads usage from their answers, but
/// proxy mode does not read the model from the form.
pub fn multipart_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    ["/images/edits", "/images/variations", "/audio/transcriptions", "/audio/translations"]
        .iter()
        .any(|p| path.ends_with(p))
}

/// Which response shape a provider path returns, if the gateway knows it. Paths without a
/// known shape are relayed and counted as requests only.
pub fn dialect_for(family: Family, path: &str) -> Option<Dialect> {
    let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    match family {
        Family::Typesafe => path.ends_with("/systemone").then_some(Dialect::SystemOne),
        Family::Cohere => path.ends_with("/rerank").then_some(Dialect::Rerank),
        Family::Openai => openai_dialect(path),
        Family::Anthropic => (path == "/v1/messages").then_some(Dialect::Anthropic),
        Family::Gemini if path.contains(":generateContent") || path.contains(":streamGenerateContent") => {
            Some(Dialect::Gemini)
        }
        Family::Gemini
            if path.ends_with(":embedContent")
                || path.ends_with(":batchEmbedContents")
                || path.ends_with(":predict") =>
        {
            Some(Dialect::GeminiEmbed)
        }
        Family::Gemini => openai_dialect(path),
        Family::Ollama => {
            if path == "/api/chat" || path == "/api/generate" {
                Some(Dialect::Ollama)
            } else {
                openai_dialect(path)
            }
        }
    }
}

fn openai_dialect(path: &str) -> Option<Dialect> {
    if path.ends_with("/chat/completions") || path.ends_with("/completions") {
        Some(Dialect::OpenAiChat)
    } else if path.ends_with("/responses") {
        Some(Dialect::OpenAiResponses)
    } else if path.ends_with("/embeddings") {
        Some(Dialect::OpenAiEmbeddings)
    } else if path.ends_with("/images/generations") || path.ends_with("/images/edits") {
        Some(Dialect::OpenAiImages)
    } else if path.ends_with("/audio/transcriptions") || path.ends_with("/audio/translations") {
        Some(Dialect::OpenAiTranscription)
    } else if path.ends_with("/rerank") {
        Some(Dialect::Rerank)
    } else {
        None
    }
}

/// What a tap found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extracted {
    pub usage: Usage,
    /// False when the provider reported no usage at all.
    pub has_usage: bool,
    /// The model the provider says answered.
    pub model: Option<String>,
    /// The provider's own finish or stop reason.
    pub finish_reason: Option<String>,
    /// True when the stream ended inside an event, which means it was cut.
    pub cut: bool,
    /// The service tier the provider says it served the call at (`batch`, `priority`, `flex`,
    /// …), which can select a price variant.
    pub service_tier: Option<String>,
    /// The provider reported a failure inside a response that had started: an `error` object
    /// in the stream, or a finish reason that means the answer broke off (GLM `network_error`).
    pub stream_error: Option<String>,
}

enum Framing {
    Sse(SseFramer),
    Lines(LineFramer),
    /// A single JSON document, buffered up to a cap. Past it, the last 64 KiB are kept:
    /// OpenAI writes an embeddings answer's `usage` last.
    Whole {
        buf: BytesMut,
        overflow: bool,
    },
}

/// What is kept of a whole body past the cap.
const TAIL: usize = 64 << 10;

pub struct UsageTap {
    dialect: Dialect,
    framing: Framing,
    out: Extracted,
    fields: Arc<UsageFields>,
}

impl UsageTap {
    /// Chooses the framing from the response's content type.
    pub fn new(dialect: Dialect, content_type: &str) -> Self {
        let ct = content_type.to_ascii_lowercase();
        let framing = if ct.starts_with("text/event-stream") {
            Framing::Sse(SseFramer::new())
        } else if ct.starts_with("application/x-ndjson") || ct.starts_with("application/jsonl") {
            Framing::Lines(LineFramer::new())
        } else {
            Framing::Whole { buf: BytesMut::new(), overflow: false }
        };
        Self { dialect, framing, out: Extracted::default(), fields: default_usage_fields() }
    }

    /// Reads cached and reasoning tokens where the provider's quirk profile says they are.
    pub fn with_fields(mut self, fields: Arc<UsageFields>) -> Self {
        self.fields = fields;
        self
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        let (dialect, out, fields) = (self.dialect, &mut self.out, &self.fields);
        match &mut self.framing {
            Framing::Sse(f) => f.feed(chunk, |ev| parse(dialect, &ev.event, &ev.data, out, fields)),
            Framing::Lines(f) => f.feed(chunk, |line| parse(dialect, "", line, out, fields)),
            Framing::Whole { buf, overflow } => {
                if *overflow || buf.len() + chunk.len() > WHOLE_BODY_CAP {
                    *overflow = true;
                    buf.extend_from_slice(chunk);
                    if buf.len() > TAIL {
                        let _ = buf.split_to(buf.len() - TAIL);
                    }
                } else {
                    buf.extend_from_slice(chunk);
                }
            }
        }
    }

    pub fn finish(mut self) -> Extracted {
        let (dialect, out, fields) = (self.dialect, &mut self.out, &self.fields);
        match &mut self.framing {
            Framing::Sse(f) => {
                out.cut = f.pending().iter().any(|b| !b.is_ascii_whitespace());
                f.finish(|ev| parse(dialect, &ev.event, &ev.data, out, fields));
            }
            Framing::Lines(f) => f.finish(|line| parse(dialect, "", line, out, fields)),
            Framing::Whole { buf, overflow } => {
                if !*overflow && !buf.is_empty() {
                    parse(dialect, "", buf, out, fields);
                } else if *overflow && dialect == Dialect::OpenAiEmbeddings {
                    // The tail of a large answer: its last `usage` object.
                    if let Some(at) = memchr::memmem::rfind(buf, b"\"usage\"")
                        && let Some(open) = buf[at..].iter().position(|b| *b == b'{')
                        && let Some(Ok(u)) =
                            serde_json::Deserializer::from_slice(&buf[at + open..]).into_iter::<Value>().next()
                    {
                        let wrapped = serde_json::to_vec(&serde_json::json!({ "usage": u })).unwrap_or_default();
                        parse(dialect, "", &wrapped, out, fields);
                    }
                }
            }
        }
        self.out
    }
}

/// Reads a complete, non-streamed response body.
pub fn parse_whole(dialect: Dialect, body: &[u8], fields: &UsageFields) -> Extracted {
    let mut out = Extracted::default();
    parse(dialect, "", body, &mut out, fields);
    out
}

fn parse(dialect: Dialect, event: &str, data: &[u8], out: &mut Extracted, fields: &UsageFields) {
    if data.is_empty() || data == b"[DONE]" {
        return;
    }
    match dialect {
        Dialect::OpenAiChat | Dialect::OpenAiEmbeddings => openai(data, out, fields),
        Dialect::GeminiEmbed => gemini_embed(data, out),
        Dialect::OpenAiResponses => openai_responses(data, out),
        Dialect::Anthropic => anthropic(event, data, out),
        Dialect::Gemini => gemini(data, out),
        Dialect::Ollama => ollama(data, out),
        Dialect::SystemOne => system_one(data, out),
        Dialect::OpenAiImages => openai_images(data, out),
        Dialect::OpenAiTranscription => openai_transcription(data, out),
        Dialect::Rerank => rerank(data, out),
    }
}

/// A rerank answer's usage, in whichever provider's shape it comes.
fn rerank(data: &[u8], out: &mut Extracted) {
    use pacod_gw_dialects::rerank;
    if let Ok((_, Some(u))) = rerank::read(data, rerank::shape_of(data)) {
        out.usage = u;
        out.has_usage = true;
    }
}

/// A transcription's usage: tokens where the answer carries them, else the audio's length.
/// A stream carries it on `transcript.text.done`.
fn openai_transcription(data: &[u8], out: &mut Extracted) {
    if let Ok(v) = serde_json::from_slice::<Value>(data)
        && let Some(u) = v.get("usage").filter(|u| u.get("type").and_then(Value::as_str) == Some("tokens"))
    {
        let n = |path: &str| u.pointer(path).and_then(Value::as_u64).unwrap_or(0);
        out.usage.input_tokens = n("/input_tokens");
        out.usage.audio_input_tokens = n("/input_token_details/audio_tokens");
        out.usage.output_tokens = n("/output_tokens");
        out.has_usage = true;
        return;
    }
    if let Some(ms) = pacod_gw_dialects::audio::duration_ms(data) {
        out.usage.audio_ms = ms;
        out.has_usage = true;
    }
}

// --- OpenAI's images -------------------------------------------------------------------------

/// gpt-image's usage: text and image tokens in, image tokens out. The output is images, so its
/// tokens are image tokens where the answer does not split them.
fn openai_images(data: &[u8], out: &mut Extracted) {
    let Ok(v) = serde_json::from_slice::<Value>(data) else { return };
    let Some(u) = v.get("usage").filter(|u| u.is_object()) else { return };
    let n = |path: &str| u.pointer(path).and_then(Value::as_u64);
    let Some(input) = n("/input_tokens") else { return };
    let output = n("/output_tokens").unwrap_or(0);
    out.usage.input_tokens = input;
    out.usage.image_input_tokens = n("/input_tokens_details/image_tokens").unwrap_or(0);
    out.usage.output_tokens = output;
    out.usage.image_output_tokens = n("/output_tokens_details/image_tokens").unwrap_or(output);
    out.has_usage = true;
}

// --- TypeSafe --------------------------------------------------------------------------------

#[derive(Deserialize)]
struct SoBody {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<SoUsage>,
}

#[derive(Deserialize)]
struct SoUsage {
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

/// System One answers the questions in `answers` and reports the tokens it read. It writes
/// none in the sense of a chat model, but reports output tokens all the same.
fn system_one(data: &[u8], out: &mut Extracted) {
    let Ok(body) = serde_json::from_slice::<SoBody>(data) else { return };
    if let Some(m) = body.model.filter(|m| !m.is_empty()) {
        out.model = Some(m);
    }
    if let Some(u) = body.usage {
        out.usage.input_tokens = u.input_tokens;
        out.usage.output_tokens = u.output_tokens;
        out.has_usage = true;
    }
}

// --- OpenAI-compatible -----------------------------------------------------------------------

#[derive(Deserialize)]
struct OaBody {
    #[serde(default)]
    model: Option<String>,
    /// Read by paths: providers put cached and reasoning tokens in different places.
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    choices: Vec<OaChoice>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct OaChoice {
    #[serde(default)]
    finish_reason: Option<String>,
    /// Kimi's stream reports usage here, on the finish chunk.
    #[serde(default)]
    usage: Option<Value>,
}

#[derive(Deserialize, Default)]
struct OaPromptDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[derive(Deserialize, Default)]
struct OaCompletionDetails {
    #[serde(default)]
    reasoning_tokens: Option<u64>,
}

fn openai(data: &[u8], out: &mut Extracted, fields: &UsageFields) {
    let Ok(body) = serde_json::from_slice::<OaBody>(data) else { return };
    if let Some(m) = body.model.filter(|m| !m.is_empty()) {
        out.model = Some(m);
    }
    if let Some(r) = body.choices.iter().find_map(|c| c.finish_reason.clone()) {
        if matches!(r.as_str(), "network_error" | "insufficient_system_resource") {
            out.stream_error = Some(format!("the provider stopped the answer: {r}"));
        }
        out.finish_reason = Some(r);
    }
    if let Some(e) = body.error.filter(|e| !e.is_null()) {
        let message = e.get("message").and_then(Value::as_str).map_or_else(|| e.to_string(), str::to_owned);
        out.stream_error = Some(message.chars().take(300).collect());
    }
    if let Some(t) = body.service_tier.filter(|t| !t.is_empty()) {
        out.service_tier = Some(t);
    }
    // The top-level usage, or the one in the choice; the last reported wins on a stream.
    let usage = body
        .usage
        .filter(Value::is_object)
        .or_else(|| body.choices.into_iter().next().and_then(|c| c.usage).filter(Value::is_object));
    if let Some(u) = usage {
        // OpenAI's prompt_tokens includes cached and written tokens; the canonical input does not.
        out.usage = usage_from_openai(&u, &fields.cached, &fields.reasoning, &fields.cache_write);
        if fields.reasoning_outside_completion {
            add_hidden_reasoning(&mut out.usage, &u);
        }
        out.has_usage = true;
    }
}

/// The usage of one OpenAI-dialect response or stream chunk, when it carries one.
pub fn openai_chat_usage(data: &[u8], fields: &UsageFields) -> Option<Usage> {
    let mut out = Extracted::default();
    openai(data, &mut out, fields);
    out.has_usage.then_some(out.usage)
}

#[derive(Deserialize)]
struct OrEnvelope {
    #[serde(default)]
    response: Option<OrBody>,
    #[serde(flatten)]
    top: OrBody,
}

#[derive(Deserialize, Default)]
struct OrBody {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    usage: Option<OrUsage>,
    /// The items, to count the calls of the tools the provider runs.
    #[serde(default)]
    output: Vec<OrItem>,
}

#[derive(Deserialize)]
struct OrItem {
    #[serde(default, rename = "type")]
    kind: String,
}

/// Items of the Responses API that are a call of a tool the provider runs, billed per call.
const RESPONSES_SERVER_TOOLS: &[&str] =
    &["web_search_call", "file_search_call", "code_interpreter_call", "image_generation_call"];

#[derive(Deserialize)]
struct OrUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<OaPromptDetails>,
    #[serde(default)]
    output_tokens_details: Option<OaCompletionDetails>,
}

/// The Responses API: a single object when not streaming, or events whose final one
/// (`response.completed`) carries the whole response under `response`.
fn openai_responses(data: &[u8], out: &mut Extracted) {
    let Ok(env) = serde_json::from_slice::<OrEnvelope>(data) else { return };
    let body = env.response.unwrap_or(env.top);
    if let Some(m) = body.model.filter(|m| !m.is_empty()) {
        out.model = Some(m);
    }
    if let Some(s) = body.status.filter(|s| s != "in_progress") {
        out.finish_reason = Some(s);
    }
    if let Some(t) = body.service_tier.filter(|t| !t.is_empty()) {
        out.service_tier = Some(t);
    }
    if let Some(u) = body.usage {
        let cached = u.input_tokens_details.and_then(|d| d.cached_tokens).unwrap_or(0);
        out.usage.cache_read_tokens = cached;
        out.usage.input_tokens = u.input_tokens.saturating_sub(cached);
        out.usage.output_tokens = u.output_tokens;
        out.usage.reasoning_tokens = u.output_tokens_details.and_then(|d| d.reasoning_tokens).unwrap_or(0);
        out.has_usage = true;
    }
    // Counted once: from the whole answer, or from the stream's last event, which holds it.
    let calls = body.output.iter().filter(|i| RESPONSES_SERVER_TOOLS.contains(&i.kind.as_str())).count();
    if calls > 0 {
        out.usage.server_tool_calls = u32::try_from(calls).unwrap_or(u32::MAX);
    }
}

// --- Anthropic -------------------------------------------------------------------------------

#[derive(Deserialize)]
struct AnBody {
    /// `message_start` wraps the message.
    #[serde(default)]
    message: Option<Box<AnBody>>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
    /// `message_delta` carries the stop reason here.
    #[serde(default)]
    delta: Option<AnDelta>,
    #[serde(default)]
    usage: Option<AnUsage>,
}

#[derive(Deserialize)]
struct AnDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct AnUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation: Option<AnCacheCreation>,
    #[serde(default)]
    server_tool_use: Option<AnServerTools>,
    #[serde(default)]
    service_tier: Option<String>,
}

#[derive(Deserialize)]
struct AnCacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: Option<u64>,
    #[serde(default)]
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct AnServerTools {
    #[serde(default)]
    web_search_requests: Option<u32>,
    #[serde(default)]
    web_fetch_requests: Option<u32>,
}

fn anthropic(_event: &str, data: &[u8], out: &mut Extracted) {
    let Ok(mut body) = serde_json::from_slice::<AnBody>(data) else { return };
    let mut apply = |b: AnBody| {
        if let Some(m) = b.model.filter(|m| !m.is_empty()) {
            out.model = Some(m);
        }
        if let Some(r) = b.stop_reason.or(b.delta.and_then(|d| d.stop_reason)) {
            out.finish_reason = Some(r);
        }
        if let Some(u) = b.usage {
            // The input side arrives in `message_start`, the output side in `message_delta`,
            // which may repeat the input side. A field that is present replaces the value.
            if let Some(v) = u.input_tokens {
                out.usage.input_tokens = v;
            }
            if let Some(v) = u.output_tokens {
                out.usage.output_tokens = v;
            }
            if let Some(v) = u.cache_read_input_tokens {
                out.usage.cache_read_tokens = v;
            }
            if let Some(v) = u.cache_creation_input_tokens {
                out.usage.cache_write_tokens = v;
            }
            if let Some(c) = u.cache_creation {
                out.usage.cache_write_5m_tokens = c.ephemeral_5m_input_tokens.unwrap_or(0);
                out.usage.cache_write_1h_tokens = c.ephemeral_1h_input_tokens.unwrap_or(0);
            }
            if let Some(s) = u.server_tool_use {
                out.usage.server_tool_calls = s.web_search_requests.unwrap_or(0) + s.web_fetch_requests.unwrap_or(0);
            }
            if let Some(t) = u.service_tier.filter(|t| !t.is_empty()) {
                out.service_tier = Some(t);
            }
            out.has_usage = true;
        }
    };
    if let Some(inner) = body.message.take() {
        apply(*inner);
    }
    apply(body);
}

// --- Gemini native ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeBody {
    #[serde(default)]
    model_version: Option<String>,
    #[serde(default)]
    usage_metadata: Option<GeUsage>,
    #[serde(default)]
    candidates: Vec<GeCandidate>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeCandidate {
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeUsage {
    #[serde(default)]
    prompt_token_count: u64,
    #[serde(default)]
    candidates_token_count: u64,
    #[serde(default)]
    cached_content_token_count: u64,
    #[serde(default)]
    thoughts_token_count: u64,
    /// The prompt of tool use (Google Search results, for example), billed as input.
    #[serde(default)]
    tool_use_prompt_token_count: u64,
    #[serde(default)]
    service_tier: Option<String>,
    /// Vertex AI: how the call was served (`ON_DEMAND`, `PROVISIONED_THROUGHPUT`, …).
    #[serde(default)]
    traffic_type: Option<String>,
}

fn gemini(data: &[u8], out: &mut Extracted) {
    // `streamGenerateContent` without `alt=sse` answers with one JSON array of responses.
    if data.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[') {
        let Ok(items) = serde_json::from_slice::<Vec<Box<serde_json::value::RawValue>>>(data) else { return };
        for item in items {
            gemini(item.get().as_bytes(), out);
        }
        return;
    }
    let Ok(body) = serde_json::from_slice::<GeBody>(data) else { return };
    if let Some(e) = body.error.filter(|e| !e.is_null()) {
        let message = e.get("message").and_then(Value::as_str).map_or_else(|| e.to_string(), str::to_owned);
        out.stream_error = Some(message.chars().take(300).collect());
    }
    if let Some(m) = body.model_version.filter(|m| !m.is_empty()) {
        out.model = Some(m);
    }
    if let Some(r) = body.candidates.into_iter().find_map(|c| c.finish_reason) {
        out.finish_reason = Some(r);
    }
    if let Some(u) = body.usage_metadata {
        out.usage.cache_read_tokens = u.cached_content_token_count;
        out.usage.input_tokens =
            (u.prompt_token_count + u.tool_use_prompt_token_count).saturating_sub(u.cached_content_token_count);
        // Thinking tokens are billed as output and reported next to the candidates' tokens.
        out.usage.output_tokens = u.candidates_token_count + u.thoughts_token_count;
        out.usage.reasoning_tokens = u.thoughts_token_count;
        out.has_usage = true;
        // Gemini's API reports a service tier, Vertex AI how the call was served.
        if let Some(t) = u.service_tier.filter(|t| !t.is_empty()).or(u.traffic_type.filter(|t| !t.is_empty())) {
            out.service_tier = Some(t.to_ascii_lowercase());
        }
    }
}

/// Vertex AI's `predict` counts each input's tokens; Gemini's embed answers report nothing.
fn gemini_embed(data: &[u8], out: &mut Extracted) {
    let Ok(v) = serde_json::from_slice::<Value>(data) else { return };
    let counted: Vec<u64> = v["predictions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.pointer("/embeddings/statistics/token_count").and_then(Value::as_f64))
        .map(|n| n as u64)
        .collect();
    let reported = v.pointer("/usageMetadata/promptTokenCount").and_then(Value::as_u64);
    if let Some(tokens) = reported.or((!counted.is_empty()).then(|| counted.iter().sum())) {
        out.usage.input_tokens = tokens;
        out.has_usage = true;
    }
}

/// The characters an embeddings request asks to embed, in any of the shapes the gateway knows:
/// OpenAI's `input`, Gemini's `content` and `requests`, Vertex AI's `instances`. For an
/// estimate of its input when the provider reports no usage.
pub fn embedding_input_chars(body: &[u8]) -> Option<usize> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let mut n = 0;
    let mut texts = |c: &Value| {
        for part in c["parts"].as_array().into_iter().flatten() {
            n += part["text"].as_str().map_or(0, |t| t.chars().count());
        }
    };
    match v.get("input") {
        Some(Value::String(s)) => return Some(s.chars().count()),
        Some(Value::Array(items)) => {
            return Some(items.iter().filter_map(Value::as_str).map(|s| s.chars().count()).sum());
        }
        _ => {}
    }
    texts(&v["content"]);
    for r in v["requests"].as_array().into_iter().flatten() {
        texts(&r["content"]);
    }
    let mut count = n;
    for i in v["instances"].as_array().into_iter().flatten() {
        count += i["content"].as_str().map_or(0, |t| t.chars().count());
    }
    (count > 0).then_some(count)
}

// --- Ollama native ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct OlBody {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    /// Part of `prompt_eval_count`, read from the cache.
    #[serde(default)]
    prompt_eval_cached_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
    /// A failure, also in the middle of a stream that answered 200.
    #[serde(default)]
    error: Option<Value>,
}

fn ollama(data: &[u8], out: &mut Extracted) {
    let Ok(body) = serde_json::from_slice::<OlBody>(data) else { return };
    if let Some(e) = body.error.filter(|e| !e.is_null()) {
        let message = e.as_str().map_or_else(|| e.to_string(), str::to_owned);
        out.stream_error = Some(message.chars().take(300).collect());
    }
    if let Some(m) = body.model.filter(|m| !m.is_empty()) {
        out.model = Some(m);
    }
    if body.done {
        out.finish_reason = Some(body.done_reason.unwrap_or_else(|| "stop".into()));
        if body.prompt_eval_count.is_some() || body.eval_count.is_some() {
            let cached = body.prompt_eval_cached_count.unwrap_or(0);
            out.usage.input_tokens = body.prompt_eval_count.unwrap_or(0).saturating_sub(cached);
            out.usage.cache_read_tokens = cached;
            out.usage.output_tokens = body.eval_count.unwrap_or(0);
            out.has_usage = true;
        }
    }
}

/// A rough token estimate from byte counts, used when the provider reports no usage and the
/// key's policy asks for an estimate (about four bytes per token).
pub fn estimate(request_bytes: u64, response_bytes: u64) -> Usage {
    Usage { input_tokens: request_bytes.div_ceil(4), output_tokens: response_bytes.div_ceil(4), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dialect: Dialect, content_type: &str, chunks: &[&[u8]]) -> Extracted {
        let mut tap = UsageTap::new(dialect, content_type);
        for c in chunks {
            tap.feed(c);
        }
        tap.finish()
    }

    #[test]
    fn dialects_by_path() {
        use Dialect::*;
        assert_eq!(dialect_for(Family::Openai, "/v1/chat/completions"), Some(OpenAiChat));
        assert_eq!(dialect_for(Family::Openai, "/api/paas/v4/chat/completions?x=1"), Some(OpenAiChat));
        assert_eq!(dialect_for(Family::Openai, "/v1/responses"), Some(OpenAiResponses));
        assert_eq!(dialect_for(Family::Openai, "/v1/embeddings"), Some(OpenAiEmbeddings));
        assert_eq!(dialect_for(Family::Openai, "/v1/files"), None);
        assert_eq!(dialect_for(Family::Anthropic, "/v1/messages"), Some(Anthropic));
        assert_eq!(dialect_for(Family::Anthropic, "/v1/messages/count_tokens"), None);
        assert_eq!(dialect_for(Family::Anthropic, "/v1/messages/batches"), None);
        assert_eq!(dialect_for(Family::Gemini, "/v1beta/models/gemini-x:streamGenerateContent?alt=sse"), Some(Gemini));
        assert_eq!(dialect_for(Family::Gemini, "/v1beta/openai/chat/completions"), Some(OpenAiChat));
        assert_eq!(dialect_for(Family::Ollama, "/api/chat"), Some(Ollama));
        assert_eq!(dialect_for(Family::Ollama, "/v1/chat/completions"), Some(OpenAiChat));
    }

    #[test]
    fn openai_non_streaming_separates_cached_from_input() {
        let body = br#"{"id":"c","model":"gpt-x-2026","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
          "usage":{"prompt_tokens":1000,"completion_tokens":50,"total_tokens":1050,
                   "prompt_tokens_details":{"cached_tokens":600},"completion_tokens_details":{"reasoning_tokens":20}}}"#;
        let e = run(Dialect::OpenAiChat, "application/json", &[&body[..40], &body[40..]]);
        assert!(e.has_usage);
        assert_eq!(e.usage.input_tokens, 400);
        assert_eq!(e.usage.cache_read_tokens, 600);
        assert_eq!(e.usage.output_tokens, 50);
        assert_eq!(e.usage.reasoning_tokens, 20);
        assert_eq!(e.model.as_deref(), Some("gpt-x-2026"));
        assert_eq!(e.finish_reason.as_deref(), Some("stop"));
    }

    const OA_STREAM: &[u8] = b"data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}],\"usage\":null}\n\n\
data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":null}\n\n\
data: {\"id\":\"c\",\"model\":\"m\",\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}\n\n\
data: [DONE]\n\n";

    #[test]
    fn openai_stream_with_usage_chunk_at_any_split() {
        for i in 0..OA_STREAM.len() {
            let e = run(Dialect::OpenAiChat, "text/event-stream; charset=utf-8", &[&OA_STREAM[..i], &OA_STREAM[i..]]);
            assert!(e.has_usage, "split {i}");
            assert_eq!((e.usage.input_tokens, e.usage.output_tokens), (9, 3), "split {i}");
            assert_eq!(e.finish_reason.as_deref(), Some("length"));
            assert!(!e.cut);
        }
    }

    #[test]
    fn openai_stream_without_usage_reports_none() {
        let s = b"data: {\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let e = run(Dialect::OpenAiChat, "text/event-stream", &[s]);
        assert!(!e.has_usage);
        assert_eq!(e.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn kimi_usage_in_the_choice_and_its_cache_writes() {
        let finish = br#"data: {"model":"kimi-k3","choices":[{"index":0,"delta":{},"finish_reason":"stop","usage":{"prompt_tokens":30,"completion_tokens":4,"total_tokens":34}}]}

"#;
        let e = run(Dialect::OpenAiChat, "text/event-stream", &[finish, b"data: [DONE]\n\n"]);
        assert!(e.has_usage);
        assert_eq!((e.usage.input_tokens, e.usage.output_tokens), (30, 4));
        let full = br#"data: {"model":"kimi-k3","choices":[],"usage":{"prompt_tokens":30,"completion_tokens":4,"cached_tokens":8,"prompt_tokens_details":{"cached_tokens":8,"cache_write_tokens":12}}}

"#;
        let e = run(Dialect::OpenAiChat, "text/event-stream", &[finish, full]);
        assert_eq!((e.usage.input_tokens, e.usage.cache_read_tokens, e.usage.cache_write_tokens), (10, 8, 12));
    }

    #[test]
    fn a_failure_inside_a_stream_is_reported() {
        let glm = br#"data: {"model":"glm-4.7","choices":[{"index":0,"delta":{},"finish_reason":"network_error"}]}

"#;
        let e = run(Dialect::OpenAiChat, "text/event-stream", &[glm]);
        assert!(e.stream_error.as_deref().is_some_and(|m| m.contains("network_error")), "{e:?}");
        let error = b"data: {\"error\":{\"message\":\"overloaded\",\"type\":\"engine_overloaded_error\"}}\n\n";
        let e = run(Dialect::OpenAiChat, "text/event-stream", &[error]);
        assert_eq!(e.stream_error.as_deref(), Some("overloaded"));
        let fine =
            br#"{"model":"m","choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
        assert_eq!(run(Dialect::OpenAiChat, "application/json", &[fine]).stream_error, None);
    }

    #[test]
    fn gemini_array_stream_tool_prompt_and_tier() {
        let a = br#"[{"candidates":[{"content":{"parts":[{"text":"po"}]}}],"usageMetadata":{"promptTokenCount":10,"totalTokenCount":10},"modelVersion":"gemini-3.8-flash"}"#;
        let b = br#"
,
{"candidates":[{"content":{"parts":[{"text":"ng"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2,"thoughtsTokenCount":5,"cachedContentTokenCount":4,"toolUsePromptTokenCount":3,"serviceTier":"FLEX"},"modelVersion":"gemini-3.8-flash"}]"#;
        // At every split point of the array.
        let whole = [&a[..], &b[..]].concat();
        for cut in 0..whole.len() {
            let e = run(Dialect::Gemini, "application/json; charset=UTF-8", &[&whole[..cut], &whole[cut..]]);
            assert!(e.has_usage, "cut at {cut}");
            assert_eq!((e.usage.input_tokens, e.usage.cache_read_tokens), (9, 4));
            assert_eq!((e.usage.output_tokens, e.usage.reasoning_tokens), (7, 5));
            assert_eq!((e.finish_reason.as_deref(), e.service_tier.as_deref()), (Some("STOP"), Some("flex")));
        }
    }

    #[test]
    fn ollama_cached_prompt_and_errors_in_the_stream() {
        let done = br#"{"model":"qwen3","done":true,"done_reason":"stop","prompt_eval_count":11,"prompt_eval_cached_count":8,"eval_count":3}
"#;
        let e = run(Dialect::Ollama, "application/x-ndjson", &[done]);
        assert_eq!((e.usage.input_tokens, e.usage.cache_read_tokens, e.usage.output_tokens), (3, 8, 3));
        let failed = b"{\"model\":\"qwen3\",\"message\":{\"content\":\"a\"},\"done\":false}\n{\"error\":\"model runner crashed\"}\n";
        let e = run(Dialect::Ollama, "application/x-ndjson", &[failed]);
        assert_eq!(e.stream_error.as_deref(), Some("model runner crashed"));
    }

    #[test]
    fn recorded_streams_are_counted() {
        let fixture = |provider: &str, case: &str| -> serde_json::Value {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../pacod-gw-testkit/fixtures/{provider}/{case}.json"));
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
        };
        // Gemini's stream without `alt=sse`: one JSON array, in two pieces.
        let fx = fixture("gemini", "stream-array");
        let array = serde_json::to_vec(&fx["response"]["body"]).unwrap();
        let (a, b) = array.split_at(array.len() / 2);
        let e = run(Dialect::Gemini, "application/json; charset=UTF-8", &[a, b]);
        let last = fx["response"]["body"].as_array().unwrap().last().unwrap()["usageMetadata"].clone();
        assert!(e.has_usage);
        assert_eq!(e.usage.input_tokens, last["promptTokenCount"].as_u64().unwrap());
        assert_eq!(e.service_tier.as_deref(), Some("standard"));
        // Gemini's compatible endpoint: thinking only in the total.
        let fx = fixture("gemini-openai", "chat-stream");
        let text = fx["response"]["text"].as_str().unwrap().as_bytes().to_vec();
        let fields = UsageFields { reasoning_outside_completion: true, ..Default::default() };
        let mut tap = UsageTap::new(Dialect::OpenAiChat, "text/event-stream").with_fields(Arc::new(fields));
        tap.feed(&text);
        let e = tap.finish();
        assert!(e.usage.reasoning_tokens > 0 && e.usage.output_tokens > e.usage.reasoning_tokens, "{:?}", e.usage);
        // DeepSeek and GLM report usage on the last chunk, unasked.
        for provider in ["deepseek", "glm"] {
            let fx = fixture(provider, "chat-stream-plain");
            let e =
                run(Dialect::OpenAiChat, "text/event-stream", &[fx["response"]["text"].as_str().unwrap().as_bytes()]);
            assert!(e.has_usage && e.usage.reasoning_tokens > 0, "{provider}: {:?}", e.usage);
        }
    }

    #[test]
    fn deepseek_cache_hits() {
        let body = br#"{"model":"deepseek-x","choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":5,"prompt_cache_hit_tokens":64,"prompt_cache_miss_tokens":36}}"#;
        let e = run(Dialect::OpenAiChat, "application/json", &[body]);
        assert_eq!((e.usage.input_tokens, e.usage.cache_read_tokens), (36, 64));
    }

    #[test]
    fn usage_is_read_where_the_provider_reports_it_and_the_tier_is_kept() {
        let body = br#"{"model":"m","service_tier":"flex","usage":{"prompt_tokens":10,"completion_tokens":3,"cache":{"hits":4},"thinking":2}}"#;
        let fields =
            UsageFields { cached: vec!["cache.hits".into()], reasoning: vec!["thinking".into()], ..Default::default() };
        let e = parse_whole(Dialect::OpenAiChat, body, &fields);
        assert_eq!((e.usage.input_tokens, e.usage.cache_read_tokens, e.usage.reasoning_tokens), (6, 4, 2));
        assert_eq!(e.service_tier.as_deref(), Some("flex"));
        // With the default places nothing is found there.
        assert_eq!(parse_whole(Dialect::OpenAiChat, body, &UsageFields::default()).usage.cache_read_tokens, 0);

        let an = br#"{"type":"message","model":"claude-x","usage":{"input_tokens":5,"output_tokens":1,"service_tier":"batch"}}"#;
        assert_eq!(parse_whole(Dialect::Anthropic, an, &UsageFields::default()).service_tier.as_deref(), Some("batch"));
    }

    #[test]
    fn anthropic_stream_merges_start_and_delta() {
        let s = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-x\",\"usage\":{\"input_tokens\":812,\"cache_read_input_tokens\":6100,\"cache_creation_input_tokens\":300,\"cache_creation\":{\"ephemeral_5m_input_tokens\":100,\"ephemeral_1h_input_tokens\":200},\"output_tokens\":1}}}\n\n\
event: ping\ndata: {\"type\":\"ping\"}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":420,\"server_tool_use\":{\"web_search_requests\":2}}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let e = run(Dialect::Anthropic, "text/event-stream", &[s]);
        assert!(e.has_usage);
        assert_eq!(e.usage.input_tokens, 812);
        assert_eq!(e.usage.cache_read_tokens, 6100);
        assert_eq!(e.usage.cache_write_tokens, 300);
        assert_eq!((e.usage.cache_write_5m_tokens, e.usage.cache_write_1h_tokens), (100, 200));
        assert_eq!(e.usage.output_tokens, 420);
        assert_eq!(e.usage.server_tool_calls, 2);
        assert_eq!(e.model.as_deref(), Some("claude-x"));
        assert_eq!(e.finish_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn anthropic_non_streaming() {
        let body = br#"{"id":"msg_1","type":"message","model":"claude-x","stop_reason":"max_tokens","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":10,"output_tokens":20}}"#;
        let e = run(Dialect::Anthropic, "application/json", &[body]);
        assert_eq!((e.usage.input_tokens, e.usage.output_tokens), (10, 20));
        assert_eq!(e.finish_reason.as_deref(), Some("max_tokens"));
    }

    #[test]
    fn anthropic_error_body_has_no_usage() {
        let body = br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let e = run(Dialect::Anthropic, "application/json", &[body]);
        assert!(!e.has_usage);
        assert_eq!(e.model, None);
    }

    #[test]
    fn a_cut_stream_is_reported() {
        let s = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{\"input_tokens\":5}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_bl";
        let e = run(Dialect::Anthropic, "text/event-stream", &[s]);
        assert!(e.cut);
        assert_eq!(e.usage.input_tokens, 5, "what arrived before the cut still counts");
    }

    #[test]
    fn gemini_usage_metadata() {
        let s = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"a\"}]}}],\"modelVersion\":\"gemini-x\"}\n\n\
data: {\"candidates\":[{\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":100,\"candidatesTokenCount\":20,\"cachedContentTokenCount\":40,\"thoughtsTokenCount\":7,\"totalTokenCount\":127}}\n\n";
        let e = run(Dialect::Gemini, "text/event-stream", &[s]);
        assert_eq!((e.usage.input_tokens, e.usage.cache_read_tokens), (60, 40));
        assert_eq!((e.usage.output_tokens, e.usage.reasoning_tokens), (27, 7));
        assert_eq!(e.finish_reason.as_deref(), Some("STOP"));
        assert_eq!(e.model.as_deref(), Some("gemini-x"));
    }

    #[test]
    fn ollama_ndjson() {
        let s = b"{\"model\":\"llama\",\"message\":{\"content\":\"a\"},\"done\":false}\n{\"model\":\"llama\",\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":26,\"eval_count\":298}\n";
        let e = run(Dialect::Ollama, "application/x-ndjson", &[&s[..30], &s[30..]]);
        assert!(e.has_usage);
        assert_eq!((e.usage.input_tokens, e.usage.output_tokens), (26, 298));
    }

    #[test]
    fn responses_api_stream_and_object() {
        let s = b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"model\":\"gpt-x\",\"status\":\"in_progress\"}}\n\n\
event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"model\":\"gpt-x\",\"status\":\"completed\",\"usage\":{\"input_tokens\":30,\"output_tokens\":12,\"input_tokens_details\":{\"cached_tokens\":10},\"output_tokens_details\":{\"reasoning_tokens\":4}}}}\n\n";
        let e = run(Dialect::OpenAiResponses, "text/event-stream", &[s]);
        assert_eq!(
            (e.usage.input_tokens, e.usage.cache_read_tokens, e.usage.output_tokens, e.usage.reasoning_tokens),
            (20, 10, 12, 4)
        );
        assert_eq!(e.finish_reason.as_deref(), Some("completed"));
        let obj = br#"{"id":"r","model":"gpt-x","status":"completed","usage":{"input_tokens":3,"output_tokens":2}}"#;
        let e = run(Dialect::OpenAiResponses, "application/json", &[obj]);
        assert_eq!((e.usage.input_tokens, e.usage.output_tokens), (3, 2));
    }

    #[test]
    fn responses_api_counts_the_tools_the_provider_ran() {
        let obj: &[u8] = br#"{"model":"gpt-x","status":"completed","output":[{"type":"web_search_call","id":"ws_1"},{"type":"web_search_call","id":"ws_2"},{"type":"message","content":[]}],"usage":{"input_tokens":10,"output_tokens":3}}"#;
        let e = run(Dialect::OpenAiResponses, "application/json", &[obj]);
        assert_eq!((e.usage.server_tool_calls, e.usage.input_tokens), (2, 10));
        let s: &[u8] = b"event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"web_search_call\"}}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"web_search_call\"}],\"usage\":{\"input_tokens\":4,\"output_tokens\":1}}}\n\n";
        let e = run(Dialect::OpenAiResponses, "text/event-stream", &[s]);
        assert_eq!(e.usage.server_tool_calls, 1, "counted once, from the closing event");
    }

    #[test]
    fn malformed_and_oversized_bodies_are_ignored() {
        let e = run(Dialect::OpenAiChat, "application/json", &[b"<html>502</html>"]);
        assert_eq!(e, Extracted::default());
        let big = vec![b'x'; WHOLE_BODY_CAP + 1];
        let e = run(Dialect::OpenAiChat, "application/json", &[&big, b"{\"usage\":{\"prompt_tokens\":1}}"]);
        assert!(!e.has_usage);
    }

    #[test]
    fn estimate_from_bytes() {
        let u = estimate(401, 0);
        assert_eq!((u.input_tokens, u.output_tokens), (101, 0));
    }

    #[test]
    fn vertex_reports_how_a_call_was_served() {
        let body = br#"{"candidates":[],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":1,"trafficType":"ON_DEMAND_PRIORITY"}}"#;
        let mut out = Extracted::default();
        gemini(body, &mut out);
        assert_eq!(out.service_tier.as_deref(), Some("on_demand_priority"));
        assert_eq!(crate::relay::recorder::price_variant_for("on_demand_priority"), "priority");
    }

    #[test]
    fn embeddings_usage_is_found_also_after_the_cap() {
        let mut tap = UsageTap::new(Dialect::OpenAiEmbeddings, "application/json");
        let vector = format!("[{}]", vec!["0.0123456789"; 100_000].join(","));
        tap.feed(br#"{"object":"list","data":["#);
        for i in 0..10 {
            tap.feed(format!(r#"{{"object":"embedding","index":{i},"embedding":{vector}}},"#).as_bytes());
        }
        tap.feed(br#"{"object":"embedding","index":10,"embedding":[1.0]}],"model":"m","usage":{"prompt_tokens":42,"total_tokens":42}}"#);
        let found = tap.finish();
        assert!(found.has_usage, "read from the tail of a {} MB answer", 10 * 1_300_000 / 1_000_000);
        assert_eq!((found.usage.input_tokens, found.usage.output_tokens), (42, 0));
    }

    #[test]
    fn system_one_answers_report_input_tokens() {
        assert_eq!(dialect_for(Family::Typesafe, "/v1/systemone"), Some(Dialect::SystemOne));
        assert_eq!(dialect_for(Family::Typesafe, "/v1/models"), None);
        let found = parse_whole(
            Dialect::SystemOne,
            br#"{"model":"jev-1.13.0","answers":{},"usage":{"input_tokens":812,"output_tokens":10}}"#,
            &UsageFields::default(),
        );
        assert!(found.has_usage);
        assert_eq!((found.usage.input_tokens, found.usage.output_tokens), (812, 10));
        assert_eq!(found.model.as_deref(), Some("jev-1.13.0"));
        let without = parse_whole(Dialect::SystemOne, br#"{"answers":{}}"#, &UsageFields::default());
        assert!(!without.has_usage);
    }

    #[test]
    fn gemini_embed_paths_and_their_input() {
        assert_eq!(dialect_for(Family::Gemini, "/v1beta/models/e:batchEmbedContents"), Some(Dialect::GeminiEmbed));
        assert_eq!(
            dialect_for(Family::Gemini, "/v1/projects/p/locations/l/publishers/google/models/e:predict"),
            Some(Dialect::GeminiEmbed)
        );
        let mut out = Extracted::default();
        gemini_embed(br#"{"predictions":[{"embeddings":{"values":[1],"statistics":{"token_count":4.0}}},{"embeddings":{"values":[1],"statistics":{"token_count":3}}}]}"#, &mut out);
        assert_eq!((out.has_usage, out.usage.input_tokens), (true, 7));
        let mut none = Extracted::default();
        gemini_embed(br#"{"embeddings":[{"values":[1]}]}"#, &mut none);
        assert!(!none.has_usage, "Gemini's API reports no usage for embeddings");
        assert_eq!(embedding_input_chars(br#"{"input":["abcd","ef"]}"#), Some(6));
        assert_eq!(embedding_input_chars(br#"{"requests":[{"content":{"parts":[{"text":"abc"}]}}]}"#), Some(3));
        assert_eq!(embedding_input_chars(br#"{"instances":[{"content":"abcde"}]}"#), Some(5));
    }
}
