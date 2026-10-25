//! The global model repository: models, capability profiles, life cycle, grants and prices
//! (concept sections 6 and 13.1).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Doc, Family, ReasoningParam};

// --- capability profile ----------------------------------------------------------------------

/// How a model treats a sampling parameter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ParamSupport {
    #[default]
    Accepted,
    /// The provider rejects the request when the parameter is present.
    Rejected,
    /// Accepted only at the model's default value.
    DefaultOnly,
}

/// How a model's reasoning is controlled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningStyle {
    /// The model does not reason, or it cannot be controlled.
    #[default]
    None,
    /// A token budget switches reasoning on.
    Budget,
    /// The model decides how much to reason; an effort level steers it.
    Adaptive,
    /// As adaptive, and reasoning cannot be switched off.
    AlwaysOn,
    /// An effort level only (OpenAI-style reasoning models).
    Effort,
}

/// How reasoning is switched off on a model that reasons by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingOff {
    /// `thinking: {type: "disabled"}`.
    #[default]
    Disabled,
    /// `thinking: {type: "between_tools"}`: the closest this model gets to off.
    BetweenTools,
    /// It cannot be switched off; the lowest effort is used instead.
    Unsupported,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheStyle {
    /// The caller marks cache breakpoints.
    Explicit,
    /// The provider caches prefixes on its own.
    Automatic,
    #[default]
    None,
}

/// Which request field carries the output-token limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    #[default]
    MaxTokens,
    MaxCompletionTokens,
}

/// What a model accepts. This is data, editable through the API, so that a new model's rules
/// need no gateway release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Capabilities {
    pub temperature: ParamSupport,
    pub top_p: ParamSupport,
    pub top_k: ParamSupport,
    /// Frequency and presence penalty.
    pub penalties: ParamSupport,
    pub reasoning: ReasoningStyle,
    /// The effort levels the model accepts, lowest first. Empty when it takes none.
    pub reasoning_efforts: Vec<String>,
    /// Budget-style models: the smallest budget the provider accepts.
    pub reasoning_min_budget: Option<u32>,
    pub thinking_off: ThinkingOff,
    pub prompt_cache: PromptCacheStyle,
    pub vision: bool,
    /// Documents (PDF and text) as input. Anthropic takes them always; an OpenAI-compatible
    /// model only where this is set.
    pub documents: bool,
    pub tools: bool,
    /// Provider-executed tools such as web search.
    pub server_tools: bool,
    pub structured_output: bool,
    /// Whether a specific tool, or any tool, can be forced.
    pub tool_choice_forced: bool,
    /// Whether `tool_choice: "none"` is accepted. GLM takes only `auto`.
    pub tool_choice_none: bool,
    /// Whether the conversation may end with a partial assistant message to continue.
    pub assistant_prefill: bool,
    pub max_tokens_field: MaxTokensField,
    /// The request field that steers this model's reasoning on an OpenAI-compatible endpoint,
    /// where it differs from its provider's: GLM-5.3 takes an effort, GLM-4.7 a switch. Unset
    /// uses the provider's quirk profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_param: Option<ReasoningParam>,
    /// Top-level request fields this model refuses where its provider's other models take them
    /// (xAI's reasoning models refuse `stop`). Removed and reported on the OpenAI-compatible
    /// wire, after the provider's own `drop_fields`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drop_fields: Vec<String>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            temperature: ParamSupport::Accepted,
            top_p: ParamSupport::Accepted,
            top_k: ParamSupport::Rejected,
            penalties: ParamSupport::Accepted,
            reasoning: ReasoningStyle::None,
            reasoning_efforts: Vec::new(),
            reasoning_min_budget: None,
            thinking_off: ThinkingOff::Disabled,
            prompt_cache: PromptCacheStyle::None,
            vision: false,
            documents: false,
            tools: true,
            server_tools: false,
            structured_output: false,
            tool_choice_forced: true,
            tool_choice_none: true,
            assistant_prefill: true,
            max_tokens_field: MaxTokensField::MaxTokens,
            reasoning_param: None,
            drop_fields: Vec::new(),
        }
    }
}

impl Capabilities {
    /// How this model's reasoning is steered: its own field, or its provider's.
    pub fn reasoning_param_or(&self, provider: ReasoningParam) -> ReasoningParam {
        self.reasoning_param.unwrap_or(provider)
    }
}

/// A named starting point for a model's capability profile.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CapabilityPreset {
    pub name: &'static str,
    pub description: &'static str,
    pub family: Family,
    pub capabilities: Capabilities,
}

fn efforts(levels: &[&str]) -> Vec<String> {
    levels.iter().map(|s| (*s).to_owned()).collect()
}

/// The built-in presets. A model's profile starts from one and can then be edited.
pub fn capability_presets() -> Vec<CapabilityPreset> {
    use ParamSupport::{Accepted, DefaultOnly, Rejected};
    let anthropic_base = Capabilities {
        top_k: Accepted,
        penalties: Rejected,
        prompt_cache: PromptCacheStyle::Explicit,
        vision: true,
        documents: true,
        server_tools: true,
        structured_output: true,
        ..Default::default()
    };
    let openai_base = Capabilities {
        prompt_cache: PromptCacheStyle::Automatic,
        vision: true,
        documents: true,
        structured_output: true,
        ..Default::default()
    };
    // OpenAI-compatible providers. Prompt caching is automatic at all of them.
    let deepseek = Capabilities {
        reasoning: ReasoningStyle::Effort,
        reasoning_efforts: efforts(&["none", "low", "high", "max"]),
        prompt_cache: PromptCacheStyle::Automatic,
        // `required` and named tool choices are refused in thinking mode, the default.
        tool_choice_forced: false,
        // Accepted on the standard endpoint, but the answer starts over instead of continuing
        // (recorded 2026-10); continuing is DeepSeek's beta prefix completion.
        assistant_prefill: false,
        ..Default::default()
    };
    let glm = Capabilities {
        penalties: Rejected,
        reasoning: ReasoningStyle::Effort,
        reasoning_efforts: efforts(&["none", "low", "medium", "high"]),
        prompt_cache: PromptCacheStyle::Automatic,
        // `required` works (recorded 2026-10), though the reference names only `auto`.
        tool_choice_forced: true,
        tool_choice_none: false,
        // Accepted, but the answer starts over instead of continuing (recorded): refused here.
        assistant_prefill: false,
        reasoning_param: Some(ReasoningParam::ThinkingToggle),
        ..Default::default()
    };
    let kimi = Capabilities {
        temperature: Rejected,
        top_p: Rejected,
        penalties: Rejected,
        reasoning: ReasoningStyle::AlwaysOn,
        reasoning_efforts: efforts(&["low", "high", "max"]),
        thinking_off: ThinkingOff::Unsupported,
        prompt_cache: PromptCacheStyle::Automatic,
        vision: true,
        structured_output: true,
        tool_choice_forced: false,
        assistant_prefill: false,
        max_tokens_field: MaxTokensField::MaxCompletionTokens,
        reasoning_param: Some(ReasoningParam::ReasoningEffort),
        ..Default::default()
    };
    // Gemini 3 through its compatible endpoint (ai.google.dev, OpenAI compatibility and
    // thinking): thinking cannot be switched off, a trailing model turn is refused, and from
    // 3.6 on the sampling parameters are ignored.
    let gemini = Capabilities {
        temperature: DefaultOnly,
        top_p: DefaultOnly,
        top_k: DefaultOnly,
        penalties: Rejected,
        reasoning: ReasoningStyle::AlwaysOn,
        reasoning_efforts: efforts(&["low", "medium", "high"]),
        thinking_off: ThinkingOff::Unsupported,
        prompt_cache: PromptCacheStyle::Automatic,
        vision: true,
        structured_output: true,
        // `required` works on both APIs (recorded 2026-10); the native one also names a tool.
        tool_choice_forced: true,
        assistant_prefill: false,
        reasoning_param: Some(ReasoningParam::ReasoningEffort),
        ..Default::default()
    };
    let ollama = Capabilities {
        top_k: Accepted,
        vision: true,
        structured_output: true,
        tool_choice_forced: false,
        ..Default::default()
    };
    vec![
        CapabilityPreset {
            name: "openai-chat",
            description: "OpenAI chat models without reasoning: every sampling parameter is accepted.",
            family: Family::Openai,
            capabilities: openai_base.clone(),
        },
        CapabilityPreset {
            name: "openai-reasoning",
            description: "OpenAI reasoning models: sampling parameters are rejected, reasoning is steered by an effort level, and the token limit is `max_completion_tokens`.",
            family: Family::Openai,
            capabilities: Capabilities {
                temperature: Rejected,
                top_p: Rejected,
                penalties: Rejected,
                reasoning: ReasoningStyle::Effort,
                reasoning_efforts: efforts(&["low", "medium", "high", "xhigh"]),
                max_tokens_field: MaxTokensField::MaxCompletionTokens,
                ..openai_base.clone()
            },
        },
        CapabilityPreset {
            name: "anthropic-adaptive",
            description: "Claude Opus 4.7 and later, Claude Sonnet 5, Claude Fable: sampling parameters are rejected, thinking is adaptive and steered by an effort level.",
            family: Family::Anthropic,
            capabilities: Capabilities {
                temperature: Rejected,
                top_p: Rejected,
                top_k: Rejected,
                reasoning: ReasoningStyle::Adaptive,
                reasoning_efforts: efforts(&["low", "medium", "high", "xhigh", "max"]),
                assistant_prefill: false,
                ..anthropic_base.clone()
            },
        },
        CapabilityPreset {
            name: "anthropic-always-thinking",
            description: "Claude Opus 5.5 and Claude Fable 5.1: as adaptive, and thinking cannot be switched off; forcing a tool is rejected.",
            family: Family::Anthropic,
            capabilities: Capabilities {
                temperature: Rejected,
                top_p: Rejected,
                top_k: Rejected,
                reasoning: ReasoningStyle::AlwaysOn,
                reasoning_efforts: efforts(&["low", "medium", "high", "xhigh", "max"]),
                thinking_off: ThinkingOff::Unsupported,
                tool_choice_forced: false,
                assistant_prefill: false,
                ..anthropic_base.clone()
            },
        },
        CapabilityPreset {
            name: "anthropic-sonnet-5-5",
            description: "Claude Sonnet 5.5: sampling parameters only at their default, thinking is switched off with `between_tools`, forcing a tool is rejected.",
            family: Family::Anthropic,
            capabilities: Capabilities {
                temperature: DefaultOnly,
                top_p: DefaultOnly,
                top_k: DefaultOnly,
                reasoning: ReasoningStyle::Adaptive,
                reasoning_efforts: efforts(&["low", "medium", "high", "xhigh", "max"]),
                thinking_off: ThinkingOff::BetweenTools,
                tool_choice_forced: false,
                assistant_prefill: false,
                ..anthropic_base.clone()
            },
        },
        CapabilityPreset {
            name: "anthropic-budget",
            description: "Claude Haiku 4.5 and earlier models: sampling parameters are accepted and thinking takes a token budget of at least 1024.",
            family: Family::Anthropic,
            capabilities: Capabilities {
                reasoning: ReasoningStyle::Budget,
                reasoning_min_budget: Some(1024),
                ..anthropic_base
            },
        },
        CapabilityPreset {
            name: "openai-compatible",
            description: "Models behind an OpenAI-compatible endpoint (GLM, DeepSeek, Kimi, Gemini's compatible endpoint, Ollama): sampling parameters accepted, an effort level of low, medium or high.",
            family: Family::Openai,
            capabilities: Capabilities {
                reasoning: ReasoningStyle::Effort,
                reasoning_efforts: efforts(&["low", "medium", "high"]),
                prompt_cache: PromptCacheStyle::Automatic,
                ..Default::default()
            },
        },
        CapabilityPreset {
            name: "openai-compatible-fixed-temperature",
            description: "Reasoning models behind an OpenAI-compatible endpoint that reject a temperature other than their default.",
            family: Family::Openai,
            capabilities: Capabilities {
                temperature: DefaultOnly,
                top_p: DefaultOnly,
                reasoning: ReasoningStyle::Effort,
                reasoning_efforts: efforts(&["low", "medium", "high"]),
                prompt_cache: PromptCacheStyle::Automatic,
                ..Default::default()
            },
        },
        // --- per provider (documentation as of 2026-10; the fixtures record what was sent) ---
        CapabilityPreset {
            name: "deepseek",
            description: "DeepSeek models (deepseek-v4-pro): thinking on by default and switched off with the effort none; no forced tool choice while thinking; JSON mode without a schema.",
            family: Family::Openai,
            capabilities: deepseek.clone(),
        },
        CapabilityPreset {
            name: "deepseek-vision",
            description: "DeepSeek models that take images (deepseek-flash).",
            family: Family::Openai,
            capabilities: Capabilities { vision: true, ..deepseek },
        },
        CapabilityPreset {
            name: "glm-5",
            description: "GLM-5.2 and later: reasoning steered by an effort (low, high, max), on by default; GLM-5.3 cannot switch it off. No tool choice none; JSON mode without a schema (GLM ignores one).",
            family: Family::Openai,
            capabilities: Capabilities {
                reasoning_efforts: efforts(&["low", "high", "max"]),
                thinking_off: ThinkingOff::Unsupported,
                reasoning_param: Some(ReasoningParam::ReasoningEffort),
                ..glm.clone()
            },
        },
        CapabilityPreset {
            name: "glm-5-vision",
            description: "GLM-5.3-Flash and other GLM-5 vision models.",
            family: Family::Openai,
            capabilities: Capabilities {
                reasoning_efforts: efforts(&["low", "high", "max"]),
                thinking_off: ThinkingOff::Unsupported,
                reasoning_param: Some(ReasoningParam::ReasoningEffort),
                vision: true,
                ..glm.clone()
            },
        },
        CapabilityPreset {
            name: "glm-4",
            description: "GLM-4.5 to GLM-4.7: reasoning switched on or off with `thinking`, no levels. No tool choice none; JSON mode without a schema (GLM ignores one).",
            family: Family::Openai,
            capabilities: glm,
        },
        CapabilityPreset {
            name: "kimi-k3",
            description: "Kimi K3: always reasons, steered by an effort (low, high, max); sampling parameters are fixed and left out; a named tool choice is refused while thinking.",
            family: Family::Openai,
            capabilities: kimi.clone(),
        },
        CapabilityPreset {
            name: "kimi-k2",
            description: "Kimi K2.6 and the K2.7 code models: reasoning switched with `thinking`; sampling parameters are fixed and left out; tool choice auto or none.",
            family: Family::Openai,
            capabilities: Capabilities {
                reasoning_efforts: efforts(&["none", "high"]),
                thinking_off: ThinkingOff::Disabled,
                reasoning_param: Some(ReasoningParam::ThinkingToggle),
                ..kimi
            },
        },
        CapabilityPreset {
            name: "ollama",
            description: "Models served by Ollama without reasoning: no tool choice, images as data URLs only.",
            family: Family::Ollama,
            capabilities: ollama.clone(),
        },
        CapabilityPreset {
            name: "ollama-thinking",
            description: "Models served by Ollama that reason (gpt-oss, qwen3, deepseek-r1): an effort level, none switches it off.",
            family: Family::Ollama,
            capabilities: Capabilities {
                reasoning: ReasoningStyle::Effort,
                reasoning_efforts: efforts(&["none", "low", "medium", "high"]),
                ..ollama
            },
        },
        CapabilityPreset {
            name: "gemini-2-5",
            description: "Gemini 2.5 models: reasoning steered by a token budget (0 switches it off on Flash); sampling parameters accepted.",
            family: Family::Gemini,
            capabilities: Capabilities {
                temperature: Accepted,
                top_p: Accepted,
                top_k: Accepted,
                reasoning: ReasoningStyle::Budget,
                reasoning_efforts: Vec::new(),
                thinking_off: ThinkingOff::Disabled,
                ..gemini.clone()
            },
        },
        CapabilityPreset {
            name: "gemini-3-flash",
            description: "Gemini 3.6 and later Flash models: reasoning always on, steered by an effort (low, medium, high); sampling parameters ignored by the model and left out; no trailing assistant turn.",
            family: Family::Gemini,
            capabilities: gemini.clone(),
        },
        CapabilityPreset {
            name: "gemini-3-flash-lite",
            description: "Gemini 3 Flash-Lite models: as the Flash models, with the effort minimal as well.",
            family: Family::Gemini,
            capabilities: Capabilities {
                reasoning_efforts: efforts(&["minimal", "low", "medium", "high"]),
                ..gemini.clone()
            },
        },
        CapabilityPreset {
            name: "gemini-3-pro",
            description: "Gemini 3.1 Pro: reasoning always on (low, medium, high); sampling parameters accepted.",
            family: Family::Gemini,
            capabilities: Capabilities { temperature: Accepted, top_p: Accepted, top_k: Accepted, ..gemini },
        },
        // xAI (docs.x.ai, reasoning): the reasoning models refuse `stop` and the penalties and
        // return no reasoning text on Chat Completions; grok-4 takes no effort either.
        CapabilityPreset {
            name: "xai-reasoning",
            description: "xAI's reasoning models (grok-4 and later, grok-code): always reason, no effort, no penalties; stop is left out.",
            family: Family::Openai,
            capabilities: Capabilities {
                penalties: Rejected,
                reasoning: ReasoningStyle::AlwaysOn,
                thinking_off: ThinkingOff::Unsupported,
                prompt_cache: PromptCacheStyle::Automatic,
                vision: true,
                structured_output: true,
                assistant_prefill: false,
                drop_fields: vec!["stop".into()],
                ..Default::default()
            },
        },
        CapabilityPreset {
            name: "xai-mini",
            description: "xAI's mini reasoning models (grok-3-mini): an effort of low or high, no penalties; stop is left out.",
            family: Family::Openai,
            capabilities: Capabilities {
                penalties: Rejected,
                reasoning: ReasoningStyle::Effort,
                reasoning_efforts: efforts(&["low", "high"]),
                prompt_cache: PromptCacheStyle::Automatic,
                structured_output: true,
                assistant_prefill: false,
                drop_fields: vec!["stop".into()],
                ..Default::default()
            },
        },
        CapabilityPreset {
            name: "xai-chat",
            description: "xAI's models without reasoning (grok-3, the non-reasoning variants): every sampling parameter is accepted.",
            family: Family::Openai,
            capabilities: Capabilities {
                prompt_cache: PromptCacheStyle::Automatic,
                vision: true,
                structured_output: true,
                ..Default::default()
            },
        },
        CapabilityPreset {
            name: "embedding",
            description: "Embedding models: text in, vectors out; no chat parameters apply.",
            family: Family::Openai,
            capabilities: Capabilities {
                temperature: Rejected,
                top_p: Rejected,
                penalties: Rejected,
                tools: false,
                tool_choice_forced: false,
                assistant_prefill: false,
                ..Default::default()
            },
        },
        CapabilityPreset {
            name: "decision",
            description: "Decision models (TypeSafe's Jev): typed answers about a text, for plugins; no chat parameters apply.",
            family: Family::Typesafe,
            capabilities: Capabilities {
                temperature: Rejected,
                top_p: Rejected,
                penalties: Rejected,
                tools: false,
                tool_choice_forced: false,
                assistant_prefill: false,
                ..Default::default()
            },
        },
        CapabilityPreset {
            name: "speech",
            description: "Text-to-speech models: no chat parameters apply.",
            family: Family::Openai,
            capabilities: Capabilities {
                temperature: Rejected,
                top_p: Rejected,
                penalties: Rejected,
                tools: false,
                tool_choice_forced: false,
                assistant_prefill: false,
                ..Default::default()
            },
        },
        CapabilityPreset {
            name: "image",
            description: "Image models (DALL·E, gpt-image, Imagen, Gemini's image models): a prompt and images in, images out; no chat parameters apply.",
            family: Family::Openai,
            capabilities: Capabilities {
                temperature: Rejected,
                top_p: Rejected,
                penalties: Rejected,
                tools: false,
                tool_choice_forced: false,
                assistant_prefill: false,
                ..Default::default()
            },
        },
        CapabilityPreset {
            name: "transcription",
            description: "Speech-to-text models (Whisper, gpt-4o-transcribe): audio in, text out; no chat parameters apply.",
            family: Family::Openai,
            capabilities: Capabilities {
                temperature: Rejected,
                top_p: Rejected,
                penalties: Rejected,
                tools: false,
                tool_choice_forced: false,
                assistant_prefill: false,
                ..Default::default()
            },
        },
        CapabilityPreset {
            name: "rerank",
            description: "Rerank models (Cohere, Jina, Voyage): a query and documents in, an order out; no chat parameters apply.",
            family: Family::Cohere,
            capabilities: Capabilities {
                temperature: Rejected,
                top_p: Rejected,
                penalties: Rejected,
                tools: false,
                tool_choice_forced: false,
                assistant_prefill: false,
                ..Default::default()
            },
        },
        CapabilityPreset {
            name: "realtime",
            description: "Realtime models (gpt-realtime): a WebSocket session with text and audio; the session's own settings apply, not chat parameters.",
            family: Family::Openai,
            capabilities: Capabilities {
                temperature: Rejected,
                top_p: Rejected,
                penalties: Rejected,
                tools: false,
                tool_choice_forced: false,
                assistant_prefill: false,
                ..Default::default()
            },
        },
    ]
}

/// The kind of model an id probably names, and what discovery read about it: an embedding
/// model when the provider says so (Gemini's methods, Ollama's capabilities) or its id says
/// `embed`; a speech model for `tts`; an image model for `dall-e`, `gpt-image`, `imagen` or
/// an id ending in `-image` (Gemini's image models); a transcription model for `whisper` or
/// `transcribe`; a rerank model for `rerank`; a realtime model for `realtime`; otherwise a
/// chat model.
pub fn suggested_kind(upstream_id: &str, embedding: Option<bool>) -> ModelKind {
    let id = upstream_id.trim_start_matches("models/").to_ascii_lowercase();
    if embedding == Some(true) || (embedding.is_none() && id.contains("embed")) {
        ModelKind::Embedding
    } else if id.contains("realtime") {
        // Before speech and transcription: `gpt-realtime` models also hear and speak.
        ModelKind::Realtime
    } else if id.contains("transcribe") || id.starts_with("whisper") || id.contains("-whisper") {
        ModelKind::Transcription
    } else if id.starts_with("tts") || id.contains("-tts") {
        ModelKind::Speech
    } else if id.starts_with("dall-e")
        || id.starts_with("gpt-image")
        || id.starts_with("imagen")
        || id.ends_with("-image")
        || id.contains("-image-")
    {
        ModelKind::Image
    } else if id.contains("rerank") {
        ModelKind::Rerank
    } else {
        ModelKind::Chat
    }
}

/// The kind and capability preset a model probably needs, from what discovery read.
pub fn suggestion(
    family: Family,
    upstream_id: &str,
    vision: Option<bool>,
    reasons: Option<bool>,
    embedding: Option<bool>,
) -> (ModelKind, &'static str) {
    // A decision provider offers nothing else, and neither does a rerank provider.
    match family {
        Family::Typesafe => return (ModelKind::Decision, "decision"),
        Family::Cohere => return (ModelKind::Rerank, "rerank"),
        _ => {}
    }
    match suggested_kind(upstream_id, embedding) {
        ModelKind::Chat => (ModelKind::Chat, suggested_preset(family, upstream_id, vision, reasons)),
        kind => (kind, kind.preset()),
    }
}

/// The capability preset a model probably needs, from its provider's family and its id, and
/// what discovery read about it. A starting point that an administrator can change.
pub fn suggested_preset(
    family: Family,
    upstream_id: &str,
    vision: Option<bool>,
    reasons: Option<bool>,
) -> &'static str {
    let id = upstream_id.trim_start_matches("models/").to_ascii_lowercase();
    let starts = |p: &str| id.starts_with(p);
    match family {
        Family::Anthropic => "anthropic-adaptive",
        Family::Typesafe => "decision",
        Family::Cohere => "rerank",
        _ if id.contains("embed") => "embedding",
        Family::Ollama => {
            if reasons == Some(true) {
                "ollama-thinking"
            } else {
                "ollama"
            }
        }
        Family::Gemini | Family::Openai => {
            if starts("gemini-2") {
                "gemini-2-5"
            } else if starts("gemini-") && id.contains("flash-lite") {
                "gemini-3-flash-lite"
            } else if starts("gemini-") && id.contains("pro") {
                "gemini-3-pro"
            } else if starts("gemini-") {
                "gemini-3-flash"
            } else if starts("deepseek") {
                if vision == Some(true) || (vision.is_none() && id.contains("flash")) {
                    "deepseek-vision"
                } else {
                    "deepseek"
                }
            } else if starts("glm-4") {
                "glm-4"
            } else if starts("glm-") {
                // GLM-5.3-Flash and the `…v…` models see images.
                if vision == Some(true) || id.contains("flash") || id.contains("5v") { "glm-5-vision" } else { "glm-5" }
            } else if starts("kimi-k3") {
                "kimi-k3"
            } else if starts("kimi-") {
                "kimi-k2"
            } else if starts("gpt-5") || starts("o1") || starts("o3") || starts("o4") {
                "openai-reasoning"
            } else if starts("gpt-") || starts("chatgpt") {
                "openai-chat"
            } else if starts("grok-") {
                if id.contains("non-reasoning") || starts("grok-2") || (starts("grok-3") && !id.contains("mini")) {
                    "xai-chat"
                } else if id.contains("mini") {
                    "xai-mini"
                } else {
                    "xai-reasoning"
                }
            } else if starts("tts") || id.contains("-tts") {
                "speech"
            } else {
                "openai-compatible"
            }
        }
    }
}

// --- model -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    #[default]
    Chat,
    Speech,
    Embedding,
    /// Answers typed questions about a text (TypeSafe's Jev): for plugins, and for callers
    /// only through `/v1/systemone`.
    Decision,
    /// Makes or edits images (`/v1/images/…`): DALL·E, gpt-image, Imagen, Gemini's image models.
    Image,
    /// Turns speech into text (`/v1/audio/transcriptions` and `translations`).
    Transcription,
    /// Orders documents by relevance to a query (`/v1/rerank`).
    Rerank,
    /// Speaks over a WebSocket session (`/v1/realtime`). Not a chat model: such models do not
    /// serve chat completions.
    Realtime,
}

impl ModelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelKind::Chat => "chat",
            ModelKind::Speech => "speech",
            ModelKind::Embedding => "embedding",
            ModelKind::Decision => "decision",
            ModelKind::Image => "image",
            ModelKind::Transcription => "transcription",
            ModelKind::Rerank => "rerank",
            ModelKind::Realtime => "realtime",
        }
    }

    /// The capability preset of a model of this kind. Chat models have many; for them this is
    /// only the generic one, and [`suggested_preset`] picks a better one from the id.
    pub fn preset(self) -> &'static str {
        match self {
            ModelKind::Chat => "openai-compatible",
            kind => kind.as_str(),
        }
    }
}

/// What a model takes as input and gives as output (concept section 6.1): `text`, `image`,
/// `document`, `audio`, `embedding`, `ranking`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Modalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}

impl Model {
    /// The model's modalities: as set, or else what its kind and capabilities imply.
    pub fn modalities(&self) -> Modalities {
        let words = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let mut derived = match self.kind {
            ModelKind::Speech => Modalities { input: words(&["text"]), output: words(&["audio"]) },
            ModelKind::Embedding => Modalities { input: words(&["text"]), output: words(&["embedding"]) },
            ModelKind::Decision => Modalities { input: words(&["text"]), output: Vec::new() },
            ModelKind::Image => Modalities { input: words(&["text", "image"]), output: words(&["image"]) },
            ModelKind::Transcription => Modalities { input: words(&["audio"]), output: words(&["text"]) },
            ModelKind::Rerank => Modalities { input: words(&["text"]), output: words(&["ranking"]) },
            ModelKind::Realtime => Modalities { input: words(&["text", "audio"]), output: words(&["text", "audio"]) },
            ModelKind::Chat => {
                let mut input = words(&["text"]);
                if self.capabilities.vision {
                    input.push("image".into());
                }
                if self.capabilities.documents {
                    input.push("document".into());
                }
                Modalities { input, output: words(&["text"]) }
            }
        };
        if !self.modalities.input.is_empty() {
            derived.input = self.modalities.input.clone();
        }
        if !self.modalities.output.is_empty() {
            derived.output = self.modalities.output.clone();
        }
        derived
    }
}

/// The life-cycle state of a model (concept section 6.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelState {
    /// Being set up. Not callable.
    #[default]
    Draft,
    Active,
    /// Still callable; responses carry a deprecation notice; new templates may not use it.
    Deprecated,
    /// Not callable itself. Requests follow the successor where the grant allows it.
    Retired,
    /// Emergency off switch. Returns to the state it had before.
    Disabled,
}

impl ModelState {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelState::Draft => "draft",
            ModelState::Active => "active",
            ModelState::Deprecated => "deprecated",
            ModelState::Retired => "retired",
            ModelState::Disabled => "disabled",
        }
    }

    /// Whether requests can be served by this model.
    pub fn callable(self) -> bool {
        matches!(self, ModelState::Active | ModelState::Deprecated)
    }

    /// Whether model lists show a model in this state (concept section 6.3): the callable
    /// ones, and disabled ones, marked.
    pub fn listed(self) -> bool {
        matches!(self, ModelState::Active | ModelState::Deprecated | ModelState::Disabled)
    }

    /// Whether a new template or grant may name a model in this state.
    pub fn assignable(self) -> bool {
        self == ModelState::Active
    }

    /// The transitions the life cycle allows from this state.
    pub fn can_become(self, to: ModelState, previous: Option<ModelState>) -> bool {
        use ModelState::*;
        match (self, to) {
            (Draft, Active) | (Active, Deprecated) | (Deprecated, Retired) | (Active, Retired) => true,
            // A deprecation or retirement can be taken back by a platform admin.
            (Deprecated, Active) | (Retired, Active) | (Retired, Deprecated) => true,
            (Active | Deprecated, Disabled) => true,
            (Disabled, s) => previous == Some(s) || (previous.is_none() && s == Active),
            _ => false,
        }
    }
}

/// One provider endpoint for one upstream model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Model {
    /// The id applications use, for example `anthropic/claude-sonnet-5-5`. Unique.
    pub name: String,
    pub provider_id: String,
    /// The id the provider knows the model by.
    pub upstream_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub kind: ModelKind,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub state: ModelState,
    /// The state the model had before it was disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_state: Option<ModelState>,
    /// The model that replaces this one when it is retired (its object id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_at: Option<DateTime<Utc>>,
    /// When the model is to be retired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sunset_at: Option<DateTime<Utc>>,
    /// Retire the model by itself when `sunset_at` has passed (concept section 6.3).
    #[serde(default)]
    pub retire_at_sunset: bool,
    /// What the model takes and gives. Empty lists are read from its kind and capabilities.
    #[serde(default)]
    pub modalities: Modalities,
    /// Explicitly without cost, for example a local model. Allows activation without a price.
    #[serde(default)]
    pub free: bool,
    #[serde(default)]
    pub notes: String,
}

impl Doc for Model {
    const KIND: &'static str = "model";
    const PREFIX: &'static str = "mdl";
}

/// Characters allowed in a model id. Slashes, dots and colons are common in model names; the
/// `@` prefix is reserved for templates.
pub fn is_model_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('@')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b':'))
}

// --- grants ----------------------------------------------------------------------------------

/// Allows an organisation to use a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OrgGrant {
    /// The model's object id.
    pub model_id: String,
    /// When the model is retired, serve requests with its successor. Default true.
    #[serde(default = "yes")]
    pub follow_successor: bool,
    /// A lower output-token ceiling than the model's own.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// A ceiling on the reasoning token budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_reasoning_tokens: Option<u32>,
    /// A ceiling on how long one call may take, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_timeout_seconds: Option<u32>,
}

impl Doc for OrgGrant {
    const KIND: &'static str = "org_grant";
    const PREFIX: &'static str = "ogr";
}

/// Restricts a workspace to some of its organisation's models. A workspace without grants of
/// its own may use every model its organisation was granted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceGrant {
    /// The model's object id.
    pub model_id: String,
    #[serde(default = "yes")]
    pub follow_successor: bool,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// A ceiling on the reasoning token budget, within the organisation's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_reasoning_tokens: Option<u32>,
    /// A ceiling on how long one call may take, in seconds, within the organisation's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_timeout_seconds: Option<u32>,
}

impl Doc for WorkspaceGrant {
    const KIND: &'static str = "ws_grant";
    const PREFIX: &'static str = "wgr";
}

fn yes() -> bool {
    true
}

// --- prices ----------------------------------------------------------------------------------

/// Rates of one price variant. Token rates are per one million tokens.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Rates {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write_5m: Option<f64>,
    pub cache_write_1h: Option<f64>,
    /// Only where reasoning tokens are priced differently from output tokens.
    pub reasoning: Option<f64>,
    /// Speech: per character of input text.
    pub per_character: Option<f64>,
    pub per_request: Option<f64>,
    /// Per provider-executed tool call, for example a web search.
    pub per_server_tool_call: Option<f64>,
    /// Audio tokens among the input, where they are priced apart from text (realtime and
    /// transcription models). Without it they are charged at `input`.
    pub input_audio: Option<f64>,
    /// Audio tokens among the output. Without it they are charged at `output`.
    pub output_audio: Option<f64>,
    /// Image tokens among the input. Without it they are charged at `input`.
    pub input_image: Option<f64>,
    /// Image tokens among the output (image models). Without it they are charged at `output`.
    pub output_image: Option<f64>,
    /// Image models: per image made. Sizes and qualities priced differently are variants.
    pub per_image: Option<f64>,
    /// Transcription: per second of audio.
    pub per_audio_second: Option<f64>,
    /// Rerank: per search unit.
    pub per_search_unit: Option<f64>,
}

impl Rates {
    /// Every rate with its name, in the order of the fields.
    pub fn named(&self) -> [(&'static str, Option<f64>); 16] {
        [
            ("input", self.input),
            ("output", self.output),
            ("cache_read", self.cache_read),
            ("cache_write_5m", self.cache_write_5m),
            ("cache_write_1h", self.cache_write_1h),
            ("reasoning", self.reasoning),
            ("per_character", self.per_character),
            ("per_request", self.per_request),
            ("per_server_tool_call", self.per_server_tool_call),
            ("input_audio", self.input_audio),
            ("output_audio", self.output_audio),
            ("input_image", self.input_image),
            ("output_image", self.output_image),
            ("per_image", self.per_image),
            ("per_audio_second", self.per_audio_second),
            ("per_search_unit", self.per_search_unit),
        ]
    }

    /// Takes over the rates `other` names; the others stay.
    pub fn overlay(&mut self, other: &Rates) {
        let pick = |mine: &mut Option<f64>, theirs: Option<f64>| *mine = theirs.or(*mine);
        pick(&mut self.input, other.input);
        pick(&mut self.output, other.output);
        pick(&mut self.cache_read, other.cache_read);
        pick(&mut self.cache_write_5m, other.cache_write_5m);
        pick(&mut self.cache_write_1h, other.cache_write_1h);
        pick(&mut self.reasoning, other.reasoning);
        pick(&mut self.per_character, other.per_character);
        pick(&mut self.per_request, other.per_request);
        pick(&mut self.per_server_tool_call, other.per_server_tool_call);
        pick(&mut self.input_audio, other.input_audio);
        pick(&mut self.output_audio, other.output_audio);
        pick(&mut self.input_image, other.input_image);
        pick(&mut self.output_image, other.output_image);
        pick(&mut self.per_image, other.per_image);
        pick(&mut self.per_audio_second, other.per_audio_second);
        pick(&mut self.per_search_unit, other.per_search_unit);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    #[default]
    Manual,
    Feed,
    Extracted,
    Import,
    Seed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PriceStatus {
    #[default]
    Active,
    /// Found by the price tracker and waiting for approval.
    Proposed,
    Superseded,
    Rejected,
}

/// Rates that apply when a call's prompt (input, cached and written tokens together) is larger
/// than a threshold: long-context prices (Gemini 3.1 Pro above 200k) and input-length tiers
/// (GLM on bigmodel.cn). The highest threshold a prompt exceeds applies, to the whole call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PriceTier {
    pub above_input_tokens: u64,
    /// Replace the standard rates they name; the others stay.
    #[serde(flatten)]
    pub rates: Rates,
    /// The same for variants. A variant without an entry here keeps its own rates.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, Rates>,
}

/// When a variant's rates apply by the clock, for example DeepSeek's off-peak hours. A call
/// is priced by the time it started; a variant the provider reports (its service tier) wins.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PriceWindow {
    /// The variant whose rates apply in these hours.
    pub variant: String,
    /// ISO weekdays: 1 is Monday, 7 is Sunday.
    pub weekdays: Vec<u8>,
    /// Spans of the day in UTC, `HH:MM-HH:MM`, start included and end not; `00:00` or `24:00`
    /// as an end is midnight, so `00:00-00:00` is the whole day.
    pub hours_utc: Vec<String>,
}

impl PriceWindow {
    /// The spans as minutes of the day, start included and end not. `None` when one is malformed.
    pub fn spans(&self) -> Option<Vec<(u32, u32)>> {
        let minute = |t: &str| -> Option<u32> {
            let (h, m) = t.trim().split_once(':')?;
            let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
            // `24:00` is midnight at the end of the day, read as 0 like `00:00`.
            ((h < 24 && m < 60) || (h, m) == (24, 0)).then_some((h % 24) * 60 + m)
        };
        self.hours_utc
            .iter()
            .map(|span| {
                let (a, b) = span.split_once('-')?;
                let (a, b) = (minute(a)?, minute(b)?);
                Some((a, if b == 0 { 24 * 60 } else { b }))
            })
            .collect()
    }

    fn covers(&self, at: DateTime<Utc>) -> bool {
        use chrono::{Datelike, Timelike};
        let day = at.weekday().number_from_monday() as u8;
        let minute = at.hour() * 60 + at.minute();
        self.weekdays.contains(&day) && self.spans().is_some_and(|s| s.iter().any(|(a, b)| (*a..*b).contains(&minute)))
    }
}

/// One version of a model's price (concept section 13.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Price {
    /// The model's object id.
    pub model_id: String,
    pub version: u32,
    pub currency: String,
    #[serde(flatten)]
    pub rates: Rates,
    /// Separate rates for variants a provider bills differently, for example `batch` or `fast`.
    #[serde(default)]
    pub variants: BTreeMap<String, Rates>,
    /// Rates for long prompts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<PriceTier>,
    /// Hours in which a variant applies by the clock.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<PriceWindow>,
    pub effective_from: DateTime<Utc>,
    #[serde(default)]
    pub source: PriceSource,
    #[serde(default)]
    pub status: PriceStatus,
    #[serde(default)]
    pub evidence_url: String,
    /// The text the figures were taken from.
    #[serde(default)]
    pub evidence_text: String,
    /// When the price tracker last found the same figures at its source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed_at: Option<DateTime<Utc>>,
}

impl Price {
    /// The variant the clock selects at `at`, if a window covers it.
    pub fn window_variant(&self, at: DateTime<Utc>, holidays: Option<&super::HolidayDates>) -> Option<&str> {
        if let Some(w) = self.windows.iter().find(|w| w.covers(at)) {
            return Some(&w.variant);
        }
        // On a holiday of the provider's calendar the clock's variant applies all day, where
        // the price's windows name one variant (DeepSeek: off-peak on Chinese public holidays).
        let first = self.windows.first()?;
        let one = self.windows.iter().all(|w| w.variant == first.variant);
        (one && holidays.is_some_and(|h| h.contains(at))).then_some(first.variant.as_str())
    }

    /// When the figures were last known to be right: taken into effect, or found unchanged by
    /// the price tracker. The stale threshold counts from here (concept section 13.3): a
    /// price that is checked every day is not stale, however long it has stood.
    pub fn checked_at(&self) -> DateTime<Utc> {
        self.confirmed_at.map_or(self.effective_from, |c| c.max(self.effective_from))
    }
}

impl Doc for Price {
    const KIND: &'static str = "price";
    const PREFIX: &'static str = "prc";
}

#[cfg(test)]
mod tests {
    use super::*;
    use ModelState::*;

    #[test]
    fn life_cycle_transitions() {
        assert!(Draft.can_become(Active, None));
        assert!(Active.can_become(Deprecated, None));
        assert!(Deprecated.can_become(Retired, None));
        assert!(Active.can_become(Retired, None));
        assert!(Active.can_become(Disabled, None));
        assert!(Deprecated.can_become(Disabled, None));
        // Disabled returns only to where it came from.
        assert!(Disabled.can_become(Deprecated, Some(Deprecated)));
        assert!(!Disabled.can_become(Active, Some(Deprecated)));
        assert!(Disabled.can_become(Active, None));
        // Not allowed.
        assert!(!Draft.can_become(Deprecated, None));
        assert!(!Draft.can_become(Disabled, None));
        assert!(!Retired.can_become(Disabled, None));
        assert!(!Active.can_become(Draft, None));
        assert!(!Active.can_become(Active, None));
    }

    #[test]
    fn callable_and_assignable_states() {
        assert!(Active.callable() && Deprecated.callable());
        assert!(!Draft.callable() && !Retired.callable() && !Disabled.callable());
        assert!(Active.assignable() && !Deprecated.assignable());
    }

    #[test]
    fn model_names() {
        for ok in ["anthropic/claude-sonnet-5-5", "gemini-3.8-flash", "glm/glm-5.3", "llama3:8b", "a_b"] {
            assert!(is_model_name(ok), "{ok}");
        }
        for bad in ["", "@planner", "a b", "a?b", "é"] {
            assert!(!is_model_name(bad), "{bad}");
        }
    }

    #[test]
    fn presets_encode_the_documented_anthropic_rules() {
        let presets = capability_presets();
        let get = |n: &str| presets.iter().find(|p| p.name == n).unwrap().capabilities.clone();
        let sonnet = get("anthropic-sonnet-5-5");
        assert_eq!(sonnet.temperature, ParamSupport::DefaultOnly);
        assert_eq!(sonnet.thinking_off, ThinkingOff::BetweenTools);
        assert!(!sonnet.tool_choice_forced && !sonnet.assistant_prefill);
        let haiku = get("anthropic-budget");
        assert_eq!((haiku.reasoning, haiku.reasoning_min_budget), (ReasoningStyle::Budget, Some(1024)));
        assert!(haiku.reasoning_efforts.is_empty());
        let opus = get("anthropic-always-thinking");
        assert_eq!((opus.reasoning, opus.thinking_off), (ReasoningStyle::AlwaysOn, ThinkingOff::Unsupported));
        assert_eq!(opus.temperature, ParamSupport::Rejected);
        assert_eq!(get("openai-reasoning").max_tokens_field, MaxTokensField::MaxCompletionTokens);
        // Names are unique.
        let mut names: Vec<_> = presets.iter().map(|p| p.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), presets.len());
    }

    #[test]
    fn a_model_document_needs_only_the_essentials() {
        let m: Model = serde_json::from_value(serde_json::json!({
            "name": "openai/gpt-x", "provider_id": "prv_1", "upstream_id": "gpt-x"
        }))
        .unwrap();
        assert_eq!(m.state, Draft);
        assert_eq!(m.capabilities, Capabilities::default());
        assert!(!m.free);
    }

    #[test]
    fn a_preset_is_suggested_by_the_model_id() {
        use Family::*;
        let s = |f, id: &str| suggested_preset(f, id, None, None);
        assert_eq!(s(Gemini, "models/gemini-2.5-flash"), "gemini-2-5");
        assert_eq!(s(Gemini, "gemini-3.5-flash-lite"), "gemini-3-flash-lite");
        assert_eq!(s(Gemini, "gemini-3.1-pro-preview"), "gemini-3-pro");
        assert_eq!(s(Gemini, "gemini-3.8-flash"), "gemini-3-flash");
        assert_eq!(s(Openai, "deepseek-v4-pro"), "deepseek");
        assert_eq!(s(Openai, "deepseek-flash"), "deepseek-vision");
        assert_eq!(suggested_preset(Openai, "deepseek-v4-pro", Some(true), None), "deepseek-vision");
        assert_eq!(s(Openai, "glm-4.7"), "glm-4");
        assert_eq!((s(Openai, "glm-5.3"), s(Openai, "glm-5.3-flash")), ("glm-5", "glm-5-vision"));
        assert_eq!((s(Openai, "kimi-k3"), s(Openai, "kimi-k2.6")), ("kimi-k3", "kimi-k2"));
        assert_eq!((s(Openai, "gpt-5-mini"), s(Openai, "gpt-4.1-nano")), ("openai-reasoning", "openai-chat"));
        assert_eq!(suggested_preset(Ollama, "qwen3", None, Some(true)), "ollama-thinking");
        assert_eq!(s(Ollama, "llama3.2"), "ollama");
        assert_eq!(s(Anthropic, "claude-haiku-5-5"), "anthropic-adaptive");
        assert_eq!(s(Openai, "some-new-model"), "openai-compatible");
        assert_eq!((s(Openai, "grok-4"), s(Openai, "grok-code-fast-1")), ("xai-reasoning", "xai-reasoning"));
        assert_eq!((s(Openai, "grok-3-mini"), s(Openai, "grok-3")), ("xai-mini", "xai-chat"));
        assert_eq!(s(Openai, "grok-4-fast-non-reasoning"), "xai-chat");
        // Every suggestion is a preset.
        let names: Vec<_> = capability_presets().iter().map(|p| p.name).collect();
        for id in ["gemini-2.5-pro", "deepseek-flash", "glm-5", "kimi-k2.7-code", "tts-1", "text-embedding-3-small"] {
            assert!(names.contains(&s(Openai, id)), "{id}");
        }
    }

    #[test]
    fn embedding_and_speech_models_are_told_apart() {
        use Family::*;
        let k = |id: &str| suggestion(Openai, id, None, None, None);
        assert_eq!(k("text-embedding-3-small"), (ModelKind::Embedding, "embedding"));
        assert_eq!(k("models/gemini-embedding-001"), (ModelKind::Embedding, "embedding"));
        assert_eq!(k("tts-1"), (ModelKind::Speech, "speech"));
        assert_eq!(k("gpt-4o-mini-tts"), (ModelKind::Speech, "speech"));
        assert_eq!(k("gpt-4.1-nano"), (ModelKind::Chat, "openai-chat"));
        assert_eq!(k("gpt-image-1"), (ModelKind::Image, "image"));
        assert_eq!(k("dall-e-3"), (ModelKind::Image, "image"));
        assert_eq!(k("models/imagen-4.0-generate-001"), (ModelKind::Image, "image"));
        assert_eq!(k("gemini-3.8-flash-image"), (ModelKind::Image, "image"));
        assert_eq!(k("gemini-2.5-flash-image-preview"), (ModelKind::Image, "image"));
        assert_eq!(k("whisper-1"), (ModelKind::Transcription, "transcription"));
        assert_eq!(k("gpt-4o-mini-transcribe"), (ModelKind::Transcription, "transcription"));
        assert_eq!(k("whisper-large-v3-turbo"), (ModelKind::Transcription, "transcription"));
        assert_eq!(k("gpt-realtime-mini"), (ModelKind::Realtime, "realtime"));
        assert_eq!(k("jina-reranker-v2-base-multilingual"), (ModelKind::Rerank, "rerank"));
        assert_eq!(suggestion(Family::Cohere, "rerank-v3.5", None, None, None), (ModelKind::Rerank, "rerank"));
        // What the provider reports wins over the id.
        assert_eq!(suggestion(Ollama, "nomic-embed-text:latest", None, None, None).0, ModelKind::Embedding);
        assert_eq!(suggestion(Ollama, "bge-m3", None, None, Some(true)), (ModelKind::Embedding, "embedding"));
        assert_eq!(suggestion(Gemini, "text-embedder-chat", None, None, Some(false)).0, ModelKind::Chat);
    }
}
