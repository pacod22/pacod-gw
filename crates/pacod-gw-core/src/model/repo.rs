//! The global model repository: models, capability profiles, life cycle, grants and prices
//! (concept sections 6 and 13.1).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Doc, Family};

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
    /// Whether the conversation may end with a partial assistant message to continue.
    pub assistant_prefill: bool,
    pub max_tokens_field: MaxTokensField,
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
            assistant_prefill: true,
            max_tokens_field: MaxTokensField::MaxTokens,
        }
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
    ]
}

// --- model -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    #[default]
    Chat,
    Speech,
    Embedding,
}

/// What a model takes as input and gives as output (concept section 6.1): `text`, `image`,
/// `document`, `audio`, `embedding`.
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
}
