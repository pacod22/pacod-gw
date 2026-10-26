//! The provider-neutral model of a chat request, a response and a stream (concept sections
//! 8.4, 8.5 and 9.2).
//!
//! Every dialect has two halves. The client half reads a caller's request into these types
//! and writes responses, stream events and errors in the caller's shape. The provider half
//! writes a provider request from these types and reads the provider's response and stream
//! back into them. With both halves for both families, every interface reaches every provider.
//!
//! Three rules shape the types (concept section 8.3):
//!
//! - **Deterministic.** Maps that end up in output keep insertion order (`serde_json` is
//!   built with `preserve_order`), and nothing here depends on time or randomness.
//! - **Unknown is carried, not dropped.** What a dialect cannot express in neutral terms
//!   travels as an opaque [`Block::Provider`] or in `extra`, and is given back to the same
//!   provider unchanged.
//! - **Reasoning crosses turns.** A [`Block::Reasoning`] keeps the provider's signature, so
//!   that it can be replayed to the provider that produced it.

pub use pacod_gw_events::Usage;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// --- request -----------------------------------------------------------------------------------

/// A chat request in neutral terms.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Request {
    /// The value of the caller's `model` field, as sent. Resolution happens outside.
    pub model: String,
    /// The system prompt, as text blocks. Empty when there is none.
    pub system: Vec<Block>,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub tool_choice: Option<ToolChoice>,
    pub response_format: Option<ResponseFormat>,
    /// Stop sequences.
    pub stop: Vec<String>,
    pub stream: bool,
    /// The caller asked for the usage chunk on an OpenAI-dialect stream.
    pub stream_usage: bool,
    pub params: Params,
    /// An end-user identifier the dialect carries itself (OpenAI `user`, Anthropic
    /// `metadata.user_id`). Used when the gateway parameters name none.
    pub user: Option<String>,
    /// The raw `gateway` object of the body, when present. Parsed by the core.
    pub gateway: Option<Value>,
    /// Vendor-specific parameters by adapter family (`provider_options` on the native
    /// interface). Applied only when that family serves the call.
    pub provider_options: Map<String, Value>,
    /// Top-level fields of the caller's request that have no neutral meaning. Forwarded when
    /// the provider speaks the caller's dialect, dropped or rejected otherwise.
    pub extra: Map<String, Value>,
}

/// The parameters the gateway governs (concept section 7.2).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Params {
    pub max_output_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub reasoning: Reasoning,
}

/// How much the model should reason, in one vocabulary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reasoning {
    /// `none`, `minimal`, `low`, `medium`, `high`, `xhigh` or `max`. `none` switches reasoning off.
    pub effort: Option<String>,
    /// For models that take a token budget.
    pub budget_tokens: Option<u64>,
    /// Return the reasoning text (a summary, where the provider only gives that).
    pub summary: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    /// A message that carries only tool results. Anthropic has no such role: results sit in a
    /// user message there, and consecutive tool messages become one user message.
    Tool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Block>,
}

/// A prompt-cache marker on a block: everything up to and including it may be cached.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cache {
    /// `5m` or `1h`. `None` leaves the lifetime to the provider's default.
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

/// One piece of content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
        /// The provider's citations for this text, as the provider gave them.
        citations: Option<Value>,
        cache: Option<Cache>,
    },
    Image {
        source: ImageSource,
        cache: Option<Cache>,
    },
    /// A document (PDF, plain text, a file reference). `source` and the other members are the
    /// provider's own, kept opaque.
    Document {
        /// The block without its `type` and its cache marker.
        body: Map<String, Value>,
        cache: Option<Cache>,
    },
    /// The model asks for a tool to be run by the client.
    ToolCall {
        id: String,
        name: String,
        /// The arguments as JSON text, exactly as the model produced them. Kept as text so
        /// that a replay is byte-identical; an empty string means `{}`.
        arguments: String,
        cache: Option<Cache>,
    },
    /// The client's answer to a tool call.
    ToolResult {
        tool_call_id: String,
        /// Text and image blocks.
        content: Vec<Block>,
        is_error: bool,
        cache: Option<Cache>,
    },
    /// The model's reasoning. Replayable only to the provider that produced it.
    Reasoning {
        text: String,
        /// The provider's signature over the block. Without it the block cannot be replayed.
        signature: Option<String>,
        /// Reasoning the provider returned encrypted instead of as text.
        redacted: Option<String>,
    },
    /// A block only the serving provider understands: a server-tool call or result, or a
    /// block type the gateway does not know. Returned to the client and accepted back
    /// unchanged; given to another provider family it is left out.
    Provider {
        /// `anthropic`, `openai`, …
        family: String,
        /// The block exactly as the provider wrote it.
        raw: Value,
    },
}

impl Block {
    pub fn text(text: impl Into<String>) -> Self {
        Block::Text { text: text.into(), citations: None, cache: None }
    }

    /// The cache marker of the block, where the block type can carry one.
    pub fn cache(&self) -> Option<&Cache> {
        match self {
            Block::Text { cache, .. }
            | Block::Image { cache, .. }
            | Block::Document { cache, .. }
            | Block::ToolCall { cache, .. }
            | Block::ToolResult { cache, .. } => cache.as_ref(),
            Block::Reasoning { .. } | Block::Provider { .. } => None,
        }
    }
}

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq)]
pub enum Tool {
    Function {
        name: String,
        description: Option<String>,
        /// The JSON schema of the arguments.
        parameters: Value,
        strict: Option<bool>,
        cache: Option<Cache>,
    },
    /// A tool the provider defines or runs itself (web search, code execution, the text
    /// editor). The definition is passed verbatim to that provider and to no other.
    Provider { family: String, definition: Value },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolChoice {
    Auto,
    /// The model must not call a tool.
    None,
    /// The model must call some tool.
    Required,
    /// The model must call this tool.
    Tool(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseFormat {
    Text,
    /// Any JSON object, without a schema.
    JsonObject,
    JsonSchema {
        name: Option<String>,
        schema: Value,
        strict: Option<bool>,
    },
}

// --- response ----------------------------------------------------------------------------------

/// Why the model stopped, in one vocabulary (concept section 8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Finish {
    Stop,
    Length,
    ToolCalls,
    /// The provider refused or filtered the answer. A successful response, not an error.
    ContentFilter,
    /// A long-running turn was paused; the client continues by sending the conversation again.
    Paused,
}

impl Finish {
    /// The name on the native interface.
    pub fn as_str(self) -> &'static str {
        match self {
            Finish::Stop => "stop",
            Finish::Length => "length",
            Finish::ToolCalls => "tool_calls",
            Finish::ContentFilter => "content_filter",
            Finish::Paused => "paused",
        }
    }
}

/// A complete answer. Serialisable, so that the response cache can keep it and write it again
/// in any caller's dialect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// The provider's id for the response.
    pub id: String,
    /// The model the provider says answered.
    pub model: String,
    pub content: Vec<Block>,
    pub finish: Finish,
    /// The provider's own word for it, for example `end_turn`.
    pub native_finish: String,
    /// `None` when the provider reported no usage.
    pub usage: Option<Usage>,
    /// Top-level members of the provider's response that have no neutral meaning, for
    /// example Anthropic's `stop_details` or `container`.
    pub extra: Map<String, Value>,
}

// --- stream ------------------------------------------------------------------------------------

/// What a content block of a stream is, announced when it opens.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockStart {
    Text,
    Reasoning,
    ToolCall {
        id: String,
        name: String,
    },
    /// A provider block. `raw` is the block as it opened; its deltas follow as
    /// [`Delta::Provider`].
    Provider {
        family: String,
        raw: Value,
    },
}

/// A piece of a content block.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Text(String),
    Reasoning(String),
    /// A fragment of a tool call's arguments, as JSON text.
    ToolArguments(String),
    /// The signature that closes a reasoning block.
    Signature(String),
    /// A citation for the text block, as the provider wrote it.
    Citation(Value),
    /// A delta of a provider block, as the provider wrote it.
    Provider(Value),
}

/// One event of a response stream. A stream is `MessageStart`, then blocks (each a
/// `ContentStart`, deltas and a `ContentStop`, never interleaved on one index), then exactly
/// one `MessageStop` or `Error`.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    MessageStart {
        id: String,
        model: String,
    },
    ContentStart {
        index: u32,
        block: BlockStart,
    },
    ContentDelta {
        index: u32,
        delta: Delta,
    },
    ContentStop {
        index: u32,
    },
    MessageStop {
        finish: Finish,
        native_finish: String,
        /// `None` when the provider reported no usage on the stream.
        usage: Option<Usage>,
    },
    Ping,
    /// The stream failed. Ends it.
    Error {
        kind: String,
        message: String,
    },
}

// --- what a translation needs to know about the target -------------------------------------------

/// How a model's reasoning is controlled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReasoningStyle {
    /// The model does not reason, or it cannot be controlled.
    #[default]
    None,
    /// A token budget switches reasoning on (older Claude models).
    Budget,
    /// The model decides; an effort level steers it (current Claude models).
    Adaptive,
    /// As adaptive, and reasoning cannot be switched off.
    AlwaysOn,
    /// An effort level only (OpenAI-style reasoning models).
    Effort,
}

/// How reasoning is switched off on a model that reasons by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThinkingOff {
    /// `thinking: {type: "disabled"}`
    #[default]
    Disabled,
    /// `thinking: {type: "between_tools"}`
    BetweenTools,
    /// It cannot be switched off: the lowest effort is used instead.
    Unsupported,
}

/// The request field that steers reasoning on an OpenAI-compatible endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReasoningParam {
    /// `reasoning_effort: "low" | …`
    #[default]
    ReasoningEffort,
    /// `thinking: {"type": "enabled" | "disabled"}`
    ThinkingToggle,
    /// No such field.
    None,
}

/// The prefix of a reasoning signature another provider than Anthropic made. Anthropic's
/// signatures go to callers as they are; the others carry their origin, `gw1.<family>.<raw>`,
/// so that each provider half gives back only its own. A provider's signature is base64 and
/// never contains a dot.
pub const SIGNATURE_PREFIX: &str = "gw1.";

/// A signature `family` made, as callers see it.
/// The origins of Google's thought signatures: Gemini's API and Vertex AI.
pub const GOOGLE_SIGNATURE_FAMILIES: [&str; 2] = ["gemini", "vertex"];

/// A Google thought signature as it travels in `extra_content.google.thought_signature` to an
/// OpenAI-dialect caller: Gemini's API's own value, as its compatible endpoint writes it, and
/// for another Google origin (Vertex AI) the whole labelled value. `None` when it is not
/// Google's.
pub fn google_wire_signature(signature: &str) -> Option<&str> {
    if let Some(raw) = signed_by(signature, "gemini") {
        return Some(raw);
    }
    GOOGLE_SIGNATURE_FAMILIES.iter().find(|f| signed_by(signature, f).is_some()).map(|_| signature)
}

/// A signature an OpenAI-dialect caller sent back in `extra_content.google.thought_signature`:
/// a labelled one as it is, a bare one as Gemini's API's.
pub fn from_google_wire(value: &str) -> String {
    if value.starts_with(SIGNATURE_PREFIX) { value.to_owned() } else { sign("gemini", value) }
}

pub fn sign(family: &str, raw: &str) -> String {
    format!("{SIGNATURE_PREFIX}{family}.{raw}")
}

/// The provider's own signature, if `signature` is one `family` made.
pub fn signed_by<'a>(signature: &'a str, family: &str) -> Option<&'a str> {
    signature.strip_prefix(SIGNATURE_PREFIX)?.strip_prefix(family)?.strip_prefix('.')
}

/// Google's documented value for a function call whose thought signature was lost: the call is
/// accepted, with poorer reasoning (ai.google.dev, thought signatures).
pub const SKIP_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

/// Whether an OpenAI-compatible endpoint wants the reasoning of earlier assistant turns back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EchoReasoning {
    /// Reasoning is never sent back.
    #[default]
    Never,
    /// When the request has tools, every earlier assistant turn carries its reasoning, empty
    /// where it has none (DeepSeek refuses such a request otherwise).
    WithTools,
    /// Assistant turns carry their reasoning where they have some (Kimi, GLM).
    Always,
}

/// What the provider half of a dialect needs to know about the model and the endpoint it
/// writes for. The core fills it from the model's capability profile and the provider's quirk
/// profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The provider family that serves the call: `openai` or `anthropic`.
    pub family: String,
    /// The provider's own id of the model. Written into the request.
    pub upstream_model: String,
    pub reasoning: ReasoningStyle,
    /// The effort levels the model accepts, lowest first.
    pub reasoning_efforts: Vec<String>,
    pub thinking_off: ThinkingOff,
    /// OpenAI dialect: the limit goes into `max_completion_tokens` instead of `max_tokens`.
    pub max_completion_tokens: bool,
    /// OpenAI dialect: how reasoning is steered on this endpoint.
    pub reasoning_param: ReasoningParam,
    /// OpenAI dialect: the model reasons in a request that offers function tools. Where it
    /// does not (GPT-5.6), such a request is sent with the effort `none`.
    pub reasoning_with_tools: bool,
    /// OpenAI dialect: the field of a message or delta that carries reasoning text.
    pub reasoning_field: String,
    /// The provider accepts prompt-cache markers on blocks. Otherwise they are left out.
    pub cache_markers: bool,
    /// Let the provider place the cache marker itself (Anthropic's top-level `cache_control`).
    pub cache_auto: bool,
    /// OpenAI dialect: streams report usage when asked with `stream_options.include_usage`.
    pub stream_usage: bool,
    /// The output-token limit to send when the request has none and the provider requires
    /// one (Anthropic does).
    pub default_max_output: u64,
    /// OpenAI dialect: the model takes documents (PDF and text) as file parts.
    pub documents: bool,
    /// OpenAI dialect: where `usage` reports cached input tokens, as dotted paths tried in order.
    pub cached_tokens_at: Vec<String>,
    /// OpenAI dialect: where `usage` reports reasoning tokens, as dotted paths tried in order.
    pub reasoning_tokens_at: Vec<String>,
    /// OpenAI dialect: where `usage` reports tokens written to the prompt cache (Kimi). They
    /// are part of `prompt_tokens` there.
    pub cache_write_tokens_at: Vec<String>,
    /// OpenAI dialect: the members an OpenAI-dialect caller may send that are passed on to
    /// the provider. Anything else outside the neutral model is left out and reported.
    pub extra_params: Vec<String>,
    /// OpenAI dialect: whether earlier reasoning goes back to the provider.
    pub echo_reasoning: EchoReasoning,
    /// The provider's name in the gateway. `provider_options` under this name are merged into
    /// the request, after those under the family's name.
    pub provider: String,
    /// OpenAI dialect: tool calls and messages carry Gemini's thought signatures in
    /// `extra_content.google.thought_signature`, both ways (Gemini's compatible endpoint).
    pub thought_signatures: bool,
    /// OpenAI dialect: reasoning tokens are left out of `completion_tokens` and only counted
    /// in `total_tokens` (Gemini's compatible endpoint).
    pub reasoning_outside_completion: bool,
    /// Gemini: the origin its thought signatures are labelled with, `gemini` (Gemini's API) or
    /// `vertex` (Vertex AI). Empty is `gemini`. A signature of another origin is not sent back.
    pub signature_family: String,
}

/// Where OpenAI-compatible providers report cached input tokens inside `usage`: OpenAI's own
/// place, and DeepSeek's.
pub const DEFAULT_CACHED_TOKENS_AT: &[&str] = &["prompt_tokens_details.cached_tokens", "prompt_cache_hit_tokens"];
/// Where OpenAI-compatible providers report reasoning tokens inside `usage`.
pub const DEFAULT_REASONING_TOKENS_AT: &[&str] = &["completion_tokens_details.reasoning_tokens"];
/// Where OpenAI-compatible providers report tokens written to the prompt cache inside `usage`:
/// Kimi's place. OpenAI and most others report none.
pub const DEFAULT_CACHE_WRITE_TOKENS_AT: &[&str] = &["prompt_tokens_details.cache_write_tokens"];
/// The provider-specific members an OpenAI-dialect caller may send by default.
pub const DEFAULT_EXTRA_PARAMS: &[&str] = &[
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

/// The owned form of one of the lists above.
pub fn owned(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            family: "openai".into(),
            upstream_model: String::new(),
            reasoning: ReasoningStyle::None,
            reasoning_efforts: Vec::new(),
            thinking_off: ThinkingOff::Disabled,
            max_completion_tokens: false,
            reasoning_param: ReasoningParam::ReasoningEffort,
            reasoning_with_tools: true,
            reasoning_field: "reasoning_content".into(),
            cache_markers: false,
            cache_auto: false,
            stream_usage: true,
            default_max_output: 4096,
            documents: false,
            cached_tokens_at: owned(DEFAULT_CACHED_TOKENS_AT),
            reasoning_tokens_at: owned(DEFAULT_REASONING_TOKENS_AT),
            cache_write_tokens_at: owned(DEFAULT_CACHE_WRITE_TOKENS_AT),
            extra_params: owned(DEFAULT_EXTRA_PARAMS),
            echo_reasoning: EchoReasoning::Never,
            provider: String::new(),
            thought_signatures: false,
            reasoning_outside_completion: false,
            signature_family: String::new(),
        }
    }
}

/// What a provider half left out or changed while writing a request, for the caller to be
/// told (`x-gw-dropped-params` and the native `gateway` block).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Notes {
    /// Names of parameters and features that were left out.
    pub dropped: Vec<String>,
    /// Sentences about values that were changed to fit.
    pub adjusted: Vec<String>,
}

/// What the gateway adds to a response it writes for a caller.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stamp {
    /// The id the caller sees. Derived by the core from the provider's id, so that the same
    /// provider answer always gives the same bytes.
    pub id: String,
    /// The model name the caller sees.
    pub model: String,
    /// Unix seconds, for dialects that carry a creation time.
    pub created: u64,
    /// The cost object to put next to the usage (concept section 9.3), when there is one.
    pub cost: Option<Value>,
    /// The native interface's `gateway` block, when the dialect has a place for it.
    pub gateway: Option<Value>,
    /// The template that served the call and its version, for the native stream's
    /// `message.start`.
    pub template: Option<(String, u32)>,
    /// The request members a Responses answer repeats (`instructions`, `tools`, …), as the
    /// caller sent them.
    pub echo: Option<Value>,
}
