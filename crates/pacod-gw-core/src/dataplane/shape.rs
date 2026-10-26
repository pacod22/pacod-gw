//! Reads the parameters the gateway governs out of a request and writes the merge's decisions
//! back, in each dialect's own field names (concept sections 7.2 and 7.4).
//!
//! Everything here works on the scanned body and a [`Patch`]: the conversation content is
//! never parsed, and members the patch does not name keep their bytes.

use bytes::Bytes;
use serde_json::{Value, json};

use crate::extract::Dialect;
use crate::json::{Kind, Op, Patch, TopLevel, scan};
use crate::model::{Capabilities, MaxTokensField, Quirks, ReasoningParam, ReasoningStyle, ThinkingOff};
use crate::params::{CallerParams, Decision, Decisions};

fn render(v: &Value) -> Bytes {
    Bytes::from(serde_json::to_vec(v).expect("a JSON value always serialises"))
}

/// An object member of the body together with its scan.
pub fn inner<'a>(body: &'a [u8], top: &TopLevel, key: &str) -> Option<(&'a [u8], TopLevel)> {
    let e = top.get(key).filter(|e| e.kind == Kind::Object)?;
    let raw = &body[e.value.clone()];
    scan(raw).ok().map(|t| (raw, t))
}

/// The names of the sampling parameters in a dialect that keeps them in one nested object.
struct Nested {
    object: &'static str,
    max: &'static str,
    temperature: &'static str,
    top_p: &'static str,
    top_k: &'static str,
    frequency_penalty: &'static str,
    presence_penalty: &'static str,
}

/// Gemini's native API (to verify against the live API).
const GEMINI: Nested = Nested {
    object: "generationConfig",
    max: "maxOutputTokens",
    temperature: "temperature",
    top_p: "topP",
    top_k: "topK",
    frequency_penalty: "frequencyPenalty",
    presence_penalty: "presencePenalty",
};

/// Ollama's native API (to verify against the live API).
const OLLAMA: Nested = Nested {
    object: "options",
    max: "num_predict",
    temperature: "temperature",
    top_p: "top_p",
    top_k: "top_k",
    frequency_penalty: "frequency_penalty",
    presence_penalty: "presence_penalty",
};

/// What the caller sent, in dialect-neutral terms.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Read {
    pub params: CallerParams,
    /// OpenAI chat: the field that carried the output-token limit.
    pub max_field: Option<&'static str>,
    /// Anthropic: the `type` of the caller's `thinking` object, when there is one.
    pub thinking_type: Option<String>,
}

/// Reads the governed parameters of a request. `dialect` is `None` for paths the gateway does
/// not model; nothing is read there.
pub fn read(dialect: Option<Dialect>, body: &[u8], top: &TopLevel) -> Read {
    match dialect {
        Some(Dialect::OpenAiChat) => {
            let (max, max_field) = match (top.u64(body, "max_completion_tokens"), top.u64(body, "max_tokens")) {
                (Some(v), _) => (Some(v), Some("max_completion_tokens")),
                (None, Some(v)) => (Some(v), Some("max_tokens")),
                (None, None) => (None, None),
            };
            Read {
                thinking_type: None,
                params: CallerParams {
                    max_output_tokens: max,
                    temperature: top.f64(body, "temperature"),
                    top_p: top.f64(body, "top_p"),
                    top_k: top.f64(body, "top_k"),
                    frequency_penalty: top.f64(body, "frequency_penalty"),
                    presence_penalty: top.f64(body, "presence_penalty"),
                    reasoning_effort: top.str(body, "reasoning_effort"),
                    reasoning_budget: None,
                },
                max_field,
            }
        }
        Some(Dialect::OpenAiResponses) => Read {
            params: CallerParams {
                max_output_tokens: top.u64(body, "max_output_tokens"),
                temperature: top.f64(body, "temperature"),
                top_p: top.f64(body, "top_p"),
                reasoning_effort: inner(body, top, "reasoning").and_then(|(raw, t)| t.str(raw, "effort")),
                ..Default::default()
            },
            ..Default::default()
        },
        Some(Dialect::Anthropic) => {
            let thinking = inner(body, top, "thinking");
            let thinking_type = thinking.as_ref().and_then(|(raw, t)| t.str(raw, "type"));
            let off = thinking_type.as_deref() == Some("disabled");
            Read {
                thinking_type,
                params: CallerParams {
                    max_output_tokens: top.u64(body, "max_tokens"),
                    temperature: top.f64(body, "temperature"),
                    top_p: top.f64(body, "top_p"),
                    top_k: top.f64(body, "top_k"),
                    reasoning_budget: thinking.as_ref().and_then(|(raw, t)| t.u64(raw, "budget_tokens")),
                    reasoning_effort: inner(body, top, "output_config")
                        .and_then(|(raw, t)| t.str(raw, "effort"))
                        .or_else(|| off.then(|| "none".to_owned())),
                    ..Default::default()
                },
                max_field: None,
            }
        }
        Some(Dialect::Gemini) => Read { params: read_gemini(body, top), ..Default::default() },
        Some(Dialect::Ollama) => Read { params: read_nested(body, top, &OLLAMA), ..Default::default() },
        Some(Dialect::OpenAiEmbeddings | Dialect::GeminiEmbed | Dialect::SystemOne | Dialect::OpenAiImages) | None => {
            Read::default()
        }
    }
}

/// Gemini's parameters, and its reasoning: a level (Gemini 3) or a token budget, 0 for none.
fn read_gemini(body: &[u8], top: &TopLevel) -> CallerParams {
    let mut params = read_nested(body, top, &GEMINI);
    let thinking = inner(body, top, GEMINI.object).and_then(|(raw, t)| inner(raw, &t, "thinkingConfig"));
    if let Some((raw, t)) = thinking {
        params.reasoning_effort = t.str(raw, "thinkingLevel").map(|l| l.to_ascii_lowercase());
        match t.raw(raw, "thinkingBudget").and_then(|b| std::str::from_utf8(b).ok()?.parse::<i64>().ok()) {
            Some(0) => params.reasoning_effort = Some("none".into()),
            Some(b) if b > 0 => params.reasoning_budget = Some(b as u64),
            _ => {}
        }
    }
    params
}

/// Gemini's API takes `generation_config` as well as `generationConfig`, and either spelling
/// of their members. The gateway reads and writes one: the body with a single camelCase
/// `generationConfig` (its `thinkingConfig` too), or `None` when it already has no other.
pub fn gemini_one_spelling(body: &Bytes, top: &TopLevel) -> Result<Option<Bytes>, crate::json::ScanError> {
    fn camel_object(v: &Value, merged: &mut serde_json::Map<String, Value>) {
        for (k, v) in v.as_object().into_iter().flatten() {
            let key = pacod_gw_dialects::gemini::camel(k);
            let v = if key == "thinkingConfig" {
                let mut thinking = merged.get(&key).and_then(Value::as_object).cloned().unwrap_or_default();
                camel_object(v, &mut thinking);
                Value::Object(thinking)
            } else {
                v.clone()
            };
            merged.insert(key, v);
        }
    }
    let raw = |key: &str| top.raw(body, key).and_then(|r| serde_json::from_slice::<Value>(r).ok());
    let (snake, camel) = (raw("generation_config"), raw("generationConfig"));
    let spelled_twice = |v: &Option<Value>| {
        v.as_ref().and_then(Value::as_object).is_some_and(|o| {
            o.iter().any(|(k, v)| {
                k.contains('_')
                    || (k == "thinkingConfig" && v.as_object().is_some_and(|t| t.keys().any(|k| k.contains('_'))))
            })
        })
    };
    if snake.is_none() && !spelled_twice(&camel) {
        return Ok(None);
    }
    let mut merged = serde_json::Map::new();
    // The camelCase spelling is read last: where both name a member, it is the one kept.
    for v in [&snake, &camel].into_iter().flatten() {
        camel_object(v, &mut merged);
    }
    let mut patch = Patch::new();
    patch.remove("generation_config");
    patch.set("generationConfig", &Value::Object(merged));
    Ok(Some(crate::json::apply(body, top, &patch)?.to_bytes()))
}

fn read_nested(body: &[u8], top: &TopLevel, n: &Nested) -> CallerParams {
    let Some((raw, t)) = inner(body, top, n.object) else { return CallerParams::default() };
    CallerParams {
        max_output_tokens: t.u64(raw, n.max),
        temperature: t.f64(raw, n.temperature),
        top_p: t.f64(raw, n.top_p),
        top_k: t.f64(raw, n.top_k),
        frequency_penalty: t.f64(raw, n.frequency_penalty),
        presence_penalty: t.f64(raw, n.presence_penalty),
        ..Default::default()
    }
}

/// What a write needs to know about the target.
pub struct Target<'a> {
    pub capabilities: &'a Capabilities,
    pub quirks: &'a Quirks,
    /// Managed mode rewrites what the caller sent into the form the provider takes (the
    /// token-limit field, the reasoning control). Proxy mode writes only what a template
    /// decided and leaves the caller's own fields alone.
    pub managed: bool,
    /// Let the provider place the prompt-cache marker (Anthropic models).
    pub cache_auto: bool,
}

/// Remarks a write adds to those of the merge.
#[derive(Debug, Default, PartialEq)]
pub struct Notes {
    pub dropped: Vec<String>,
    pub adjusted: Vec<String>,
}

fn apply_number<T: Copy + Into<Value>>(patch: &mut Patch, key: &str, d: &Decision<T>) {
    match d {
        Decision::Keep => {}
        Decision::Set(v) => {
            patch.set(key, &(*v).into());
        }
        Decision::Remove => {
            patch.remove(key);
        }
    }
}

fn apply_nested<T: Copy + Into<Value>>(patch: &mut Patch, object: &str, key: &str, d: &Decision<T>) {
    match d {
        Decision::Keep => {}
        Decision::Set(v) => {
            patch.nested(object, key, Op::Set(render(&(*v).into())));
        }
        Decision::Remove => {
            patch.nested(object, key, Op::Remove);
        }
    }
}

/// Writes the decisions into `patch` in the dialect's field names.
pub fn write(
    dialect: Option<Dialect>,
    patch: &mut Patch,
    d: &Decisions,
    sent: &Read,
    t: &Target<'_>,
    notes: &mut Notes,
) {
    match dialect {
        Some(Dialect::OpenAiChat) => write_openai_chat(patch, d, sent, t, notes),
        Some(Dialect::OpenAiResponses) => {
            apply_number(patch, "max_output_tokens", &d.max_output_tokens);
            apply_number(patch, "temperature", &d.temperature);
            apply_number(patch, "top_p", &d.top_p);
            match &d.reasoning_effort {
                Decision::Keep => {}
                Decision::Set(e) => {
                    patch.nested("reasoning", "effort", Op::Set(render(&json!(e))));
                }
                Decision::Remove => {
                    patch.nested("reasoning", "effort", Op::Remove);
                }
            }
        }
        Some(Dialect::Anthropic) => write_anthropic(patch, d, sent, t, notes),
        Some(Dialect::Gemini) => write_nested(patch, d, &GEMINI),
        Some(Dialect::Ollama) => write_nested(patch, d, &OLLAMA),
        Some(Dialect::OpenAiEmbeddings | Dialect::GeminiEmbed | Dialect::SystemOne | Dialect::OpenAiImages) | None => {}
    }
}

fn write_nested(patch: &mut Patch, d: &Decisions, n: &Nested) {
    apply_nested(patch, n.object, n.max, &d.max_output_tokens);
    apply_nested(patch, n.object, n.temperature, &d.temperature);
    apply_nested(patch, n.object, n.top_p, &d.top_p);
    apply_nested(patch, n.object, n.top_k, &d.top_k);
    apply_nested(patch, n.object, n.frequency_penalty, &d.frequency_penalty);
    apply_nested(patch, n.object, n.presence_penalty, &d.presence_penalty);
}

fn lowest_effort(caps: &Capabilities) -> Option<&String> {
    caps.reasoning_efforts.iter().find(|e| e.as_str() != "none")
}

fn write_openai_chat(patch: &mut Patch, d: &Decisions, sent: &Read, t: &Target<'_>, notes: &mut Notes) {
    apply_number(patch, "temperature", &d.temperature);
    apply_number(patch, "top_p", &d.top_p);
    apply_number(patch, "top_k", &d.top_k);
    apply_number(patch, "frequency_penalty", &d.frequency_penalty);
    apply_number(patch, "presence_penalty", &d.presence_penalty);

    // The output-token limit goes into the field this model takes.
    let (field, other) = match t.capabilities.max_tokens_field {
        MaxTokensField::MaxTokens => ("max_tokens", "max_completion_tokens"),
        MaxTokensField::MaxCompletionTokens => ("max_completion_tokens", "max_tokens"),
    };
    let value = match &d.max_output_tokens {
        Decision::Keep => sent.params.max_output_tokens,
        Decision::Set(v) => Some(*v),
        Decision::Remove => None,
    };
    let misplaced = t.managed && sent.max_field.is_some_and(|f| f != field);
    if let Some(v) = value
        && (!d.max_output_tokens.is_keep() || misplaced)
    {
        patch.set(field, &json!(v));
        patch.remove(other);
    }

    // Reasoning, in the form this endpoint takes.
    let caps = t.capabilities;
    let effort = match &d.reasoning_effort {
        Decision::Keep => sent.params.reasoning_effort.clone(),
        Decision::Set(e) => Some(e.clone()),
        Decision::Remove => None,
    };
    let decided = !d.reasoning_effort.is_keep();
    match caps.reasoning_param_or(t.quirks.reasoning_param) {
        ReasoningParam::ReasoningEffort => match effort.as_deref() {
            Some("none") if decided || t.managed => {
                if caps.reasoning_efforts.iter().any(|e| e == "none") {
                    patch.set("reasoning_effort", &json!("none"));
                } else if let Some(low) = lowest_effort(caps).filter(|_| caps.reasoning != ReasoningStyle::None) {
                    notes
                        .adjusted
                        .push(format!("reasoning cannot be switched off on this model; the effort {low} was used"));
                    patch.set("reasoning_effort", &json!(low));
                } else {
                    patch.remove("reasoning_effort");
                }
            }
            Some(e) if decided => {
                patch.set("reasoning_effort", &json!(e));
            }
            None if decided => {
                patch.remove("reasoning_effort");
            }
            _ => {}
        },
        ReasoningParam::ThinkingToggle => {
            if decided || t.managed {
                // Only the switch: other members of the provider's `thinking` object (GLM's
                // `clear_thinking`, Kimi's `keep`) stay as they are.
                if let Some(e) = effort.as_deref() {
                    let switch = if e == "none" { "disabled" } else { "enabled" };
                    patch.nested("thinking", "type", Op::Set(render(&json!(switch))));
                }
                patch.remove("reasoning_effort");
            }
        }
        ReasoningParam::None => {
            if decided || t.managed {
                if effort.is_some() && !d.dropped.iter().any(|p| p == "reasoning.effort") {
                    notes.dropped.push("reasoning.effort".into());
                }
                patch.remove("reasoning_effort");
            }
        }
    }
}

fn write_anthropic(patch: &mut Patch, d: &Decisions, sent: &Read, t: &Target<'_>, notes: &mut Notes) {
    apply_number(patch, "max_tokens", &d.max_output_tokens);
    apply_number(patch, "temperature", &d.temperature);
    apply_number(patch, "top_p", &d.top_p);
    apply_number(patch, "top_k", &d.top_k);

    let caps = t.capabilities;
    // Managed mode also puts the caller's own setting into the form this model takes; proxy
    // mode writes only what a template decided.
    let effort = match &d.reasoning_effort {
        Decision::Keep => sent.params.reasoning_effort.clone(),
        Decision::Set(e) => Some(e.clone()),
        Decision::Remove => None,
    };
    let decided = !d.reasoning_effort.is_keep();
    let off = effort.as_deref() == Some("none") && (decided || t.managed);
    let sent_type = sent.thinking_type.as_deref();

    match caps.reasoning {
        ReasoningStyle::Budget => {
            match &d.reasoning_budget {
                Decision::Set(b) => {
                    patch.set("thinking", &json!({ "type": "enabled", "budget_tokens": b }));
                }
                Decision::Remove => {
                    patch.remove("thinking");
                }
                Decision::Keep => {
                    if t.managed && sent_type == Some("adaptive") {
                        // This model takes a budget, and the caller named none.
                        notes.dropped.push("thinking".into());
                        patch.remove("thinking");
                    }
                }
            }
            if off {
                patch.set("thinking", &json!({ "type": "disabled" }));
            }
            if matches!(d.reasoning_effort, Decision::Remove) {
                patch.nested("output_config", "effort", Op::Remove);
            }
        }
        ReasoningStyle::Adaptive | ReasoningStyle::AlwaysOn => {
            if off {
                match caps.thinking_off {
                    ThinkingOff::Disabled => {
                        patch.set("thinking", &json!({ "type": "disabled" }));
                    }
                    ThinkingOff::BetweenTools => {
                        patch.set("thinking", &json!({ "type": "between_tools" }));
                    }
                    ThinkingOff::Unsupported => {
                        if sent_type == Some("disabled") {
                            patch.set("thinking", &json!({ "type": "adaptive" }));
                        }
                        if let Some(low) = lowest_effort(caps) {
                            notes.adjusted.push(format!(
                                "thinking cannot be switched off on this model; the effort {low} was used"
                            ));
                            patch.nested("output_config", "effort", Op::Set(render(&json!(low))));
                        }
                    }
                }
            } else {
                match &d.reasoning_effort {
                    Decision::Set(e) => {
                        // The caller's own thinking object, with its display setting, is kept.
                        patch.set_if_absent("thinking", &json!({ "type": "adaptive" }));
                        patch.nested("output_config", "effort", Op::Set(render(&json!(e))));
                    }
                    Decision::Remove => {
                        patch.nested("output_config", "effort", Op::Remove);
                    }
                    Decision::Keep => {}
                }
                if (decided || t.managed) && sent_type == Some("enabled") {
                    // A token budget is the older models' control; this one decides itself.
                    notes
                        .adjusted
                        .push("a thinking budget is not taken by this model; adaptive thinking was used".into());
                    patch.set("thinking", &json!({ "type": "adaptive" }));
                }
            }
        }
        ReasoningStyle::None | ReasoningStyle::Effort => {
            if t.managed {
                if sent_type.is_some() {
                    notes.dropped.push("thinking".into());
                    patch.remove("thinking");
                }
                if matches!(d.reasoning_effort, Decision::Remove) {
                    patch.nested("output_config", "effort", Op::Remove);
                }
            }
        }
    }

    if t.cache_auto {
        // Anthropic places the breakpoint itself when the request carries a top-level marker.
        patch.set_if_absent("cache_control", &json!({ "type": "ephemeral" }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::apply;
    use crate::model::repo::capability_presets;
    use crate::model::{Param, ReasoningSpec, TemplateSpec, UnsupportedParams};
    use crate::params::{MergeContext, merge};
    use crate::snapshot::GrantTerms;

    fn caps(preset: &str) -> Capabilities {
        capability_presets().into_iter().find(|p| p.name == preset).unwrap().capabilities
    }

    /// Runs read, merge and write on a body, the way both modes do, and returns the new body.
    fn shaped(
        dialect: Dialect,
        body: &str,
        spec: &TemplateSpec,
        c: &Capabilities,
        quirks: &Quirks,
        managed: bool,
    ) -> (String, Notes, Decisions) {
        let bytes = Bytes::from(body.to_owned());
        let top = scan(&bytes).unwrap();
        let sent = read(Some(dialect), &bytes, &top);
        let cx = MergeContext {
            spec: Some(spec),
            capabilities: c,
            terms: GrantTerms::open(),
            model_max_output: None,
            policy: UnsupportedParams::Drop,
            filter_caller: managed,
        };
        let d = merge(&cx, &sent.params).unwrap();
        let mut patch = Patch::new();
        let mut notes = Notes::default();
        let t = Target {
            capabilities: c,
            quirks,
            managed,
            cache_auto: spec.prompt_cache == crate::model::PromptCacheMode::Auto,
        };
        write(Some(dialect), &mut patch, &d, &sent, &t, &mut notes);
        let out = apply(&bytes, &top, &patch).unwrap().to_bytes();
        (String::from_utf8(out.to_vec()).unwrap(), notes, d)
    }

    #[test]
    fn the_concepts_planner_example_yields_exactly_the_stated_bytes() {
        // Concept appendix B: @planner = claude-haiku-4-5 with a ceiling of 400 output tokens.
        let spec = TemplateSpec { max_output_tokens: Some(400), ..Default::default() };
        let body = "{\"model\":\"@planner\",\"max_tokens\":8000,\n       \"messages\":[{\"role\":\"user\",\"content\":\"Plan the next step.\"}]}";
        let bytes = Bytes::from(body);
        let top = scan(&bytes).unwrap();
        let sent = read(Some(Dialect::Anthropic), &bytes, &top);
        assert_eq!(sent.params.max_output_tokens, Some(8000));
        let c = caps("anthropic-budget");
        let cx = MergeContext {
            spec: Some(&spec),
            capabilities: &c,
            terms: GrantTerms::open(),
            model_max_output: Some(64_000),
            policy: UnsupportedParams::Drop,
            filter_caller: false,
        };
        let d = merge(&cx, &sent.params).unwrap();
        let mut patch = Patch::new();
        patch.set("model", &json!("claude-haiku-4-5"));
        let t = Target { capabilities: &c, quirks: &Quirks::default(), managed: false, cache_auto: false };
        write(Some(Dialect::Anthropic), &mut patch, &d, &sent, &t, &mut Notes::default());
        let out = apply(&bytes, &top, &patch).unwrap().to_bytes();
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "{\"model\":\"claude-haiku-4-5\",\"max_tokens\":400,\n       \"messages\":[{\"role\":\"user\",\"content\":\"Plan the next step.\"}]}"
        );
    }

    #[test]
    fn openai_chat_parameters_are_written_in_the_fields_the_model_takes() {
        let spec = TemplateSpec {
            max_output_tokens: Some(1000),
            temperature: Some(Param::locked(0.2)),
            top_p: Some(Param::omitted()),
            reasoning: ReasoningSpec { effort: Some(Param::default_value("high".into())), ..Default::default() },
            ..Default::default()
        };
        let q = Quirks::default();

        // A reasoning model: the limit moves to max_completion_tokens, sampling is dropped.
        let (out, notes, d) = shaped(
            Dialect::OpenAiChat,
            r#"{"model":"m","max_tokens":5000,"temperature":1,"top_p":0.9,"messages":[]}"#,
            &spec,
            &caps("openai-reasoning"),
            &q,
            true,
        );
        assert_eq!(out, r#"{"model":"m","messages":[],"max_completion_tokens":1000,"reasoning_effort":"high"}"#);
        assert_eq!(d.dropped, ["temperature"]);
        assert_eq!(notes, Notes::default());

        // A plain chat model takes the locked temperature; top_p is omitted; no effort exists.
        let (out, _, d) = shaped(
            Dialect::OpenAiChat,
            r#"{"model":"m","temperature":1,"top_p":0.9,"messages":[]}"#,
            &spec,
            &caps("openai-chat"),
            &q,
            true,
        );
        assert_eq!(out, r#"{"model":"m","temperature":0.2,"messages":[],"max_tokens":1000}"#);
        assert_eq!(d.dropped, ["reasoning.effort"]);

        // Proxy mode leaves the caller's own fields alone, even a misplaced token limit.
        let none = TemplateSpec::default();
        let body = r#"{"model":"m","max_tokens":50,"reasoning_effort":"low","messages":[]}"#;
        let (out, _, _) = shaped(Dialect::OpenAiChat, body, &none, &caps("openai-reasoning"), &q, false);
        assert_eq!(out, body);
    }

    #[test]
    fn reasoning_follows_the_endpoints_own_control() {
        let spec = TemplateSpec {
            reasoning: ReasoningSpec { effort: Some(Param::locked("none".into())), ..Default::default() },
            ..Default::default()
        };
        let c = caps("openai-compatible");
        let body = r#"{"model":"m","reasoning_effort":"high","messages":[]}"#;

        // An on/off switch instead of levels.
        let toggle = Quirks { reasoning_param: ReasoningParam::ThinkingToggle, ..Default::default() };
        let (out, _, _) = shaped(Dialect::OpenAiChat, body, &spec, &c, &toggle, true);
        assert_eq!(out, r#"{"model":"m","messages":[],"thinking":{"type":"disabled"}}"#);
        let on = TemplateSpec::default();
        let (out, _, _) = shaped(Dialect::OpenAiChat, body, &on, &c, &toggle, true);
        assert_eq!(out, r#"{"model":"m","messages":[],"thinking":{"type":"enabled"}}"#);

        // Levels, on a model that cannot switch reasoning off: the lowest level is used and said.
        let (out, notes, _) = shaped(Dialect::OpenAiChat, body, &spec, &c, &Quirks::default(), true);
        assert_eq!(out, r#"{"model":"m","reasoning_effort":"low","messages":[]}"#);
        assert_eq!(notes.adjusted.len(), 1);

        // No reasoning field at all on this endpoint.
        let nothing = Quirks { reasoning_param: ReasoningParam::None, ..Default::default() };
        let (out, notes, _) = shaped(Dialect::OpenAiChat, body, &on, &c, &nothing, true);
        assert_eq!(out, r#"{"model":"m","messages":[]}"#);
        assert_eq!(notes.dropped, ["reasoning.effort"]);
    }

    #[test]
    fn anthropic_thinking_is_written_for_each_style() {
        let q = Quirks::default();
        let effort = |e: &str| TemplateSpec {
            reasoning: ReasoningSpec { effort: Some(Param::locked(e.into())), ..Default::default() },
            ..Default::default()
        };
        let body = r#"{"model":"m","max_tokens":100,"messages":[]}"#;

        let (out, _, _) = shaped(Dialect::Anthropic, body, &effort("high"), &caps("anthropic-adaptive"), &q, false);
        assert_eq!(
            out,
            r#"{"model":"m","max_tokens":100,"messages":[],"thinking":{"type":"adaptive"},"output_config":{"effort":"high"}}"#
        );

        // The caller's thinking object is kept; only the effort is written next to other settings.
        let with = r#"{"model":"m","max_tokens":100,"thinking":{"type":"adaptive","display":"summarized"},"output_config":{"format":{"type":"json_schema"},"effort":"low"},"messages":[]}"#;
        let (out, _, _) = shaped(Dialect::Anthropic, with, &effort("high"), &caps("anthropic-adaptive"), &q, false);
        assert_eq!(
            out,
            r#"{"model":"m","max_tokens":100,"thinking":{"type":"adaptive","display":"summarized"},"output_config":{"format":{"type":"json_schema"},"effort":"high"},"messages":[]}"#
        );

        // Off, in each model's own terms.
        let (out, _, _) = shaped(Dialect::Anthropic, body, &effort("none"), &caps("anthropic-adaptive"), &q, false);
        assert!(out.ends_with(r#""thinking":{"type":"disabled"}}"#), "{out}");
        let (out, _, _) = shaped(Dialect::Anthropic, body, &effort("none"), &caps("anthropic-sonnet-5-5"), &q, false);
        assert!(out.ends_with(r#""thinking":{"type":"between_tools"}}"#), "{out}");
        let (out, notes, _) =
            shaped(Dialect::Anthropic, body, &effort("none"), &caps("anthropic-always-thinking"), &q, false);
        assert!(out.ends_with(r#""output_config":{"effort":"low"}}"#), "{out}");
        assert_eq!(notes.adjusted.len(), 1);

        // A budget model takes a budget, raised to its minimum, and the automatic cache marker.
        let budget = TemplateSpec {
            reasoning: ReasoningSpec { budget_tokens: Some(Param::default_value(500)), ..Default::default() },
            prompt_cache: crate::model::PromptCacheMode::Auto,
            ..Default::default()
        };
        let (out, _, d) = shaped(Dialect::Anthropic, body, &budget, &caps("anthropic-budget"), &q, false);
        assert_eq!(
            out,
            r#"{"model":"m","max_tokens":100,"messages":[],"thinking":{"type":"enabled","budget_tokens":1024},"cache_control":{"type":"ephemeral"}}"#
        );
        assert_eq!(d.adjusted.len(), 1);
    }

    #[test]
    fn managed_mode_puts_the_callers_own_thinking_into_the_form_the_model_takes() {
        let q = Quirks::default();
        let none = TemplateSpec::default();
        // A budget sent to a model that decides for itself.
        let budget =
            r#"{"model":"m","max_tokens":100,"thinking":{"type":"enabled","budget_tokens":4096},"messages":[]}"#;
        let (out, notes, d) = shaped(Dialect::Anthropic, budget, &none, &caps("anthropic-adaptive"), &q, true);
        assert_eq!(out, r#"{"model":"m","max_tokens":100,"thinking":{"type":"adaptive"},"messages":[]}"#);
        assert_eq!(d.dropped, ["reasoning.budget_tokens"]);
        assert_eq!(notes.adjusted.len(), 1);
        // Proxy mode leaves the caller's request alone, whatever the model will say to it.
        let (out, _, _) = shaped(Dialect::Anthropic, budget, &none, &caps("anthropic-adaptive"), &q, false);
        assert_eq!(out, budget);

        // "Off" on a model that cannot switch thinking off.
        let off = r#"{"model":"m","max_tokens":100,"thinking":{"type":"disabled"},"messages":[]}"#;
        let (out, notes, _) = shaped(Dialect::Anthropic, off, &none, &caps("anthropic-always-thinking"), &q, true);
        assert_eq!(
            out,
            r#"{"model":"m","max_tokens":100,"thinking":{"type":"adaptive"},"messages":[],"output_config":{"effort":"low"}}"#
        );
        assert_eq!(notes.adjusted.len(), 1);
        // And on one that needs another word for it.
        let (out, _, _) = shaped(Dialect::Anthropic, off, &none, &caps("anthropic-sonnet-5-5"), &q, true);
        assert_eq!(out, r#"{"model":"m","max_tokens":100,"thinking":{"type":"between_tools"},"messages":[]}"#);

        // Adaptive thinking and an effort sent to a budget model: neither exists there.
        let adaptive = r#"{"model":"m","max_tokens":100,"thinking":{"type":"adaptive"},"output_config":{"effort":"high"},"messages":[]}"#;
        let (out, notes, d) = shaped(Dialect::Anthropic, adaptive, &none, &caps("anthropic-budget"), &q, true);
        assert_eq!(out, r#"{"model":"m","max_tokens":100,"output_config":{},"messages":[]}"#);
        assert_eq!((d.dropped, notes.dropped), (vec!["reasoning.effort".to_owned()], vec!["thinking".to_owned()]));

        // A conforming request keeps every byte.
        let fine = r#"{"model":"m","max_tokens":100,"thinking":{"type":"adaptive","display":"summarized"},"output_config":{"effort":"high"},"messages":[]}"#;
        let (out, notes, d) = shaped(Dialect::Anthropic, fine, &none, &caps("anthropic-adaptive"), &q, true);
        assert_eq!(out, fine);
        assert!(d.dropped.is_empty() && notes == Notes::default());
    }

    #[test]
    fn nested_dialects_are_patched_inside_their_parameter_object() {
        let spec =
            TemplateSpec { max_output_tokens: Some(256), temperature: Some(Param::locked(0.1)), ..Default::default() };
        let c = Capabilities { top_k: crate::model::ParamSupport::Accepted, ..Default::default() };
        let q = Quirks::default();

        let (out, _, _) = shaped(
            Dialect::Gemini,
            r#"{"contents":[{"parts":[{"text":"hi"}]}],"generationConfig":{"maxOutputTokens":9000,"topK":40}}"#,
            &spec,
            &c,
            &q,
            false,
        );
        assert_eq!(
            out,
            r#"{"contents":[{"parts":[{"text":"hi"}]}],"generationConfig":{"maxOutputTokens":256,"topK":40,"temperature":0.1}}"#
        );

        let (out, _, _) = shaped(Dialect::Ollama, r#"{"model":"llama","messages":[]}"#, &spec, &c, &q, false);
        assert_eq!(out, r#"{"model":"llama","messages":[],"options":{"num_predict":256,"temperature":0.1}}"#);

        let sent = read(
            Some(Dialect::Ollama),
            br#"{"options":{"num_predict":7,"top_p":0.5}}"#,
            &scan(br#"{"options":{"num_predict":7,"top_p":0.5}}"#).unwrap(),
        );
        assert_eq!((sent.params.max_output_tokens, sent.params.top_p), (Some(7), Some(0.5)));
    }

    #[test]
    fn anthropic_requests_are_read_in_neutral_terms() {
        let body = br#"{"max_tokens":300,"top_k":5,"thinking":{"type":"enabled","budget_tokens":2048},"output_config":{"effort":"medium"}}"#;
        let r = read(Some(Dialect::Anthropic), body, &scan(body).unwrap());
        assert_eq!(r.params.max_output_tokens, Some(300));
        assert_eq!(r.params.top_k, Some(5.0));
        assert_eq!(r.params.reasoning_budget, Some(2048));
        assert_eq!(r.params.reasoning_effort.as_deref(), Some("medium"));
        let off = br#"{"thinking":{"type":"disabled"}}"#;
        assert_eq!(
            read(Some(Dialect::Anthropic), off, &scan(off).unwrap()).params.reasoning_effort.as_deref(),
            Some("none")
        );
        assert_eq!(read(None, body, &scan(body).unwrap()), Read::default());
    }
}
