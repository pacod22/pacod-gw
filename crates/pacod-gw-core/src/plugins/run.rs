//! The hook: runs a workspace's plugins on a call before the gateway plans it
//! (`docs/plugin-concept.md`, sections 3.3 to 3.7 and 4.4).
//!
//! Enforcing bindings run before planning and the call waits for them, within their time.
//! Shadow bindings run alongside the provider call; the call never waits for them, and only the
//! writing of its usage event waits for their verdict.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use pacod_gw_events::UsageEvent;
use pacod_gw_plugin::{Action, ErrorKind, PluginError, RequestInput, Signals, TrialAction, Verdict};
use serde_json::{Value, json};

use super::ActiveBinding;
use super::host::CoreHost;
use crate::dataplane::Gw;
use crate::dataplane::auth::Caller;
use crate::dataplane::errors::GwError;
use crate::model::{OnError, PluginMode};
use crate::relay::CallRecorder;
use crate::telemetry::metrics;

/// What one binding answered about a call.
pub struct Ran {
    pub binding: Arc<ActiveBinding>,
    pub result: Result<Verdict, PluginError>,
    pub elapsed: Duration,
}

/// What a call's recorder carries for its plugins.
#[derive(Default)]
pub struct PluginState {
    /// Shadow bindings still at work. The usage event is written once they answer.
    pub pending: Option<tokio::task::JoinHandle<Vec<Ran>>>,
    /// The verdicts of the enforcing bindings, for the audit record.
    pub audit: Vec<Value>,
    /// A rule flagged the call: its audit record is captured.
    pub flagged: bool,
    /// The intent label routing and limits act on.
    pub act: Option<String>,
    /// The value of `x-gw-intent`, when a binding exposes the intent.
    pub expose: Option<String>,
}

impl std::fmt::Debug for PluginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginState")
            .field("pending", &self.pending.is_some())
            .field("flagged", &self.flagged)
            .field("act", &self.act)
            .finish_non_exhaustive()
    }
}

/// Runs the workspace's plugins on a call. `input` is built only when a plugin is bound.
///
/// Returns the signals of the enforcing bindings for routing and limits, or the error the call
/// is refused with: a block, or a binding that could not answer and must (`on_error: closed`).
pub async fn before_plan(
    gw: &Arc<Gw>,
    caller: &Caller,
    input: impl FnOnce() -> Option<RequestInput>,
    rec: &mut CallRecorder,
) -> Result<Option<Signals>, GwError> {
    let bindings = &caller.workspace.plugins;
    // The gateway's own calls (a plugin's model calls, the price tracker) are never looked at.
    if bindings.is_empty() || caller.internal.is_some() {
        return Ok(None);
    }
    let Some(mut input) = input() else { return Ok(None) };
    input.call_id = rec.request_id();
    let input = Arc::new(input);
    let traceparent = rec.trace.traceparent(rec.trace.span_id);
    let (enforce, shadow): (Vec<_>, Vec<_>) = bindings.iter().cloned().partition(|b| b.mode == PluginMode::Enforce);

    if !shadow.is_empty() {
        let (gw, caller, input, traceparent) = (gw.clone(), caller.clone(), input.clone(), traceparent.clone());
        rec.plugins.pending =
            Some(tokio::spawn(async move { run_all(&gw, &caller, &input, &shadow, &traceparent).await }));
    }
    if enforce.is_empty() {
        return Ok(None);
    }

    let started = Instant::now();
    let ran = run_all(gw, caller, &input, &enforce, &traceparent).await;
    rec.timeline.plugin_spent(started.elapsed());
    record(&ran, &mut rec.event);
    rec.plugins.audit = ran.iter().map(audit_entry).collect();

    // A block wins over everything; among blocks the binding with the lowest priority (the
    // first: bindings are sorted) gives the message.
    for r in &ran {
        if let Ok(Verdict { action: Action::Block { code, message }, .. }) = &r.result {
            return Err(GwError::plugin_blocked(code, message.clone()));
        }
    }
    if let Some((err, b)) = ran
        .iter()
        .find_map(|r| r.result.as_ref().err().map(|e| (e, &r.binding)))
        .filter(|(_, b)| b.on_error == OnError::Closed)
    {
        return Err(refusal(&b.plugin, err));
    }

    let mut signals = Signals::default();
    for r in &ran {
        let Ok(v) = &r.result else { continue };
        if signals.intent.is_none() {
            signals.intent = v.signals.intent.clone();
            if r.binding.expose {
                rec.plugins.expose = v.signals.intent.as_ref().map(|i| i.label.clone());
            }
        }
        for (k, x) in &v.signals.flags {
            signals.flags.entry(k.clone()).or_insert(*x);
        }
        for (k, x) in &v.signals.scores {
            signals.scores.entry(k.clone()).or_insert(*x);
        }
        rec.plugins.flagged |= v.flag;
    }
    rec.plugins.act = signals.act().map(str::to_owned);
    Ok(Some(signals))
}

/// The error an enforcing binding refuses a call with when it could not give a verdict.
fn refusal(plugin: &str, e: &PluginError) -> GwError {
    match &e.kind {
        ErrorKind::Limited(r) if r.kind == "quota_exceeded" => GwError::quota_exceeded(r.message.clone()),
        ErrorKind::Limited(r) => GwError::rate_limited(r.message.clone(), r.retry_after_seconds.unwrap_or(1)),
        k if k.is_configuration() => GwError::plugin_misconfigured(format!(
            "the plugin \"{plugin}\" must check every call of this workspace and is not set up to: {}",
            k.name()
        )),
        k => GwError::plugin_unavailable(format!(
            "the plugin \"{plugin}\" must check every call of this workspace and could not check this one: {}",
            k.name()
        )),
    }
}

async fn run_all(
    gw: &Arc<Gw>,
    caller: &Caller,
    input: &RequestInput,
    bindings: &[Arc<ActiveBinding>],
    traceparent: &str,
) -> Vec<Ran> {
    join_all(bindings.iter().map(|b| run_one(gw, caller, input, b, traceparent))).await
}

async fn run_one(
    gw: &Arc<Gw>,
    caller: &Caller,
    input: &RequestInput,
    binding: &Arc<ActiveBinding>,
    traceparent: &str,
) -> Ran {
    let started = Instant::now();
    let result = match &binding.bound {
        Err(why) => Err(PluginError::new(ErrorKind::NotBuilt, why.clone())),
        Ok(bound) => {
            let host =
                CoreHost::new(gw.clone(), caller.clone(), &binding.plugin, &input.call_id, traceparent.to_owned());
            match tokio::time::timeout(binding.timeout, bound.on_request(input, &host)).await {
                Ok(r) => r,
                Err(_) => Err(PluginError::new(
                    ErrorKind::Timeout,
                    format!("no answer within {} ms", binding.timeout.as_millis()),
                )),
            }
        }
    };
    let elapsed = started.elapsed();
    if let Err(e) = &result {
        tracing::warn!(plugin = %binding.plugin, binding = %binding.id, error = %e, "a plugin gave no verdict");
    }
    Ran { binding: binding.clone(), result, elapsed }
}

/// How strongly a verdict acted, to pick the one the event records.
fn verdict_name(mode: PluginMode, v: &Verdict) -> (u8, &'static str) {
    let blocks = matches!(v.action, Action::Block { .. });
    let trial = |a: TrialAction| v.trial.iter().any(|t| t.action == a);
    match mode {
        PluginMode::Enforce if blocks => (5, "block"),
        PluginMode::Enforce if v.flag => (4, "flag"),
        _ if blocks || trial(TrialAction::Block) => (3, "would_block"),
        _ if v.flag || trial(TrialAction::Flag) => (2, "would_flag"),
        _ => (1, "allow"),
    }
}

/// Writes what the bindings found onto the usage event, and counts it in the metrics.
pub fn record(ran: &[Ran], event: &mut UsageEvent) {
    let mut strongest = (0u8, "");
    for r in ran {
        let engine = match &r.result {
            Ok(v) => v.signals.intent.as_ref().map_or("", |i| i.engine.as_str()),
            Err(_) => "",
        };
        match &r.result {
            Ok(v) => {
                let name = verdict_name(r.binding.mode, v);
                strongest = strongest.max(name);
                metrics::record_plugin(&r.binding.plugin, engine, name.1, None, r.elapsed);
                if let Some(i) = &v.signals.intent
                    && event.intent.is_empty()
                {
                    event.intent = i.label.clone();
                    event.intent_confidence = i.confidence;
                    event.intent_engine = i.engine.clone();
                }
                if let Some(i) = &v.signals.intent {
                    metrics::record_plugin_cache(&r.binding.plugin, i.cached);
                }
                if v.flag && !v.alerts.is_empty() && r.binding.mode == PluginMode::Enforce {
                    for rule in &v.alerts {
                        metrics::record_plugin_flagged(&r.binding.plugin, rule);
                    }
                }
            }
            Err(e) => {
                metrics::record_plugin(&r.binding.plugin, engine, "", Some(e.kind.name()), r.elapsed);
                if event.plugin_error.is_empty() {
                    event.plugin_error = e.kind.name().to_owned();
                }
            }
        }
    }
    if !strongest.1.is_empty() {
        event.plugin_verdict = strongest.1.to_owned();
    }
}

/// One binding's verdict as the audit record keeps it.
pub fn audit_entry(r: &Ran) -> Value {
    let b = &r.binding;
    let mut entry = json!({
        "binding": b.id,
        "plugin": b.plugin,
        "version": b.manifest.map(|m| m.version),
        "mode": b.mode.name(),
        "elapsed_ms": (r.elapsed.as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0,
    });
    match &r.result {
        Ok(v) => entry["verdict"] = serde_json::to_value(v).unwrap_or(Value::Null),
        Err(e) => entry["error"] = json!({ "kind": e.kind.name(), "message": e.message }),
    }
    entry
}
