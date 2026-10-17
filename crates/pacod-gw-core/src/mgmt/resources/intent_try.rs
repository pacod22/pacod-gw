//! Trying a workspace's intent binding on sample texts (`docs/plugin-concept.md`, section 6.4):
//! one text with each engine side by side, or a labelled set with accuracy and a confusion
//! matrix per engine.
//!
//! The binding's settings are compiled again with the engine asked for and without the cache,
//! so that every engine answers itself. The engines' calls are real: they cost money and count
//! against the workspace's limits like the plugin's calls on the request path.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use pacod_gw_plugin::{Action, Bound, Conversation, RequestInput, Role, Turn};
use pacod_gw_plugin_intent::settings::EngineChoice;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::dataplane::auth::Caller;
use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{Params, ScopeKind, collection_path};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::plugins::ActiveBinding;
use crate::plugins::host::CoreHost;
use crate::telemetry::trace::TraceContext;

/// The longest text tried, in characters.
const MAX_TEXT: usize = 32_000;
/// The most samples an evaluation takes: each costs a call per engine.
const MAX_SAMPLES: usize = 200;
/// Samples classified at once.
const PARALLEL: usize = 4;
/// The wrong answers an evaluation lists.
const LISTED_WRONG: usize = 20;

/// A text to classify, as a user's message.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntentTry {
    pub text: String,
    /// The engines to ask, each on its own. Empty: the binding's engine.
    #[serde(default)]
    pub engines: Vec<EngineChoice>,
}

/// What one engine answered about a text.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EngineAnswer {
    pub engine: String,
    pub intent: Option<String>,
    pub confidence: Option<f32>,
    /// The intent routing and limits would act on.
    pub act: Option<String>,
    pub flags: BTreeMap<String, f32>,
    pub scores: BTreeMap<String, f32>,
    /// `allow` or `block`, as the catalogue's rules decided.
    pub action: String,
    /// What a blocked caller would be told.
    pub message: Option<String>,
    pub rule: Option<String>,
    /// A rule asked for the call to be marked.
    pub flagged: bool,
    /// Rules in trial that matched, as `rule: action`.
    pub trial: Vec<String>,
    pub elapsed_ms: f64,
    /// The probabilities, the rationale and the versions that answered.
    pub detail: Value,
    /// Why the engine gave no answer.
    pub error: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct IntentTryResult {
    /// The binding tried: the workspace's own, or its organisation's.
    pub binding: String,
    pub answers: Vec<EngineAnswer>,
}

/// A text and the intent it should get.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntentSample {
    pub text: String,
    pub intent: String,
}

/// A labelled set to classify with each engine.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntentEvaluate {
    /// 1 to 200 samples.
    pub samples: Vec<IntentSample>,
    /// Empty: the binding's engine.
    #[serde(default)]
    pub engines: Vec<EngineChoice>,
}

/// How well one intent was found.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct IntentScore {
    /// Samples labelled with the intent.
    pub support: u32,
    /// Answers that named it.
    pub predicted: u32,
    pub correct: u32,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WrongAnswer {
    /// The sample's text, cut to 120 characters.
    pub text: String,
    pub expected: String,
    pub got: String,
}

/// One engine's results on a labelled set.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EngineEvaluation {
    pub engine: String,
    pub samples: u32,
    pub correct: u32,
    /// Correct answers of all samples, errors counted as wrong.
    pub accuracy: f64,
    pub errors: u32,
    pub per_intent: BTreeMap<String, IntentScore>,
    /// Expected intent, then the answer, then how often.
    pub confusion: BTreeMap<String, BTreeMap<String, u32>>,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    /// The first wrong answers.
    pub wrong: Vec<WrongAnswer>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct IntentEvaluation {
    pub binding: String,
    pub engines: Vec<EngineEvaluation>,
}

/// The workspace's intent binding, and the caller its engines' calls run as.
fn binding(
    st: &AdminState,
    p: &Principal,
    params: &std::collections::HashMap<String, String>,
) -> ApiResult<(Caller, Arc<ActiveBinding>)> {
    let caller = crate::mgmt::playground::caller(st, p, params)?;
    let b = caller
        .workspace
        .plugins
        .iter()
        .find(|b| b.plugin == pacod_gw_plugin_intent::ID)
        .cloned()
        .ok_or_else(|| ApiError::conflict("the intent plugin is not switched on for this workspace"))?;
    Ok((caller, b))
}

/// The binding compiled for one engine, without the cache. `None` and the reason when that
/// engine cannot be used here.
fn compile_for(caller: &Caller, b: &ActiveBinding, engine: EngineChoice) -> Result<Arc<dyn Bound>, String> {
    let plugin = crate::plugins::find(&b.plugin).ok_or("the intent plugin is not built into this gateway")?;
    let snap = &caller.snapshot;
    let ws = &caller.workspace;
    let org = snap.organisations.get(&ws.organisation_id).ok_or("the organisation is gone")?;
    let mut settings = b.settings.clone();
    if !settings.is_object() {
        settings = json!({});
    }
    settings["engine"] = json!(engine.name());
    settings["cache_seconds"] = json!(0);
    let install = snap.plugin_installs.get(&b.plugin).filter(|i| i.enabled).ok_or("the plugin is not installed")?;
    crate::plugins::check_engines(plugin.as_ref(), &settings, install, ws.settings.plugin_third_parties.as_deref())?;
    let lookup = |name: &str| ws.catalogue(org, name).map(crate::snapshot::CatalogueRt::view);
    let scope = crate::plugins::Scope::of_workspace(snap, org, ws, &lookup);
    crate::plugins::compile(plugin.as_ref(), &settings, &scope).map_err(|p| p.join("; "))
}

/// The engines asked for, or the binding's.
fn engines_of(asked: &[EngineChoice], b: &ActiveBinding) -> Vec<EngineChoice> {
    if !asked.is_empty() {
        let mut out = asked.to_vec();
        out.dedup();
        return out;
    }
    let own = b.settings.get("engine").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    vec![own]
}

/// Classifies one text with a compiled binding.
async fn ask(
    st: &AdminState,
    caller: &Caller,
    b: &ActiveBinding,
    bound: &dyn Bound,
    engine: EngineChoice,
    text: &str,
) -> EngineAnswer {
    let call_id = format!("try_{}", uuid::Uuid::now_v7().simple());
    let input = RequestInput {
        call_id: call_id.clone(),
        organisation_id: caller.key.organisation_id.clone(),
        workspace_id: caller.key.workspace_id.clone(),
        key_id: caller.key.id.clone(),
        key_name: caller.key.doc.name.clone(),
        interface: "native".into(),
        path: "/plugins/intent/try".into(),
        body_bytes: text.len(),
        conversation: Conversation {
            turns: vec![Turn { role: Role::User, text: text.to_owned() }],
            ..Default::default()
        },
        ..Default::default()
    };
    let trace = TraceContext::from_traceparent(None);
    let host = CoreHost::new(st.gw.clone(), caller.clone(), &b.plugin, &call_id, trace.traceparent(trace.span_id));
    let started = Instant::now();
    let result = tokio::time::timeout(b.timeout, bound.on_request(&input, &host)).await;
    let elapsed_ms = (started.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;
    let mut answer = EngineAnswer {
        engine: engine.name().into(),
        intent: None,
        confidence: None,
        act: None,
        flags: BTreeMap::new(),
        scores: BTreeMap::new(),
        action: "allow".into(),
        message: None,
        rule: None,
        flagged: false,
        trial: Vec::new(),
        elapsed_ms,
        detail: Value::Null,
        error: None,
    };
    match result {
        Err(_) => answer.error = Some(format!("timeout: no answer within {} ms", b.timeout.as_millis())),
        Ok(Err(e)) => answer.error = Some(e.to_string()),
        Ok(Ok(v)) => {
            if let Some(i) = v.signals.intent {
                answer.intent = Some(i.label);
                answer.confidence = Some(i.confidence);
                answer.act = Some(i.act);
                answer.engine = i.engine;
            }
            answer.flags = v.signals.flags;
            answer.scores = v.signals.scores;
            if let Action::Block { message, .. } = v.action {
                answer.action = "block".into();
                answer.message = Some(message);
            }
            answer.rule = v.rule;
            answer.flagged = v.flag;
            answer.trial = v.trial.iter().map(|t| format!("{}: {:?}", t.rule, t.action).to_lowercase()).collect();
            answer.detail = v.detail;
        }
    }
    answer
}

/// Classifies one text with each engine asked for.
async fn try_text(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<IntentTry>,
) -> ApiResult<Json<IntentTryResult>> {
    if input.text.trim().is_empty() || input.text.chars().count() > MAX_TEXT {
        return Err(ApiError::invalid(format!("text holds 1 to {MAX_TEXT} characters")));
    }
    let (caller, b) = binding(&st, &p, &params)?;
    let mut answers = Vec::new();
    for engine in engines_of(&input.engines, &b) {
        match compile_for(&caller, &b, engine) {
            Ok(bound) => answers.push(ask(&st, &caller, &b, bound.as_ref(), engine, &input.text).await),
            Err(why) => answers.push(EngineAnswer {
                engine: engine.name().into(),
                intent: None,
                confidence: None,
                act: None,
                flags: BTreeMap::new(),
                scores: BTreeMap::new(),
                action: "allow".into(),
                message: None,
                rule: None,
                flagged: false,
                trial: Vec::new(),
                elapsed_ms: 0.0,
                detail: Value::Null,
                error: Some(why),
            }),
        }
    }
    Ok(Json(IntentTryResult { binding: b.id.clone(), answers }))
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let at = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[at.min(sorted.len() - 1)]
}

/// Classifies a labelled set with each engine and scores the answers.
async fn evaluate(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<IntentEvaluate>,
) -> ApiResult<Json<IntentEvaluation>> {
    if input.samples.is_empty() || input.samples.len() > MAX_SAMPLES {
        return Err(ApiError::invalid(format!("samples holds 1 to {MAX_SAMPLES} labelled texts")));
    }
    if input.samples.iter().any(|s| s.text.trim().is_empty() || s.text.chars().count() > MAX_TEXT) {
        return Err(ApiError::invalid(format!("every sample's text holds 1 to {MAX_TEXT} characters")));
    }
    let (caller, b) = binding(&st, &p, &params)?;
    let mut out = Vec::new();
    for engine in engines_of(&input.engines, &b) {
        let bound =
            compile_for(&caller, &b, engine).map_err(|why| ApiError::invalid(format!("{}: {why}", engine.name())))?;
        let mut answers: Vec<EngineAnswer> = Vec::with_capacity(input.samples.len());
        for chunk in input.samples.chunks(PARALLEL) {
            let asked = chunk.iter().map(|s| ask(&st, &caller, &b, bound.as_ref(), engine, &s.text));
            answers.extend(futures_util::future::join_all(asked).await);
        }
        let mut e = EngineEvaluation {
            engine: engine.name().into(),
            samples: input.samples.len() as u32,
            correct: 0,
            accuracy: 0.0,
            errors: 0,
            per_intent: BTreeMap::new(),
            confusion: BTreeMap::new(),
            latency_p50_ms: 0.0,
            latency_p95_ms: 0.0,
            wrong: Vec::new(),
        };
        let mut latencies = Vec::new();
        for (sample, answer) in input.samples.iter().zip(&answers) {
            let got = match (&answer.error, &answer.intent) {
                (Some(_), _) | (None, None) => {
                    e.errors += 1;
                    "error".to_owned()
                }
                (None, Some(i)) => {
                    latencies.push(answer.elapsed_ms);
                    i.clone()
                }
            };
            e.per_intent.entry(sample.intent.clone()).or_default().support += 1;
            if got != "error" {
                e.per_intent.entry(got.clone()).or_default().predicted += 1;
            }
            *e.confusion.entry(sample.intent.clone()).or_default().entry(got.clone()).or_default() += 1;
            if got == sample.intent {
                e.correct += 1;
                e.per_intent.entry(got).or_default().correct += 1;
            } else if e.wrong.len() < LISTED_WRONG {
                e.wrong.push(WrongAnswer {
                    text: sample.text.chars().take(120).collect(),
                    expected: sample.intent.clone(),
                    got,
                });
            }
        }
        for score in e.per_intent.values_mut() {
            score.precision = (score.predicted > 0).then(|| f64::from(score.correct) / f64::from(score.predicted));
            score.recall = (score.support > 0).then(|| f64::from(score.correct) / f64::from(score.support));
        }
        e.accuracy = f64::from(e.correct) / f64::from(e.samples);
        latencies.sort_by(f64::total_cmp);
        e.latency_p50_ms = percentile(&latencies, 0.5);
        e.latency_p95_ms = percentile(&latencies, 0.95);
        out.push(e);
    }
    Ok(Json(IntentEvaluation { binding: b.id.clone(), engines: out }))
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let base = collection_path(ScopeKind::Workspace, "plugins/intent");
    let try_in = api.schema::<IntentTry>();
    let try_out = api.schema::<IntentTryResult>();
    let eval_in = api.schema::<IntentEvaluate>();
    let eval_out = api.schema::<IntentEvaluation>();
    api.add(
        api.op(
            "post",
            &format!("{base}/try"),
            "intentTry",
            "plugins",
            "Classify a text with the workspace's intent binding, one engine or several (costs calls)",
        )
        .body(try_in)
        .ok(try_out),
    );
    api.add(
        api.op(
            "post",
            &format!("{base}/evaluate"),
            "intentEvaluate",
            "plugins",
            "Classify a labelled set with each engine and score the answers (costs a call per sample and engine)",
        )
        .body(eval_in)
        .ok(eval_out),
    );
    router.route(&format!("{base}/try"), post(try_text)).route(&format!("{base}/evaluate"), post(evaluate))
}
