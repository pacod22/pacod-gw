//! CEL conditions of routing rules: for what the typed conditions cannot say.
//!
//! A condition is compiled once per snapshot. Its cost is bounded when it is compiled: a
//! short source, a fixed set of variables and functions, and no comprehension macros
//! (`all`, `exists`, `map`, `filter`), so that evaluation is linear in the size of the
//! expression. A condition that fails to evaluate, or yields anything but a boolean, does not
//! match: routing then falls through to the next rule. Refusing the call instead would turn a
//! mistake in a scheme into an outage.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cel::{Context, Program, Value};

use super::RouteInput;
use super::live::now_ms;
use crate::telemetry::metrics;

/// The longest source accepted.
pub const MAX_SOURCE: usize = 512;

/// What a condition may read.
const VARIABLES: &[&str] = &[
    "complexity",
    "agent_id",
    "task",
    "user_id",
    "end_user",
    "session_id",
    "template",
    "interface",
    "stream",
    "input_tokens",
    "tags",
    "intent",
    "intent_confidence",
    "flags",
];

/// The functions a condition may call, besides the operators.
const FUNCTIONS: &[&str] =
    &["size", "startsWith", "endsWith", "contains", "has", "int", "uint", "double", "string", "@in"];

/// Macros that loop.
const LOOPS: &[&str] = &["all", "exists", "exists_one", "map", "filter"];

/// At most one warning about failing conditions every so often.
const WARN_EVERY_MS: u64 = 10_000;
static LAST_WARNING: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The standard library, built once per thread; each evaluation adds its variables in a
    /// scope of its own.
    static ROOT: Context<'static> = Context::default();
}

#[derive(Debug)]
pub struct CelRule {
    program: Program,
    /// The variables the condition reads, by position in `VARIABLES`.
    reads: u16,
    scheme: Arc<str>,
    rule: Arc<str>,
}

fn is_operator(f: &str) -> bool {
    // `_==_`, `_&&_`, `!_`, `-_`, `_[_]` … but not `@not_strictly_false`, the loop marker.
    f.starts_with('_') || f == "!_" || f == "-_"
}

impl CelRule {
    pub fn compile(source: &str, scheme: &str, rule: &str) -> Result<Self, String> {
        if source.len() > MAX_SOURCE {
            return Err(format!("the CEL condition is longer than {MAX_SOURCE} characters"));
        }
        let squeezed: String = source.chars().filter(|c| !c.is_whitespace()).collect();
        if let Some(m) = LOOPS.iter().find(|m| squeezed.contains(&format!(".{m}("))) {
            return Err(format!("the CEL condition may not use the macro {m}: conditions do not loop"));
        }
        let program = Program::compile(source).map_err(|e| format!("the CEL condition does not parse: {e}"))?;
        let refs = program.references();
        let mut reads = 0u16;
        for v in refs.variables() {
            let Some(i) = VARIABLES.iter().position(|x| *x == v) else {
                return Err(format!("the CEL condition reads \"{v}\"; it may read {}", VARIABLES.join(", ")));
            };
            reads |= 1 << i;
        }
        if let Some(f) = refs.functions().into_iter().find(|f| !is_operator(f) && !FUNCTIONS.contains(f)) {
            return Err(format!("the CEL condition calls \"{f}\", which conditions may not use"));
        }
        let c = Self { program, reads, scheme: scheme.into(), rule: rule.into() };
        // Tried on a call that says nothing: an answer that is not true or false is a mistake.
        // An error is not: a condition may well need a value an empty call lacks.
        if let Ok(v) = c.evaluate(&RouteInput::empty())
            && !matches!(v, Value::Bool(_))
        {
            return Err("the CEL condition must be true or false".into());
        }
        Ok(c)
    }

    fn evaluate(&self, input: &RouteInput<'_>) -> cel::ResolveResult {
        ROOT.with(|root| {
            let mut ctx = root.new_inner_scope();
            for (i, name) in VARIABLES.iter().enumerate() {
                if self.reads & (1 << i) == 0 {
                    continue;
                }
                let text = |v: Option<&str>| Value::from(v.unwrap_or_default());
                let value = match *name {
                    "complexity" => Value::from(input.complexity.map_or("", |c| c.name())),
                    "agent_id" => text(input.agent_id),
                    "task" => text(input.task),
                    "user_id" => text(input.user_id),
                    "end_user" => text(input.end_user),
                    "session_id" => text(input.session_id),
                    "template" => Value::from(input.template),
                    "interface" => Value::from(input.interface),
                    "stream" => Value::Bool(input.stream),
                    "input_tokens" => Value::Int(i64::from(input.input_tokens)),
                    "intent" => text(input.intent),
                    "intent_confidence" => Value::Float(f64::from(input.intent_confidence)),
                    "flags" => Value::from(
                        input.flags.iter().map(|(k, v)| (k.clone(), f64::from(*v))).collect::<HashMap<String, f64>>(),
                    ),
                    _ => Value::from(
                        input.tags.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<HashMap<String, String>>(),
                    ),
                };
                ctx.add_variable_from_value(*name, value);
            }
            self.program.execute(&ctx)
        })
    }

    /// Whether the condition holds. Anything but `true` does not match.
    pub fn matches(&self, input: &RouteInput<'_>) -> bool {
        let why = match self.evaluate(input) {
            Ok(Value::Bool(b)) => return b,
            Ok(other) => format!("yields {other:?}, not true or false"),
            Err(e) => e.to_string(),
        };
        metrics::record_cel_error(&self.scheme);
        let now = now_ms();
        let last = LAST_WARNING.load(Ordering::Relaxed);
        if now >= last + WARN_EVERY_MS
            && LAST_WARNING.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok()
        {
            tracing::warn!(scheme = %self.scheme, rule = %self.rule, error = %why, "a routing condition failed; the rule did not match");
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::Complexity;

    fn rule(src: &str) -> Result<CelRule, String> {
        CelRule::compile(src, "s", "r")
    }

    #[test]
    fn conditions_read_the_call() {
        let mut tags = BTreeMap::new();
        tags.insert("team".to_owned(), "search".to_owned());
        let input = RouteInput {
            complexity: Some(Complexity::High),
            task: Some("code-review"),
            tags: &tags,
            input_tokens: 40_000,
            stream: true,
            ..RouteInput::empty()
        };
        let yes = |src: &str| rule(src).unwrap().matches(&input);
        assert!(yes(r#"complexity == "high" && input_tokens > 32000"#));
        assert!(yes(r#"task.startsWith("code-") || agent_id == "x""#));
        assert!(yes(r#"tags["team"] == "search" && stream"#));
        assert!(yes(r#""team" in tags && size(tags) == 1"#));
        assert!(!yes(r#"agent_id != """#), "a value not given is empty");
        // A missing tag is an error at evaluation: the rule does not match.
        assert!(!yes(r#"tags["site"] == "eu""#));
    }

    #[test]
    fn what_a_condition_may_not_do() {
        for (src, why) in [
            ("tags.all(k, k != \"x\")", "loop"),
            ("[1, 2].exists( x , x > 1)", "loop"),
            ("request.size > 3", "reads"),
            ("task.matches(\"a.*\")", "calls"),
            ("task", "true or false"),
            ("task ==", "parse"),
        ] {
            let e = rule(src).unwrap_err();
            assert!(e.contains(why), "{src}: {e}");
        }
        assert!(rule(&"a".repeat(MAX_SOURCE + 1)).unwrap_err().contains("longer"));
    }
}
