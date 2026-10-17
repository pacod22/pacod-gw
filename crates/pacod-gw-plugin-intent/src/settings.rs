//! A binding's settings (`docs/plugin-concept.md`, sections 5.3 to 5.6).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which engine classifies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EngineChoice {
    /// A chat model the organisation chooses.
    #[default]
    Llm,
    /// TypeSafe's Jev, a classifier that answers with probabilities.
    Jev,
    /// Jev first; the LLM decides when Jev is unsure.
    Cascade,
}

impl EngineChoice {
    pub fn name(self) -> &'static str {
        match self {
            EngineChoice::Llm => "llm",
            EngineChoice::Jev => "jev",
            EngineChoice::Cascade => "cascade",
        }
    }
}

/// Which part of the conversation is classified.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The user's newest turn: what the user asks now.
    #[default]
    LastUser,
    /// The newest turns, as many as `turns` says.
    LastTurns,
    /// The whole conversation, within the budget.
    Conversation,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LlmSettings {
    /// A `@template` or a model id the workspace may use. Its capability profile must offer
    /// JSON-schema output.
    pub model: String,
    /// Ask for a short reason, kept in the audit record only.
    pub rationale: bool,
    /// Named facts to extract, kept in the audit record only.
    pub fields: Vec<Field>,
}

/// A fact the LLM engine extracts, for example the product a question is about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// Lower-case letters, digits and underscores.
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct JevSettings {
    /// The decision model of the repository, by its id. A versioned one is recommended: the
    /// aliases (`jev-latest`, `jev-preview`) move when TypeSafe releases.
    pub model: String,
}

impl Default for JevSettings {
    fn default() -> Self {
        Self { model: "typesafe/jev-1.13.0".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CascadeSettings {
    /// Below this confidence of Jev's intent the LLM decides.
    pub escalate_below: f32,
    /// The LLM also decides when Jev answers `unknown`.
    pub escalate_unknown: bool,
}

impl Default for CascadeSettings {
    fn default() -> Self {
        Self { escalate_below: 0.6, escalate_unknown: true }
    }
}

/// The settings of an intent binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// The intent catalogue: the workspace's own of that name, else its organisation's.
    pub catalogue: String,
    pub engine: EngineChoice,
    pub scope: Scope,
    /// With `scope: last_turns`: how many.
    pub turns: u8,
    /// Classify the system prompt too. Off: it is the application's, not the user's.
    pub include_system: bool,
    /// Classify the results of tool calls too. Off: they are data, and a common way to smuggle
    /// instructions in.
    pub include_tool_results: bool,
    /// The most text classified, in tokens (about four characters each). The oldest text goes
    /// first.
    pub max_input_tokens: u32,
    /// Routing and limits act on the intent from this confidence; below it they see
    /// `unknown`. The event records the intent either way.
    pub act_min_confidence: f32,
    /// How long an answer is reused for the same text, in seconds. 0 asks every time.
    pub cache_seconds: u32,
    pub llm: LlmSettings,
    pub jev: JevSettings,
    pub cascade: CascadeSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            catalogue: String::new(),
            engine: EngineChoice::Llm,
            scope: Scope::LastUser,
            turns: 4,
            include_system: false,
            include_tool_results: false,
            max_input_tokens: 2_000,
            act_min_confidence: 0.5,
            cache_seconds: 600,
            llm: LlmSettings::default(),
            jev: JevSettings::default(),
            cascade: CascadeSettings::default(),
        }
    }
}

impl Settings {
    /// What is wrong with the settings, apart from the catalogue and the models.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.catalogue.trim().is_empty() {
            out.push("catalogue names the intent catalogue to classify against".into());
        }
        if !(100..=16_000).contains(&self.max_input_tokens) {
            out.push("max_input_tokens must be between 100 and 16000".into());
        }
        if self.scope == Scope::LastTurns && !(1..=50).contains(&self.turns) {
            out.push("turns must be between 1 and 50".into());
        }
        if !(0.0..=1.0).contains(&self.act_min_confidence) {
            out.push("act_min_confidence must be between 0 and 1".into());
        }
        if !(0.0..=1.0).contains(&self.cascade.escalate_below) {
            out.push("cascade.escalate_below must be between 0 and 1".into());
        }
        if self.cache_seconds > 86_400 {
            out.push("cache_seconds must be at most 86400".into());
        }
        let uses_llm = matches!(self.engine, EngineChoice::Llm | EngineChoice::Cascade);
        let uses_jev = matches!(self.engine, EngineChoice::Jev | EngineChoice::Cascade);
        if uses_llm && self.llm.model.trim().is_empty() {
            out.push("llm.model names the model or @template that classifies".into());
        }
        if uses_jev && self.jev.model.trim().is_empty() {
            out.push("jev.model names the decision model that classifies".into());
        }
        if self.llm.fields.len() > 16 {
            out.push("llm.fields holds at most 16 facts".into());
        }
        for f in &self.llm.fields {
            let ok = !f.name.is_empty()
                && f.name.len() <= 40
                && f.name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !ok {
                out.push(format!(
                    "llm.fields: \"{}\" is not a name of lower-case letters, digits and underscores",
                    f.name
                ));
            }
        }
        out
    }
}
