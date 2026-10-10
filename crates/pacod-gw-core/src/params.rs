//! How a template's parameters, the caller's parameters and the model's capability profile
//! combine (concept section 7.2).
//!
//! 1. Values: the caller's value overrides a template default; a template's locked value
//!    overrides the caller; "omit" removes the parameter.
//! 2. Ceilings: the output-token limit is the minimum of the template's, the grant's and the
//!    model's.
//! 3. Capability filter: runs last. A parameter the model does not accept is dropped, or the
//!    request is rejected when the caller sent it and the policy says so.
//!
//! The result is expressed as a decision per parameter relative to what the caller sent, so
//! that the same logic drives translation, the same-dialect path and proxy mode.

use std::collections::BTreeMap;

use crate::dataplane::errors::GwError;
use crate::model::{Capabilities, ParamSupport, ReasoningStyle, Rule, TemplateSpec, UnsupportedParams};
use crate::snapshot::GrantTerms;

/// What to do with a parameter in the request that goes to the provider.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision<T> {
    /// Leave it as the caller sent it (present or absent).
    Keep,
    /// Write this value.
    Set(T),
    /// Take the caller's value out.
    Remove,
}

impl<T> Decision<T> {
    pub fn is_keep(&self) -> bool {
        matches!(self, Decision::Keep)
    }
}

/// The dialect-neutral parameters the caller sent. `None` means absent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallerParams {
    pub max_output_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub reasoning_effort: Option<String>,
    pub reasoning_budget: Option<u64>,
}

/// What the merge needs to know about the target.
pub struct MergeContext<'a> {
    pub spec: Option<&'a TemplateSpec>,
    pub capabilities: &'a Capabilities,
    pub terms: GrantTerms,
    /// The model's own output ceiling.
    pub model_max_output: Option<u32>,
    pub policy: UnsupportedParams,
    /// Whether the capability filter applies to values the caller sent. It does in managed
    /// mode; proxy mode does not police the caller's own parameters.
    pub filter_caller: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decisions {
    pub max_output_tokens: Decision<u64>,
    /// The output-token limit that applies after the merge, when there is one.
    pub effective_max_output: Option<u64>,
    pub temperature: Decision<f64>,
    pub top_p: Decision<f64>,
    pub top_k: Decision<f64>,
    pub frequency_penalty: Decision<f64>,
    pub presence_penalty: Decision<f64>,
    pub reasoning_effort: Decision<String>,
    pub reasoning_budget: Decision<u64>,
    /// Parameters that were removed because the model does not accept them.
    pub dropped: Vec<String>,
    /// Values that were changed to fit, with the reason.
    pub adjusted: Vec<String>,
    /// Where the value of each parameter the call carries came from, for the audit record:
    /// `caller`, `template_default`, `template_locked`, `template_omit`, `ceiling`,
    /// `model_minimum`, `fitted` or `filter` (taken out by the capability filter).
    pub sources: BTreeMap<String, &'static str>,
}

/// Where a value comes from, before the capability filter.
fn source_of<T>(rule: &Rule<T>, caller_sent: bool) -> Option<&'static str> {
    match rule {
        Rule::Locked(_) => Some("template_locked"),
        Rule::Omit => Some("template_omit"),
        Rule::Default(_) if caller_sent => Some("caller"),
        Rule::Default(_) => Some("template_default"),
        Rule::Unset if caller_sent => Some("caller"),
        Rule::Unset => None,
    }
}

/// Effort levels in ascending order. `none` switches reasoning off.
const EFFORT_ORDER: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

fn effort_rank(e: &str) -> Option<usize> {
    EFFORT_ORDER.iter().position(|x| *x == e)
}

struct Merge<'a> {
    cx: &'a MergeContext<'a>,
    dropped: Vec<String>,
    adjusted: Vec<String>,
    sources: BTreeMap<String, &'static str>,
}

impl Merge<'_> {
    /// Applies the capability filter to one value. `from_caller` tells whether the value is the
    /// caller's own, which decides whether the policy may reject the request.
    fn filter<T>(
        &mut self,
        name: &str,
        value: Option<T>,
        support: ParamSupport,
        from_caller: bool,
    ) -> Result<Option<T>, GwError> {
        let Some(v) = value else { return Ok(None) };
        if from_caller && !self.cx.filter_caller {
            return Ok(Some(v));
        }
        let accepted = match support {
            ParamSupport::Accepted => true,
            // The gateway does not know every model's default value, so any explicit value
            // counts as a deviation. Sending nothing is always accepted.
            ParamSupport::Rejected | ParamSupport::DefaultOnly => false,
        };
        if accepted {
            return Ok(Some(v));
        }
        if from_caller && self.cx.policy == UnsupportedParams::Reject {
            return Err(GwError::unsupported_feature(format!("the model does not accept the parameter {name}")));
        }
        self.dropped.push(name.to_owned());
        self.sources.insert(name.to_owned(), "filter");
        Ok(None)
    }

    fn sampling<T: Clone + PartialEq>(
        &mut self,
        name: &str,
        rule: Rule<T>,
        caller: &Option<T>,
        support: ParamSupport,
    ) -> Result<Decision<T>, GwError> {
        // Step 1: values.
        if let Some(source) = source_of(&rule, caller.is_some()) {
            self.sources.insert(name.to_owned(), source);
        }
        let (effective, from_caller) = match rule {
            Rule::Locked(v) => (Some(v), false),
            Rule::Omit => (None, false),
            Rule::Default(v) => match caller {
                Some(c) => (Some(c.clone()), true),
                None => (Some(v), false),
            },
            Rule::Unset => (caller.clone(), true),
        };
        // Step 3: capability filter.
        let effective = self.filter(name, effective, support, from_caller)?;
        Ok(match (caller, effective) {
            (None, None) => Decision::Keep,
            (Some(c), Some(e)) if *c == e => Decision::Keep,
            (_, Some(e)) => Decision::Set(e),
            (Some(_), None) => Decision::Remove,
        })
    }
}

/// Combines template, caller and capabilities.
pub fn merge(cx: &MergeContext<'_>, caller: &CallerParams) -> Result<Decisions, GwError> {
    let mut m = Merge { cx, dropped: Vec::new(), adjusted: Vec::new(), sources: BTreeMap::new() };
    let caps = cx.capabilities;
    let spec = cx.spec;

    let temperature = m.sampling(
        "temperature",
        spec.map_or(Rule::Unset, |s| Rule::of(&s.temperature)),
        &caller.temperature,
        caps.temperature,
    )?;
    let top_p = m.sampling("top_p", spec.map_or(Rule::Unset, |s| Rule::of(&s.top_p)), &caller.top_p, caps.top_p)?;
    let top_k = m.sampling("top_k", Rule::Unset, &caller.top_k, caps.top_k)?;
    let frequency_penalty = m.sampling(
        "frequency_penalty",
        spec.map_or(Rule::Unset, |s| Rule::of(&s.frequency_penalty)),
        &caller.frequency_penalty,
        caps.penalties,
    )?;
    let presence_penalty = m.sampling(
        "presence_penalty",
        spec.map_or(Rule::Unset, |s| Rule::of(&s.presence_penalty)),
        &caller.presence_penalty,
        caps.penalties,
    )?;

    // --- output tokens: default from the template, ceiling from template, grant and model ---
    let ceiling = [spec.and_then(|s| s.max_output_tokens), cx.terms.max_output_tokens, cx.model_max_output]
        .into_iter()
        .flatten()
        .min()
        .map(u64::from);
    let wanted = caller.max_output_tokens.or(spec.and_then(|s| s.max_output_tokens).map(u64::from));
    let effective_max_output = match (wanted, ceiling) {
        (Some(w), Some(c)) => Some(w.min(c)),
        (Some(w), None) => Some(w),
        (None, _) => None,
    };
    if caller.max_output_tokens.is_some() {
        m.sources.insert("max_output_tokens".into(), "caller");
    } else if wanted.is_some() {
        m.sources.insert("max_output_tokens".into(), "template_default");
    }
    if let (Some(w), Some(e)) = (wanted, effective_max_output)
        && e < w
    {
        m.sources.insert("max_output_tokens".into(), "ceiling");
    }
    if let (Some(c), Some(e)) = (caller.max_output_tokens, effective_max_output)
        && e < c
    {
        m.adjusted.push(format!("max_output_tokens lowered from {c} to {e}"));
    }
    let max_output_tokens = match (caller.max_output_tokens, effective_max_output) {
        (Some(c), Some(e)) if c == e => Decision::Keep,
        (_, Some(e)) => Decision::Set(e),
        (None, None) => Decision::Keep,
        (Some(_), None) => Decision::Keep,
    };

    // --- reasoning ---
    let effort_rule = spec.map_or(Rule::Unset, |s| Rule::of(&s.reasoning.effort));
    if let Some(source) = source_of(&effort_rule, caller.reasoning_effort.is_some()) {
        m.sources.insert("reasoning.effort".into(), source);
    }
    let (effort, effort_from_caller) = match effort_rule {
        Rule::Locked(v) => (Some(v), false),
        Rule::Omit => (None, false),
        Rule::Default(v) => match &caller.reasoning_effort {
            Some(c) => (Some(c.clone()), true),
            None => (Some(v), false),
        },
        Rule::Unset => (caller.reasoning_effort.clone(), true),
    };
    let effort = match effort {
        None => None,
        Some(e) if effort_from_caller && !cx.filter_caller => Some(e),
        Some(e) => {
            let fitted = fit_effort(&mut m, e.clone(), effort_from_caller)?;
            if fitted.as_ref().is_some_and(|f| *f != e) {
                m.sources.insert("reasoning.effort".into(), "fitted");
            }
            fitted
        }
    };
    let reasoning_effort = match (&caller.reasoning_effort, effort) {
        (None, None) => Decision::Keep,
        (Some(c), Some(e)) if *c == e => Decision::Keep,
        (_, Some(e)) => Decision::Set(e),
        (Some(_), None) => Decision::Remove,
    };

    let budget_rule = spec.map_or(Rule::Unset, |s| Rule::of(&s.reasoning.budget_tokens));
    if let Some(source) = source_of(&budget_rule, caller.reasoning_budget.is_some()) {
        m.sources.insert("reasoning.budget_tokens".into(), source);
    }
    let (budget, budget_from_caller) = match budget_rule {
        Rule::Locked(v) => (Some(u64::from(v)), false),
        Rule::Omit => (None, false),
        Rule::Default(v) => match caller.reasoning_budget {
            Some(c) => (Some(c), true),
            None => (Some(u64::from(v)), false),
        },
        Rule::Unset => (caller.reasoning_budget, true),
    };
    let budget = match budget {
        None => None,
        Some(b) if budget_from_caller && !cx.filter_caller => Some(b),
        Some(_) if caps.reasoning != ReasoningStyle::Budget => {
            if budget_from_caller && cx.policy == UnsupportedParams::Reject {
                return Err(GwError::unsupported_feature("the model does not take a reasoning token budget"));
            }
            m.dropped.push("reasoning.budget_tokens".into());
            m.sources.insert("reasoning.budget_tokens".into(), "filter");
            None
        }
        Some(b) => {
            let min = u64::from(caps.reasoning_min_budget.unwrap_or(0));
            if b < min {
                m.sources.insert("reasoning.budget_tokens".into(), "model_minimum");
                m.adjusted.push(format!("reasoning budget raised from {b} to the model's minimum {min}"));
                Some(min)
            } else {
                Some(b)
            }
        }
    };
    let reasoning_budget = match (caller.reasoning_budget, budget) {
        (None, None) => Decision::Keep,
        (Some(c), Some(e)) if c == e => Decision::Keep,
        (_, Some(e)) => Decision::Set(e),
        (Some(_), None) => Decision::Remove,
    };

    Ok(Decisions {
        max_output_tokens,
        effective_max_output,
        temperature,
        top_p,
        top_k,
        frequency_penalty,
        presence_penalty,
        reasoning_effort,
        reasoning_budget,
        dropped: m.dropped,
        adjusted: m.adjusted,
        sources: m.sources,
    })
}

/// Proxy mode, a template set to `check` (concept section 7.4): nothing is written, so the
/// request must already respect the template's locked values and its ceilings. Returns what
/// is wrong with the request, if anything.
pub fn check(cx: &MergeContext<'_>, caller: &CallerParams) -> Result<(), String> {
    let Some(spec) = cx.spec else { return Ok(()) };
    fn rule<T: PartialEq + std::fmt::Display>(name: &str, rule: Rule<T>, caller: &Option<T>) -> Result<(), String> {
        match (rule, caller) {
            (Rule::Locked(v), Some(c)) if *c == v => Ok(()),
            (Rule::Locked(v), Some(c)) => Err(format!("{name} must be {v}, not {c}")),
            (Rule::Locked(v), None) => Err(format!("{name} must be set to {v}")),
            (Rule::Omit, Some(_)) => Err(format!("{name} must not be sent")),
            _ => Ok(()),
        }
    }
    rule("temperature", Rule::of(&spec.temperature), &caller.temperature)?;
    rule("top_p", Rule::of(&spec.top_p), &caller.top_p)?;
    rule("frequency_penalty", Rule::of(&spec.frequency_penalty), &caller.frequency_penalty)?;
    rule("presence_penalty", Rule::of(&spec.presence_penalty), &caller.presence_penalty)?;
    rule("the reasoning effort", Rule::of(&spec.reasoning.effort), &caller.reasoning_effort)?;
    let budget = match Rule::of(&spec.reasoning.budget_tokens) {
        Rule::Locked(v) => Rule::Locked(u64::from(v)),
        Rule::Default(v) => Rule::Default(u64::from(v)),
        Rule::Omit => Rule::Omit,
        Rule::Unset => Rule::Unset,
    };
    rule("the reasoning budget", budget, &caller.reasoning_budget)?;

    let ceiling = [spec.max_output_tokens, cx.terms.max_output_tokens, cx.model_max_output]
        .into_iter()
        .flatten()
        .min()
        .map(u64::from);
    match (ceiling, caller.max_output_tokens) {
        (Some(c), Some(v)) if v > c => Err(format!("the output-token limit must be at most {c}, not {v}")),
        // Without a limit in the request the provider's own maximum would apply.
        (Some(c), None) if spec.max_output_tokens.is_some() => {
            Err(format!("the output-token limit must be set, to at most {c}"))
        }
        _ => Ok(()),
    }
}

/// Fits an effort level to what the model accepts: an unsupported level becomes the nearest
/// lower one the model has, and a model without effort levels drops it.
fn fit_effort(m: &mut Merge<'_>, effort: String, from_caller: bool) -> Result<Option<String>, GwError> {
    let caps = m.cx.capabilities;
    let supported = &caps.reasoning_efforts;
    if effort == "none" {
        // Switching reasoning off is expressed by the adapter in the provider's own terms.
        return Ok(Some(effort));
    }
    if supported.is_empty() || matches!(caps.reasoning, ReasoningStyle::None | ReasoningStyle::Budget) {
        if from_caller && m.cx.policy == UnsupportedParams::Reject {
            return Err(GwError::unsupported_feature("the model does not take a reasoning effort"));
        }
        m.dropped.push("reasoning.effort".into());
        m.sources.insert("reasoning.effort".into(), "filter");
        return Ok(None);
    }
    if supported.contains(&effort) {
        return Ok(Some(effort));
    }
    let Some(rank) = effort_rank(&effort) else {
        return Err(GwError::invalid_request(format!("\"{effort}\" is not a reasoning effort level")));
    };
    // The highest supported level below the requested one, or the lowest supported level.
    let fitted = supported
        .iter()
        .filter(|s| effort_rank(s).is_some_and(|r| r <= rank))
        .max_by_key(|s| effort_rank(s))
        .or_else(|| supported.iter().min_by_key(|s| effort_rank(s)))
        .cloned()
        .expect("supported is not empty");
    m.adjusted.push(format!("reasoning effort {effort} is not available on this model; {fitted} was used"));
    Ok(Some(fitted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::repo::capability_presets;
    use crate::model::{Param, ReasoningSpec};

    fn caps(preset: &str) -> Capabilities {
        capability_presets().into_iter().find(|p| p.name == preset).unwrap().capabilities
    }

    fn cx<'a>(spec: Option<&'a TemplateSpec>, capabilities: &'a Capabilities) -> MergeContext<'a> {
        MergeContext {
            spec,
            capabilities,
            terms: GrantTerms { follow_successor: true, max_output_tokens: None },
            model_max_output: None,
            policy: UnsupportedParams::Drop,
            filter_caller: true,
        }
    }

    fn caller(f: impl FnOnce(&mut CallerParams)) -> CallerParams {
        let mut c = CallerParams::default();
        f(&mut c);
        c
    }

    #[test]
    fn a_check_template_accepts_only_requests_that_already_conform() {
        let spec = TemplateSpec {
            max_output_tokens: Some(400),
            temperature: Some(Param::locked(0.2)),
            top_p: Some(Param::omitted()),
            frequency_penalty: Some(Param::default_value(0.5)),
            ..Default::default()
        };
        let c = caps("openai-chat");
        let cx = cx(Some(&spec), &c);
        let ok = caller(|p| {
            p.max_output_tokens = Some(400);
            p.temperature = Some(0.2);
        });
        assert_eq!(check(&cx, &ok), Ok(()));
        // A default is not enforced; a lock, an omission and the ceiling are.
        assert_eq!(
            check(
                &cx,
                &caller(|p| {
                    p.max_output_tokens = Some(100);
                    p.temperature = Some(0.2);
                    p.frequency_penalty = Some(1.0);
                })
            ),
            Ok(())
        );
        let e = |f: fn(&mut CallerParams)| {
            check(
                &cx,
                &caller(|p| {
                    p.max_output_tokens = Some(400);
                    p.temperature = Some(0.2);
                    f(p)
                }),
            )
            .unwrap_err()
        };
        assert!(e(|p| p.temperature = Some(0.9)).contains("temperature must be 0.2"));
        assert!(e(|p| p.temperature = None).contains("temperature must be set"));
        assert!(e(|p| p.top_p = Some(0.5)).contains("top_p must not be sent"));
        assert!(e(|p| p.max_output_tokens = Some(8000)).contains("at most 400"));
        assert!(e(|p| p.max_output_tokens = None).contains("must be set"));
        // Without a template there is nothing to check.
        assert_eq!(check(&super::tests::cx(None, &c), &CallerParams::default()), Ok(()));
    }

    #[test]
    fn concept_example_template_default_dropped_by_the_capability_filter() {
        // @synthesizer sets temperature 0.3 as a default on Claude Sonnet 5.5, which takes
        // temperature only at its default; the caller sends nothing.
        let spec = TemplateSpec { temperature: Some(Param::default_value(0.3)), ..Default::default() };
        let c = caps("anthropic-sonnet-5-5");
        let d = merge(&cx(Some(&spec), &c), &CallerParams::default()).unwrap();
        assert_eq!(d.temperature, Decision::Keep, "nothing is written");
        assert_eq!(d.dropped, ["temperature"]);
    }

    #[test]
    fn value_precedence_model_template_caller_locked() {
        let c = caps("openai-chat");
        let run = |p: Option<Param<f64>>, sent: Option<f64>| {
            let spec = TemplateSpec { temperature: p, ..Default::default() };
            merge(&cx(Some(&spec), &c), &caller(|x| x.temperature = sent)).unwrap().temperature
        };
        // No template opinion: the caller's value stays.
        assert_eq!(run(None, Some(0.9)), Decision::Keep);
        assert_eq!(run(None, None), Decision::Keep);
        // Template default: used when the caller is silent, overridden when not.
        assert_eq!(run(Some(Param::default_value(0.3)), None), Decision::Set(0.3));
        assert_eq!(run(Some(Param::default_value(0.3)), Some(0.9)), Decision::Keep);
        // Locked: always the template's value.
        assert_eq!(run(Some(Param::locked(0.3)), Some(0.9)), Decision::Set(0.3));
        assert_eq!(run(Some(Param::locked(0.3)), Some(0.3)), Decision::Keep);
        assert_eq!(run(Some(Param::locked(0.3)), None), Decision::Set(0.3));
        // Omit: never sent.
        assert_eq!(run(Some(Param::omitted()), Some(0.9)), Decision::Remove);
        assert_eq!(run(Some(Param::omitted()), None), Decision::Keep);
    }

    #[test]
    fn rejected_parameters_are_dropped_or_refuse_the_request() {
        let c = caps("anthropic-adaptive");
        let sent = caller(|x| {
            x.temperature = Some(0.2);
            x.top_p = Some(0.9);
        });
        let d = merge(&cx(None, &c), &sent).unwrap();
        assert_eq!((d.temperature, d.top_p), (Decision::Remove, Decision::Remove));
        assert_eq!(d.dropped, ["temperature", "top_p"]);

        let mut strict = cx(None, &c);
        strict.policy = UnsupportedParams::Reject;
        let err = merge(&strict, &sent).unwrap_err();
        assert_eq!(err.kind, "unsupported_feature");

        // A template's own unsupported value never fails the request, whatever the policy.
        let spec = TemplateSpec { temperature: Some(Param::default_value(0.3)), ..Default::default() };
        let mut strict = cx(Some(&spec), &c);
        strict.policy = UnsupportedParams::Reject;
        let d = merge(&strict, &CallerParams::default()).unwrap();
        assert_eq!(d.dropped, ["temperature"]);
    }

    #[test]
    fn proxy_mode_does_not_filter_the_callers_own_parameters() {
        let c = caps("anthropic-adaptive");
        let spec = TemplateSpec { top_p: Some(Param::default_value(0.5)), ..Default::default() };
        let mut proxy = cx(Some(&spec), &c);
        proxy.filter_caller = false;
        let d = merge(&proxy, &caller(|x| x.temperature = Some(0.2))).unwrap();
        assert_eq!(d.temperature, Decision::Keep, "the caller's value is relayed as sent");
        assert_eq!(d.top_p, Decision::Keep, "the template's value is not written to a model that rejects it");
        assert_eq!(d.dropped, ["top_p"]);
    }

    #[test]
    fn output_tokens_default_and_ceilings() {
        let c = caps("openai-chat");
        let spec = TemplateSpec { max_output_tokens: Some(400), ..Default::default() };
        let run = |spec: Option<&TemplateSpec>, grant: Option<u32>, model: Option<u32>, sent: Option<u64>| {
            let mut x = cx(spec, &c);
            x.terms.max_output_tokens = grant;
            x.model_max_output = model;
            let d = merge(&x, &caller(|p| p.max_output_tokens = sent)).unwrap();
            (d.max_output_tokens, d.effective_max_output)
        };
        // The concept's @planner example: the caller asks for 8000, the ceiling is 400.
        assert_eq!(run(Some(&spec), None, None, Some(8000)), (Decision::Set(400), Some(400)));
        // Below the ceiling: untouched.
        assert_eq!(run(Some(&spec), None, None, Some(100)), (Decision::Keep, Some(100)));
        // Silent caller: the template's value is the default.
        assert_eq!(run(Some(&spec), None, None, None), (Decision::Set(400), Some(400)));
        // The minimum of template, grant and model wins.
        assert_eq!(run(Some(&spec), Some(300), Some(1000), Some(8000)), (Decision::Set(300), Some(300)));
        assert_eq!(run(None, None, Some(1000), Some(8000)), (Decision::Set(1000), Some(1000)));
        // No template and no ceiling: nothing to say.
        assert_eq!(run(None, None, None, None), (Decision::Keep, None));
        assert_eq!(run(None, None, Some(1000), None), (Decision::Keep, None), "a ceiling alone sets no default");
    }

    #[test]
    fn effort_is_fitted_to_the_model() {
        let run = |preset: &str, sent: &str| {
            let c = caps(preset);
            let d = merge(&cx(None, &c), &caller(|p| p.reasoning_effort = Some(sent.into()))).unwrap();
            (d.reasoning_effort, d.dropped, d.adjusted.len())
        };
        assert_eq!(run("anthropic-adaptive", "max"), (Decision::Keep, vec![], 0));
        // xhigh on a model that tops out at high.
        assert_eq!(run("openai-compatible", "xhigh"), (Decision::Set("high".into()), vec![], 1));
        assert_eq!(run("openai-compatible", "max"), (Decision::Set("high".into()), vec![], 1));
        // A model with no effort levels: dropped.
        assert_eq!(run("openai-chat", "high"), (Decision::Remove, vec!["reasoning.effort".to_owned()], 0));
        assert_eq!(run("anthropic-budget", "high"), (Decision::Remove, vec!["reasoning.effort".to_owned()], 0));
        // "none" is passed on for the adapter to express.
        assert_eq!(run("anthropic-adaptive", "none"), (Decision::Keep, vec![], 0));
    }

    #[test]
    fn unknown_effort_is_an_error() {
        let c = caps("openai-compatible");
        let err = merge(&cx(None, &c), &caller(|p| p.reasoning_effort = Some("extreme".into()))).unwrap_err();
        assert_eq!(err.kind, "invalid_request");
    }

    #[test]
    fn budget_only_on_budget_models_and_at_least_the_minimum() {
        let haiku = caps("anthropic-budget");
        let spec = TemplateSpec {
            reasoning: ReasoningSpec { budget_tokens: Some(Param::default_value(500)), ..Default::default() },
            ..Default::default()
        };
        let d = merge(&cx(Some(&spec), &haiku), &CallerParams::default()).unwrap();
        assert_eq!(d.reasoning_budget, Decision::Set(1024));
        assert_eq!(d.adjusted.len(), 1);

        let opus = caps("anthropic-always-thinking");
        let d = merge(&cx(Some(&spec), &opus), &CallerParams::default()).unwrap();
        assert_eq!(d.reasoning_budget, Decision::Keep);
        assert_eq!(d.dropped, ["reasoning.budget_tokens"]);
        let d = merge(&cx(None, &opus), &caller(|p| p.reasoning_budget = Some(8000))).unwrap();
        assert_eq!(d.reasoning_budget, Decision::Remove);
    }
}
