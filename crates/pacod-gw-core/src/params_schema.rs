//! What a model or a template accepts, as a JSON Schema for the applications that call it
//! (concept section 6.5).
//!
//! The schema is made from the capability profile, the grant's ceilings and the template's
//! rules each time a list is read, and never stored, so that it says what the request path
//! does (`params.rs`, `dataplane/translate.rs`): a request that conforms loses none of the
//! caller's parameters to the capability filter and has none of them changed to fit. A
//! parameter the target does not take is the schema `false`; a value the template locks is
//! `const` and `readOnly`, because the gateway writes it whatever the caller sends. What the
//! schema does not name (the conversation, `stream`, `seed`) is left open.

use serde_json::{Map, Value, json};

use crate::model::{
    Capabilities, Model, ModelKind, ParamRange, ParamSupport, ReasoningStyle, Rule, TemplateSpec, ThinkingOff,
};
use crate::snapshot::{GrantTerms, ModelRt, Snapshot, WorkspaceRt};

/// The request vocabulary a schema is written in: that of the list it is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spelling {
    /// The gateway's own interface (`/gw/v1/chat`), in the names templates use.
    Native,
    /// OpenAI's Chat Completions.
    OpenAiChat,
    /// Anthropic's Messages.
    Anthropic,
}

/// One parameter, in neutral terms.
#[derive(Debug, Clone, PartialEq)]
enum Slot {
    /// Dropped or refused by the target: the schema `false`.
    Refused,
    Number(Bounds),
    Choice {
        values: Vec<String>,
        default: Option<String>,
    },
    Flag {
        default: Option<bool>,
    },
    /// The template's value, written whatever the caller sends.
    Locked(Value),
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Bounds {
    min: Option<f64>,
    max: Option<f64>,
    default: Option<f64>,
    integer: bool,
}

/// How a target answers a request for a JSON schema. Ordered from least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SchemaOutput {
    /// The schema is dropped (or the request refused under `reject`).
    None,
    /// In JSON mode, with the schema as an instruction (`dataplane/structured.rs`).
    Emulated,
    Native,
}

/// A model as the gateway serves it to one workspace.
pub struct SchemaTarget<'a> {
    pub model: &'a Model,
    /// The provider's quirk `drop_fields`: removed from every request on its wire.
    pub provider_drop_fields: &'a [String],
    pub terms: GrantTerms,
}

impl<'a> SchemaTarget<'a> {
    /// A model as a workspace may use it: under its grants' ceilings, at its provider.
    pub fn of(snap: &'a Snapshot, ws: &WorkspaceRt, m: &'a ModelRt) -> Self {
        SchemaTarget {
            model: &m.doc,
            provider_drop_fields: snap.providers.get(&m.doc.provider_id).map_or(&[], |p| &p.doc.quirks.drop_fields),
            terms: snap.grant_terms(ws, &m.id).unwrap_or_else(GrantTerms::open),
        }
    }
}

/// What a target accepts, in neutral terms: a model, or what every model of a template's pool
/// accepts, with the template's rules over it.
#[derive(Debug, Clone, PartialEq)]
pub struct Accepted {
    kind: ModelKind,
    max_output_tokens: Slot,
    temperature: Slot,
    top_p: Slot,
    top_k: Slot,
    frequency_penalty: Slot,
    presence_penalty: Slot,
    effort: Slot,
    budget_tokens: Slot,
    summary: Slot,
    /// The model decides how much to reason (Anthropic's adaptive thinking).
    adaptive: bool,
    schema: SchemaOutput,
    json_mode: bool,
    tools: bool,
    tool_choice_none: bool,
    tool_choice_forced: bool,
    stop: bool,
}

/// A sampling parameter: refused, or a number within the model's range, or else the bounds
/// that hold for it everywhere.
fn sampling(support: ParamSupport, range: Option<ParamRange>, floor: Bounds) -> Slot {
    // A parameter accepted only at its default is dropped whatever its value (`Merge::filter`),
    // because the gateway does not know every model's default.
    if support != ParamSupport::Accepted {
        return Slot::Refused;
    }
    let r = range.unwrap_or_default();
    Slot::Number(Bounds { min: r.min.or(floor.min), max: r.max.or(floor.max), default: r.default, ..floor })
}

fn effort(caps: &Capabilities) -> Slot {
    // `none` switches reasoning off where the model allows it (`fit_effort` lets it through
    // and the adapter writes it in the provider's terms); other levels are fitted to the
    // model's own, so only those are offered.
    let mut values = match caps.reasoning {
        ReasoningStyle::None => return Slot::Refused,
        ReasoningStyle::Budget => Vec::new(),
        _ => caps.reasoning_efforts.clone(),
    };
    if caps.thinking_off != ThinkingOff::Unsupported && !values.iter().any(|v| v == "none") {
        values.insert(0, "none".to_owned());
    }
    if values.is_empty() {
        return Slot::Refused;
    }
    let default = caps.default_effort.clone().filter(|d| values.contains(d));
    Slot::Choice { values, default }
}

fn smaller(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

fn larger(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, y) => x.or(y),
    }
}

/// What both of two slots accept.
fn meet(a: Slot, b: &Slot) -> Slot {
    match (a, b) {
        (Slot::Refused, _) | (_, Slot::Refused) => Slot::Refused,
        (Slot::Number(x), Slot::Number(y)) => Slot::Number(Bounds {
            min: larger(x.min, y.min),
            max: smaller(x.max, y.max),
            default: x.default.filter(|d| y.default == Some(*d)),
            integer: x.integer || y.integer,
        }),
        (Slot::Choice { values, default }, Slot::Choice { values: other, default: theirs }) => {
            let values: Vec<String> = values.into_iter().filter(|v| other.contains(v)).collect();
            if values.is_empty() {
                return Slot::Refused;
            }
            let default = default.filter(|d| theirs.as_ref() == Some(d) && values.contains(d));
            Slot::Choice { values, default }
        }
        (Slot::Flag { default }, Slot::Flag { default: theirs }) => {
            Slot::Flag { default: default.filter(|d| *theirs == Some(*d)) }
        }
        (a, _) => a,
    }
}

/// A slot under a template's rule for it (concept section 7.2).
fn ruled<T: serde::Serialize>(slot: Slot, rule: Rule<T>) -> Slot {
    match (slot, rule) {
        // What the target does not take, the capability filter drops from the template too.
        (Slot::Refused, _) | (_, Rule::Omit) => Slot::Refused,
        (_, Rule::Locked(v)) => Slot::Locked(json!(v)),
        (Slot::Number(mut b), Rule::Default(v)) => {
            b.default = json!(v).as_f64();
            Slot::Number(b)
        }
        (Slot::Choice { values, .. }, Rule::Default(v)) => {
            let default = json!(v).as_str().map(str::to_owned).filter(|d| values.contains(d));
            Slot::Choice { values, default }
        }
        (Slot::Flag { .. }, Rule::Default(v)) => Slot::Flag { default: json!(v).as_bool() },
        (slot, _) => slot,
    }
}

impl Accepted {
    /// What one model accepts.
    pub fn of(t: &SchemaTarget<'_>) -> Self {
        let caps = &t.model.capabilities;
        let dropped = |name: &str| caps.drop_fields.iter().chain(t.provider_drop_fields).any(|f| f == name);
        let s = &caps.sampling;
        let slot = |name: &str, support: ParamSupport, range: Option<ParamRange>, floor: Bounds| {
            if dropped(name) { Slot::Refused } else { sampling(support, range, floor) }
        };
        let temperature =
            slot("temperature", caps.temperature, s.temperature, Bounds { min: Some(0.0), ..Default::default() });
        let top_p = slot("top_p", caps.top_p, s.top_p, Bounds { min: Some(0.0), max: Some(1.0), ..Default::default() });
        let top_k = slot("top_k", caps.top_k, s.top_k, Bounds { min: Some(0.0), integer: true, ..Default::default() });
        let frequency_penalty = slot("frequency_penalty", caps.penalties, s.frequency_penalty, Bounds::default());
        let presence_penalty = slot("presence_penalty", caps.penalties, s.presence_penalty, Bounds::default());
        let ceiling = [t.model.max_output_tokens, t.terms.max_output_tokens].into_iter().flatten().min();
        Accepted {
            kind: t.model.kind,
            max_output_tokens: Slot::Number(Bounds {
                min: Some(1.0),
                max: ceiling.map(f64::from),
                default: None,
                integer: true,
            }),
            temperature,
            top_p,
            top_k,
            frequency_penalty,
            presence_penalty,
            effort: if dropped("reasoning_effort") { Slot::Refused } else { effort(caps) },
            // A budget outside the model's minimum and the grant's ceiling is moved into them.
            budget_tokens: match caps.reasoning {
                ReasoningStyle::Budget => Slot::Number(Bounds {
                    min: Some(f64::from(caps.reasoning_min_budget.unwrap_or(1))),
                    max: t.terms.max_reasoning_tokens.map(f64::from),
                    default: None,
                    integer: true,
                }),
                _ => Slot::Refused,
            },
            summary: match caps.reasoning {
                ReasoningStyle::None => Slot::Refused,
                _ => Slot::Flag { default: None },
            },
            adaptive: matches!(caps.reasoning, ReasoningStyle::Adaptive | ReasoningStyle::AlwaysOn),
            schema: if caps.structured_output {
                SchemaOutput::Native
            } else if caps.json_mode {
                SchemaOutput::Emulated
            } else {
                SchemaOutput::None
            },
            json_mode: caps.json_mode,
            tools: caps.tools,
            tool_choice_none: caps.tool_choice_none,
            tool_choice_forced: caps.tool_choice_forced,
            stop: !dropped("stop"),
        }
    }

    /// What every model of a pool accepts: a call may be served, or moved on to, by any of
    /// them. `None` for an empty pool.
    pub fn pool(targets: &[SchemaTarget<'_>]) -> Option<Self> {
        let mut members = targets.iter().map(Self::of);
        let first = members.next()?;
        Some(members.fold(first, |a, b| Accepted {
            kind: a.kind,
            max_output_tokens: meet(a.max_output_tokens, &b.max_output_tokens),
            temperature: meet(a.temperature, &b.temperature),
            top_p: meet(a.top_p, &b.top_p),
            top_k: meet(a.top_k, &b.top_k),
            frequency_penalty: meet(a.frequency_penalty, &b.frequency_penalty),
            presence_penalty: meet(a.presence_penalty, &b.presence_penalty),
            effort: meet(a.effort, &b.effort),
            budget_tokens: meet(a.budget_tokens, &b.budget_tokens),
            summary: meet(a.summary, &b.summary),
            adaptive: a.adaptive && b.adaptive,
            schema: a.schema.min(b.schema),
            json_mode: a.json_mode && b.json_mode,
            tools: a.tools && b.tools,
            tool_choice_none: a.tool_choice_none && b.tool_choice_none,
            tool_choice_forced: a.tool_choice_forced && b.tool_choice_forced,
            stop: a.stop && b.stop,
        }))
    }

    /// Lays a template's rules over what its pool accepts.
    pub fn with_spec(mut self, spec: &TemplateSpec) -> Self {
        self.temperature = ruled(self.temperature, Rule::of(&spec.temperature));
        self.top_p = ruled(self.top_p, Rule::of(&spec.top_p));
        self.frequency_penalty = ruled(self.frequency_penalty, Rule::of(&spec.frequency_penalty));
        self.presence_penalty = ruled(self.presence_penalty, Rule::of(&spec.presence_penalty));
        self.effort = ruled(self.effort, Rule::of(&spec.reasoning.effort));
        self.budget_tokens = ruled(self.budget_tokens, Rule::of(&spec.reasoning.budget_tokens));
        self.summary = ruled(self.summary, Rule::of(&spec.reasoning.summary));
        // The template's limit is the default and the most a caller may ask for.
        if let (Some(m), Slot::Number(b)) = (spec.max_output_tokens, &mut self.max_output_tokens) {
            b.max = smaller(b.max, Some(f64::from(m)));
            b.default = b.max;
        }
        self
    }

    /// The JSON Schema of a request to this target, in the vocabulary of `spelling`. `name` is
    /// what the request's `model` must say.
    pub fn render(&self, name: &str, spelling: Spelling) -> Value {
        let mut p = Map::new();
        p.insert("model".into(), json!({ "const": name }));
        match self.kind {
            ModelKind::Chat => match spelling {
                Spelling::Native => self.native(&mut p),
                Spelling::OpenAiChat => self.openai(&mut p),
                Spelling::Anthropic => self.anthropic(&mut p),
            },
            kind => other_kind(kind, &mut p),
        }
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": name,
            "type": "object",
            "properties": p,
        })
    }

    fn reasoning_off(&self) -> bool {
        matches!(&self.effort, Slot::Choice { values, .. } if values.iter().any(|v| v == "none"))
    }

    fn tool_choice_words(&self, forced: &str) -> Vec<&'static str> {
        let mut words = vec!["auto"];
        if self.tool_choice_none {
            words.push("none");
        }
        if self.tool_choice_forced {
            words.push(if forced == "any" { "any" } else { "required" });
        }
        words
    }

    fn native(&self, p: &mut Map<String, Value>) {
        p.insert("max_output_tokens".into(), slot(&self.max_output_tokens));
        self.sampling_into(p, true);
        let reasoning = [&self.effort, &self.budget_tokens, &self.summary];
        p.insert(
            "reasoning".into(),
            if reasoning.iter().all(|s| **s == Slot::Refused) {
                Value::Bool(false)
            } else {
                json!({ "type": "object", "additionalProperties": false, "properties": {
                    "effort": slot(&self.effort),
                    "budget_tokens": slot(&self.budget_tokens),
                    "summary": slot(&self.summary),
                } })
            },
        );
        let mut types = vec!["text"];
        let mut format = json!({ "type": "object", "required": ["type"], "properties": {} });
        if self.schema != SchemaOutput::None {
            types.push("json_schema");
            format["properties"] = json!({ "name": { "type": "string" }, "schema": { "type": "object" }, "strict": { "type": "boolean" } });
            self.mark_emulated(&mut format);
        }
        format["properties"]["type"] = json!({ "enum": types });
        p.insert("response_format".into(), format);
        let named = json!({ "type": "object", "required": ["tool"], "properties": { "tool": { "type": "string" } } });
        self.tools_into(p, named);
        p.insert("stop".into(), if self.stop { strings() } else { Value::Bool(false) });
    }

    fn openai(&self, p: &mut Map<String, Value>) {
        let max = slot(&self.max_output_tokens);
        p.insert("max_tokens".into(), max.clone());
        p.insert("max_completion_tokens".into(), max);
        self.sampling_into(p, true);
        p.insert("reasoning_effort".into(), slot(&self.effort));
        let mut types = vec!["text"];
        if self.json_mode {
            types.push("json_object");
        }
        let mut format = json!({ "type": "object", "required": ["type"], "properties": { "type": { "enum": [] } } });
        if self.schema != SchemaOutput::None {
            types.push("json_schema");
            format["properties"]["json_schema"] = json!({ "type": "object", "required": ["name", "schema"], "properties": {
                "name": { "type": "string" }, "schema": { "type": "object" }, "strict": { "type": "boolean" } } });
            self.mark_emulated(&mut format);
        }
        format["properties"]["type"] = json!({ "enum": types });
        p.insert("response_format".into(), format);
        let named = json!({ "type": "object", "required": ["type", "function"], "properties": {
            "type": { "const": "function" },
            "function": { "type": "object", "required": ["name"], "properties": { "name": { "type": "string" } } } } });
        self.tools_into(p, named);
        p.insert("stop".into(), if self.stop { strings() } else { Value::Bool(false) });
    }

    fn anthropic(&self, p: &mut Map<String, Value>) {
        p.insert("max_tokens".into(), slot(&self.max_output_tokens));
        // Anthropic's Messages API has no penalties.
        self.sampling_into(p, false);
        let mut thinking = Vec::new();
        if self.budget_tokens != Slot::Refused {
            thinking.push("enabled");
        }
        if self.adaptive {
            thinking.push("adaptive");
        }
        if self.reasoning_off() {
            thinking.push("disabled");
        }
        p.insert(
            "thinking".into(),
            if thinking.is_empty() {
                Value::Bool(false)
            } else {
                json!({ "type": "object", "required": ["type"], "properties": {
                    "type": { "enum": thinking }, "budget_tokens": slot(&self.budget_tokens) } })
            },
        );
        // Reasoning is switched off with `thinking`, not with an effort.
        let effort = match &self.effort {
            Slot::Choice { values, default } => {
                let values: Vec<String> = values.iter().filter(|v| *v != "none").cloned().collect();
                if values.is_empty() { Slot::Refused } else { Slot::Choice { default: default.clone(), values } }
            }
            other => other.clone(),
        };
        let mut format = Value::Bool(false);
        if self.schema != SchemaOutput::None {
            format = json!({ "type": "object", "required": ["type", "schema"], "properties": {
                "type": { "const": "json_schema" }, "schema": { "type": "object" } } });
            self.mark_emulated(&mut format);
        }
        p.insert(
            "output_config".into(),
            json!({ "type": "object", "properties": { "effort": slot(&effort), "format": format } }),
        );
        if self.tools {
            let mut words = self.tool_choice_words("any");
            if self.tool_choice_forced {
                words.push("tool");
            }
            p.insert("tools".into(), json!({ "type": "array", "items": { "type": "object" } }));
            p.insert(
                "tool_choice".into(),
                json!({ "type": "object", "required": ["type"], "properties": {
                    "type": { "enum": words }, "name": { "type": "string" } } }),
            );
        } else {
            p.insert("tools".into(), Value::Bool(false));
            p.insert("tool_choice".into(), Value::Bool(false));
        }
        p.insert(
            "stop_sequences".into(),
            if self.stop { json!({ "type": "array", "items": { "type": "string" } }) } else { Value::Bool(false) },
        );
    }

    fn sampling_into(&self, p: &mut Map<String, Value>, penalties: bool) {
        p.insert("temperature".into(), slot(&self.temperature));
        p.insert("top_p".into(), slot(&self.top_p));
        p.insert("top_k".into(), slot(&self.top_k));
        if penalties {
            p.insert("frequency_penalty".into(), slot(&self.frequency_penalty));
            p.insert("presence_penalty".into(), slot(&self.presence_penalty));
        }
    }

    /// `tools` and `tool_choice`, with `named` the dialect's form of naming one tool.
    fn tools_into(&self, p: &mut Map<String, Value>, named: Value) {
        if !self.tools {
            p.insert("tools".into(), Value::Bool(false));
            p.insert("tool_choice".into(), Value::Bool(false));
            return;
        }
        p.insert("tools".into(), json!({ "type": "array", "items": { "type": "object" } }));
        let words = json!({ "enum": self.tool_choice_words("required") });
        p.insert(
            "tool_choice".into(),
            if self.tool_choice_forced { json!({ "anyOf": [words, named] }) } else { words },
        );
    }

    fn mark_emulated(&self, format: &mut Value) {
        if self.schema == SchemaOutput::Emulated {
            // The gateway answers in the provider's JSON mode and checks the answer.
            format["x-gw-emulated"] = json!(true);
        }
    }
}

fn strings() -> Value {
    json!({ "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] })
}

fn number(v: f64, integer: bool) -> Value {
    if integer && v.fract() == 0.0 && v.abs() < 9.0e15 { json!(v as i64) } else { json!(v) }
}

fn slot(s: &Slot) -> Value {
    match s {
        Slot::Refused => Value::Bool(false),
        Slot::Number(b) => {
            let mut v = json!({ "type": if b.integer { "integer" } else { "number" } });
            if let Some(m) = b.min {
                v["minimum"] = number(m, b.integer);
            }
            if let Some(m) = b.max {
                v["maximum"] = number(m, b.integer);
            }
            if let Some(d) = b.default {
                v["default"] = number(d, b.integer);
            }
            v
        }
        Slot::Choice { values, default } => {
            let mut v = json!({ "type": "string", "enum": values });
            if let Some(d) = default {
                v["default"] = json!(d);
            }
            v
        }
        Slot::Flag { default } => {
            let mut v = json!({ "type": "boolean" });
            if let Some(d) = default {
                v["default"] = json!(d);
            }
            v
        }
        Slot::Locked(value) => json!({ "const": value, "readOnly": true }),
    }
}

/// The members a request for a model of another kind than chat may carry, as the gateway's
/// endpoint for that kind reads them.
fn other_kind(kind: ModelKind, p: &mut Map<String, Value>) {
    let more = match kind {
        ModelKind::Embedding => json!({
            "dimensions": { "type": "integer", "minimum": 1 },
            "encoding_format": { "enum": ["float", "base64"] },
        }),
        ModelKind::Rerank => json!({
            "query": { "type": "string" },
            "documents": { "type": "array" },
            "top_n": { "type": "integer", "minimum": 1 },
            "return_documents": { "type": "boolean" },
            "max_tokens_per_doc": { "type": "integer", "minimum": 1 },
        }),
        ModelKind::Image => json!({
            "prompt": { "type": "string" },
            "n": { "type": "integer", "minimum": 1 },
            "size": { "type": "string" },
            "quality": { "type": "string" },
            "output_format": { "type": "string" },
        }),
        ModelKind::Transcription => json!({
            "language": { "type": "string" },
            "prompt": { "type": "string" },
            "response_format": { "type": "string" },
        }),
        ModelKind::Speech => json!({ "input": { "type": "string" }, "voice": { "type": "string" } }),
        ModelKind::Chat | ModelKind::Decision | ModelKind::Realtime => json!({}),
    };
    if let Value::Object(more) = more {
        p.extend(more);
    }
}

/// The schema of a request to a model, as served to a workspace with `terms`.
pub fn model_schema(t: &SchemaTarget<'_>, spelling: Spelling) -> Value {
    Accepted::of(t).render(&t.model.name, spelling)
}

/// The schema of a request to a template (`@name`): what every model of its pool accepts, under
/// the template's rules.
pub fn template_schema(name: &str, spec: &TemplateSpec, pool: &[SchemaTarget<'_>], spelling: Spelling) -> Value {
    let reference = format!("@{name}");
    match Accepted::pool(pool) {
        Some(a) => a.with_spec(spec).render(&reference, spelling),
        None => {
            json!({ "$schema": "https://json-schema.org/draft/2020-12/schema", "title": reference, "type": "object" })
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::dataplane::managed::Wire;
    use crate::dataplane::shape;
    use crate::dataplane::translate::rule_out;
    use crate::extract::Dialect;
    use crate::json::{scan, validate};
    use crate::model::repo::capability_presets;
    use crate::model::{Family, Param, ReasoningSpec, UnsupportedParams};
    use crate::params::{CallerParams, MergeContext, merge};

    const SPELLINGS: [Spelling; 3] = [Spelling::Native, Spelling::OpenAiChat, Spelling::Anthropic];
    const EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

    /// The chat presets, each as a model with an output limit.
    fn models() -> Vec<(Model, Family)> {
        capability_presets()
            .into_iter()
            .filter(|p| crate::model::repo::suggestion(p.family, p.name, None, None, None).0 == ModelKind::Chat)
            .filter(|p| !["embedding", "speech", "image", "transcription", "rerank", "realtime"].contains(&p.name))
            .map(|p| {
                let m: Model = serde_json::from_value(json!({
                    "name": format!("p/{}", p.name), "provider_id": "prv", "upstream_id": p.name,
                    "capabilities": p.capabilities, "max_output_tokens": 8192
                }))
                .unwrap();
                (m, p.family)
            })
            .collect()
    }

    fn wire(family: Family) -> Wire {
        match family {
            Family::Anthropic => Wire::Anthropic,
            Family::Gemini => Wire::Gemini,
            _ => Wire::OpenAi,
        }
    }

    fn terms() -> GrantTerms {
        GrantTerms { max_output_tokens: Some(4096), max_reasoning_tokens: Some(16000), ..GrantTerms::open() }
    }

    /// Requests that each set one governed parameter, in a dialect's names.
    fn candidates(spelling: Spelling) -> Vec<Value> {
        let mut out = Vec::new();
        let sampling = |out: &mut Vec<Value>, penalties: bool| {
            for v in [-1.0, 0.0, 0.7, 1.0, 1.5, 3.0] {
                out.push(json!({ "temperature": v }));
                out.push(json!({ "top_p": v }));
                if penalties {
                    out.push(json!({ "frequency_penalty": v }));
                    out.push(json!({ "presence_penalty": v }));
                }
            }
            out.push(json!({ "top_k": 40 }));
        };
        match spelling {
            Spelling::Native => {
                for v in [1, 1000, 100_000] {
                    out.push(json!({ "max_output_tokens": v }));
                }
                sampling(&mut out, true);
                for e in EFFORTS {
                    out.push(json!({ "reasoning": { "effort": e } }));
                }
                for b in [100, 2048, 100_000] {
                    out.push(json!({ "reasoning": { "budget_tokens": b } }));
                }
            }
            Spelling::OpenAiChat => {
                for v in [1, 1000, 100_000] {
                    out.push(json!({ "max_tokens": v }));
                    out.push(json!({ "max_completion_tokens": v }));
                }
                sampling(&mut out, true);
                for e in EFFORTS {
                    out.push(json!({ "reasoning_effort": e }));
                }
            }
            Spelling::Anthropic => {
                for v in [1, 1000, 100_000] {
                    out.push(json!({ "max_tokens": v }));
                }
                sampling(&mut out, false);
                for e in EFFORTS {
                    out.push(json!({ "output_config": { "effort": e } }));
                }
                for b in [100, 2048, 100_000] {
                    out.push(json!({ "thinking": { "type": "enabled", "budget_tokens": b } }));
                }
                out.push(json!({ "thinking": { "type": "disabled" } }));
            }
        }
        out
    }

    fn body(spelling: Spelling, name: &str, params: &Value) -> Value {
        let mut b = json!({ "model": name, "messages": [{ "role": "user", "content": "hi" }] });
        if spelling == Spelling::Anthropic {
            b["max_tokens"] = json!(100);
        }
        for (k, v) in params.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    }

    /// The governed parameters of a request, read as the request path reads them.
    fn caller(spelling: Spelling, body: &Value) -> CallerParams {
        let bytes = serde_json::to_vec(body).unwrap();
        match spelling {
            Spelling::Native => {
                let p = pacod_gw_dialects::native::parse_request(&bytes).unwrap().params;
                CallerParams {
                    max_output_tokens: p.max_output_tokens,
                    temperature: p.temperature,
                    top_p: p.top_p,
                    top_k: p.top_k,
                    frequency_penalty: p.frequency_penalty,
                    presence_penalty: p.presence_penalty,
                    reasoning_effort: p.reasoning.effort,
                    reasoning_budget: p.reasoning.budget_tokens,
                }
            }
            Spelling::OpenAiChat => shape::read(Some(Dialect::OpenAiChat), &bytes, &scan(&bytes).unwrap()).params,
            Spelling::Anthropic => shape::read(Some(Dialect::Anthropic), &bytes, &scan(&bytes).unwrap()).params,
        }
    }

    /// What the merge drops and changes, for one model.
    fn merged(m: &Model, spec: Option<&TemplateSpec>, c: &CallerParams) -> (Vec<String>, Vec<String>) {
        let cx = MergeContext {
            spec,
            capabilities: &m.capabilities,
            terms: terms(),
            model_max_output: m.max_output_tokens,
            policy: UnsupportedParams::Drop,
            filter_caller: true,
        };
        let d = merge(&cx, c).unwrap();
        (d.dropped, d.adjusted)
    }

    /// The invariant: a request that conforms to the schema loses none of the caller's
    /// parameters on any model of the target, and has none of them changed. What the target
    /// does on its own (a template default the model refuses) is the same with or without
    /// the caller's parameters.
    fn holds(name: &str, schema: &Value, members: &[&Model], spec: Option<&TemplateSpec>, spelling: Spelling) -> usize {
        let base = body(spelling, name, &json!({}));
        assert!(
            validate(schema, &base).is_empty(),
            "{name} {spelling:?}: the bare request: {}",
            validate(schema, &base)[0]
        );
        let mut valid = 0;
        for params in candidates(spelling) {
            let request = body(spelling, name, &params);
            if !validate(schema, &request).is_empty() {
                continue;
            }
            valid += 1;
            for m in members {
                let before = merged(m, spec, &caller(spelling, &base));
                let after = merged(m, spec, &caller(spelling, &request));
                assert_eq!(after, before, "{name} {spelling:?} on {}: {params} conforms but was changed", m.name);
            }
        }
        valid
    }

    #[test]
    fn a_conforming_request_keeps_every_parameter_on_every_preset() {
        for (m, family) in models() {
            let target = SchemaTarget { model: &m, provider_drop_fields: &[], terms: terms() };
            for spelling in SPELLINGS {
                let schema = model_schema(&target, spelling);
                let valid = holds(&m.name, &schema, &[&m], None, spelling);
                assert!(valid > 0, "{} {spelling:?}: nothing conforms", m.name);
            }
            // The features the capability profile rules out, on the native interface.
            let schema = model_schema(&target, Spelling::Native);
            let tools = json!([{ "type": "function", "name": "lookup", "parameters": { "type": "object" } }]);
            let features = [
                json!({ "response_format": { "type": "json_schema", "name": "a", "schema": { "type": "object" } } }),
                json!({ "tools": tools, "tool_choice": "none" }),
                json!({ "tools": tools, "tool_choice": "required" }),
                json!({ "tools": tools, "tool_choice": { "tool": "lookup" } }),
            ];
            for f in features {
                let request = body(Spelling::Native, &m.name, &f);
                if !validate(&schema, &request).is_empty() {
                    continue;
                }
                let mut req = pacod_gw_dialects::native::parse_request(&serde_json::to_vec(&request).unwrap()).unwrap();
                let ruled = rule_out(&m, wire(family), &mut req, UnsupportedParams::Drop).unwrap();
                assert!(ruled.dropped.is_empty(), "{}: {f} conforms but lost {:?}", m.name, ruled.dropped);
            }
        }
    }

    #[test]
    fn what_the_schema_forbids_is_dropped() {
        for (m, _) in models() {
            let target = SchemaTarget { model: &m, provider_drop_fields: &[], terms: terms() };
            let schema = model_schema(&target, Spelling::Native);
            let p = &schema["properties"];
            for name in ["temperature", "top_p", "top_k", "frequency_penalty", "presence_penalty"] {
                if p[name] != json!(false) {
                    continue;
                }
                let c = caller(Spelling::Native, &body(Spelling::Native, &m.name, &json!({ name: 0.5 })));
                let (dropped, _) = merged(&m, None, &c);
                assert!(dropped.iter().any(|d| d == name), "{}: {name} is false in the schema but kept", m.name);
            }
            let effort = &p["reasoning"]["properties"]["effort"];
            for e in EFFORTS.iter().filter(|e| **e != "none") {
                let listed = effort["enum"].as_array().is_some_and(|l| l.iter().any(|x| x == e));
                if listed {
                    continue;
                }
                let c = caller(
                    Spelling::Native,
                    &body(Spelling::Native, &m.name, &json!({ "reasoning": { "effort": e } })),
                );
                let (dropped, adjusted) = merged(&m, None, &c);
                assert!(
                    !dropped.is_empty() || !adjusted.is_empty(),
                    "{}: the effort {e} is not offered but kept",
                    m.name
                );
            }
        }
    }

    fn preset_model(preset: &str) -> Model {
        models().into_iter().map(|(m, _)| m).find(|m| m.upstream_id == preset).unwrap()
    }

    #[test]
    fn a_template_offers_what_its_whole_pool_takes_under_its_rules() {
        let spec = TemplateSpec {
            max_output_tokens: Some(2000),
            temperature: Some(Param::default_value(0.3)),
            top_p: Some(Param::locked(0.9)),
            frequency_penalty: Some(Param::omitted()),
            reasoning: ReasoningSpec { effort: Some(Param::locked("low".into())), ..Default::default() },
            ..Default::default()
        };
        for pool in [["anthropic-sonnet-5-5", "openai-gpt-5-6"], ["deepseek", "glm-5"], ["openai-chat", "gemini-3-pro"]]
        {
            let members: Vec<Model> = pool.iter().map(|p| preset_model(p)).collect();
            let targets: Vec<SchemaTarget<'_>> =
                members.iter().map(|m| SchemaTarget { model: m, provider_drop_fields: &[], terms: terms() }).collect();
            for spelling in SPELLINGS {
                let schema = template_schema("t", &spec, &targets, spelling);
                let refs: Vec<&Model> = members.iter().collect();
                holds("@t", &schema, &refs, Some(&spec), spelling);
            }
            let schema = template_schema("t", &spec, &targets, Spelling::Native);
            let p = &schema["properties"];
            assert_eq!(p["max_output_tokens"]["maximum"], 2000, "{pool:?}");
            assert_eq!(p["max_output_tokens"]["default"], 2000, "{pool:?}");
            assert_eq!(p["frequency_penalty"], json!(false), "omitted: {pool:?}");
        }
        // Where the whole pool takes them, a default is a default and a locked value is fixed.
        let members = [preset_model("openai-chat"), preset_model("gemini-3-pro")];
        let targets: Vec<SchemaTarget<'_>> =
            members.iter().map(|m| SchemaTarget { model: m, provider_drop_fields: &[], terms: terms() }).collect();
        let schema = template_schema("t", &spec, &targets, Spelling::Native);
        let p = &schema["properties"];
        assert_eq!(p["temperature"]["default"], 0.3);
        assert_eq!(p["top_p"], json!({ "const": 0.9, "readOnly": true }));
        assert_eq!(p["model"], json!({ "const": "@t" }));
        // A sampling parameter one member refuses is refused for the template.
        let members = [preset_model("anthropic-sonnet-5-5"), preset_model("openai-gpt-5-6")];
        let targets: Vec<SchemaTarget<'_>> =
            members.iter().map(|m| SchemaTarget { model: m, provider_drop_fields: &[], terms: terms() }).collect();
        let schema = template_schema("t", &spec, &targets, Spelling::Native);
        assert_eq!(schema["properties"]["temperature"], json!(false));
        assert_eq!(
            schema["properties"]["reasoning"]["properties"]["effort"],
            json!({ "const": "low", "readOnly": true })
        );
    }

    #[test]
    fn a_model_schema_says_what_the_profile_and_the_grant_say() {
        let deepseek = preset_model("deepseek");
        let t = SchemaTarget { model: &deepseek, provider_drop_fields: &[], terms: terms() };
        let s = model_schema(&t, Spelling::Native);
        let p = &s["properties"];
        assert_eq!(s["$schema"], "https://json-schema.org/draft/2020-12/schema");
        assert_eq!(
            p["max_output_tokens"],
            json!({ "type": "integer", "minimum": 1, "maximum": 4096 }),
            "the grant's ceiling"
        );
        assert_eq!(p["top_k"], json!(false));
        assert_eq!(p["reasoning"]["properties"]["effort"]["enum"], json!(["none", "low", "high", "max"]));
        assert_eq!(p["reasoning"]["properties"]["budget_tokens"], json!(false));
        assert_eq!(p["response_format"]["x-gw-emulated"], true);
        assert_eq!(p["tool_choice"]["enum"], json!(["auto", "none"]), "no forced tool choice while thinking");
        let o = model_schema(&t, Spelling::OpenAiChat);
        assert_eq!(
            o["properties"]["response_format"]["properties"]["type"]["enum"],
            json!(["text", "json_object", "json_schema"])
        );
        assert_eq!(o["properties"]["reasoning_effort"]["enum"], json!(["none", "low", "high", "max"]));

        let haiku = preset_model("anthropic-budget");
        let t = SchemaTarget { model: &haiku, provider_drop_fields: &[], terms: terms() };
        let a = model_schema(&t, Spelling::Anthropic);
        let p = &a["properties"];
        assert_eq!(p["temperature"], json!({ "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 1.0 }));
        assert_eq!(p["thinking"]["properties"]["type"]["enum"], json!(["enabled", "disabled"]));
        assert_eq!(
            p["thinking"]["properties"]["budget_tokens"],
            json!({ "type": "integer", "minimum": 1024, "maximum": 16000 })
        );
        assert!(p.get("frequency_penalty").is_none(), "Anthropic's API has no penalties");

        // What the provider or the model drops from every request.
        let xai = preset_model("xai-reasoning");
        let t = SchemaTarget { model: &xai, provider_drop_fields: &["temperature".to_owned()], terms: terms() };
        let p = model_schema(&t, Spelling::OpenAiChat)["properties"].clone();
        assert_eq!((&p["stop"], &p["temperature"]), (&json!(false), &json!(false)));
    }

    #[test]
    fn other_kinds_name_what_their_endpoints_read() {
        let m: Model = serde_json::from_value(
            json!({ "name": "openai/e", "provider_id": "prv", "upstream_id": "e", "kind": "embedding" }),
        )
        .unwrap();
        let t = SchemaTarget { model: &m, provider_drop_fields: &[], terms: GrantTerms::open() };
        let s = model_schema(&t, Spelling::OpenAiChat);
        assert_eq!(s["properties"]["dimensions"], json!({ "type": "integer", "minimum": 1 }));
        assert!(s["properties"].get("temperature").is_none());
    }
}
