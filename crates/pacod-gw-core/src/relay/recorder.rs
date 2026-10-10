//! Settles a call exactly once: usage, cost, limits, timing, metrics and the usage event.
//!
//! The recorder is created right after authentication and travels with the call. When the
//! response body ends, when the handler fails, or when the client goes away and the body is
//! dropped, the recorder settles. That is what guarantees one event per call, including calls
//! that were cancelled half way.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Instant, SystemTime};

use pacod_gw_events::usage::{status, usage_source};
use pacod_gw_events::{Usage, UsageEvent};
use serde_json::json;

use super::audit::{AuditCapture, outcome_of};
use super::timeline::Timeline;
use crate::cost::{Costed, cost};
use crate::dataplane::Gw;
use crate::extract::{Extracted, UsageTap, estimate};
use crate::limits::Applicable;
use crate::model::{Price, UsagePolicy};
use crate::telemetry::metrics;
use crate::telemetry::trace::{Span, TraceContext};

/// How a call is priced (concept section 13.2).
#[derive(Debug, Clone, Default)]
pub enum Pricing {
    /// No repository model is known for the call, for example a relayed path that names none.
    #[default]
    Unknown,
    /// The model is marked free. Carries the currency the zero is reported in.
    Free(String),
    Priced(Arc<Price>),
    /// The model has no price in effect: cost stays empty and the event is flagged.
    Missing,
}

/// One of a key's concurrent calls. Gives the slot back when dropped.
pub struct ConcurrencySlot(Arc<AtomicU32>);

impl ConcurrencySlot {
    /// Takes a slot unless `max` calls are already in progress.
    pub fn take(counter: &Arc<AtomicU32>, max: u32) -> Option<Self> {
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            if current >= max {
                return None;
            }
            match counter.compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => return Some(Self(counter.clone())),
                Err(seen) => current = seen,
            }
        }
    }
}

impl Drop for ConcurrencySlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct CallRecorder {
    gw: Arc<Gw>,
    pub event: UsageEvent,
    pub timeline: Timeline,
    /// Reads usage from the response as it is relayed.
    pub tap: Option<UsageTap>,
    pub usage_policy: UsagePolicy,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub trace: TraceContext,
    pub pricing: Pricing,
    /// The price variant the response calls for, for example `batch`. Empty for standard rates.
    pub price_variant: String,
    /// The provider and the provider's id of the model resolution chose, to notice when the
    /// provider answers with another one.
    pub served: Option<(String, String)>,
    /// Where each parameter's value came from, for the audit record.
    pub param_sources: std::collections::BTreeMap<String, &'static str>,
    /// The limits the call was admitted under. They are settled with its usage and cost.
    pub limits: Vec<Applicable>,
    /// Held until the call has ended, for keys with a concurrency cap.
    pub slot: Option<ConcurrencySlot>,
    /// Audit mode: the full record of the call, written when it settles.
    pub audit: Option<Box<AuditCapture>>,
    /// The value of `x-gw-route`, for a call a routing scheme decided.
    pub route_header: Option<http::HeaderValue>,
    /// Counts the call in flight at the model that serves it, for routing.
    pub in_flight: Option<crate::routing::InFlight>,
    /// Every provider attempt, for a child span each.
    pub attempt_log: Vec<AttemptRecord>,
    /// The mode the call is counted under in `gw_streams_in_flight`.
    counted_mode: Option<&'static str>,
    settled: bool,
}

/// One attempt to call a provider, as its span reports it.
#[derive(Debug, Clone)]
pub struct AttemptRecord {
    pub span_id: [u8; 8],
    pub provider: String,
    pub model: String,
    pub start: SystemTime,
    /// When the provider's response head arrived or the attempt failed.
    pub end: SystemTime,
    /// The provider's status; 0 when no response arrived.
    pub status: u16,
    /// The provider's id of the request, from its response headers.
    pub request_id: String,
    pub error: Option<String>,
}

impl CallRecorder {
    pub fn new(gw: Arc<Gw>, trace: TraceContext) -> Self {
        gw.live.started();
        let mut event = UsageEvent::new(uuid::Uuid::now_v7(), gw.boot.instance_id.clone());
        // Empty means "not decided yet"; a call that ends without an outcome was cancelled.
        event.status = String::new();
        Self {
            gw,
            event,
            timeline: Timeline::new(),
            tap: None,
            usage_policy: UsagePolicy::Estimate,
            request_bytes: 0,
            response_bytes: 0,
            trace,
            pricing: Pricing::Unknown,
            price_variant: String::new(),
            served: None,
            param_sources: Default::default(),
            limits: Vec::new(),
            slot: None,
            audit: None,
            route_header: None,
            in_flight: None,
            attempt_log: Vec::new(),
            counted_mode: None,
            settled: false,
        }
    }

    /// Counts a request the gateway refused in the live figures' alert window.
    pub fn record_rejection(&self, reason: &'static str) {
        self.gw.live.record_rejection(reason);
    }

    /// Sets the call's mode, `proxy` or `managed`, and counts it in flight under it.
    pub fn set_mode(&mut self, mode: &'static str) {
        self.event.mode = mode.to_owned();
        if self.counted_mode.is_none() {
            metrics::call_started(mode);
            self.counted_mode = Some(mode);
        }
    }

    pub fn request_id(&self) -> String {
        format!("gw_{}", self.event.id.simple())
    }

    /// Records how the call ended. The first outcome wins over a later "cancelled".
    pub fn outcome(&mut self, status: &str, http_status: u16, error_type: &str, message: &str) {
        self.event.status = status.to_owned();
        self.event.http_status = http_status;
        self.event.error_type = error_type.to_owned();
        // Keep events small; the full provider error is in the audit record when audit mode is on.
        self.event.error_message = message.chars().take(500).collect();
    }

    /// Decides the outcome from the provider's HTTP status, unless one was recorded already.
    /// Called when the response body has been relayed completely.
    pub fn complete_from_http(&mut self) {
        if !self.event.status.is_empty() {
            return;
        }
        let s = self.event.http_status;
        match s {
            200..=399 => self.outcome(status::OK, s, "", ""),
            429 => self.outcome(status::ERROR, s, "upstream_rate_limited", "the provider's rate limit was reached"),
            _ => self.outcome(status::ERROR, s, "upstream_error", "the provider answered with an error"),
        }
    }

    /// The cost of `usage` at the call's price. `None` when no repository model is known.
    pub fn cost_of(&self, usage: &Usage) -> Option<Costed> {
        match &self.pricing {
            Pricing::Unknown => None,
            Pricing::Free(currency) => Some(Costed::free(currency)),
            Pricing::Priced(price) => Some(cost(price, usage, &self.price_variant)),
            Pricing::Missing => Some(Costed::unpriced()),
        }
    }

    /// Takes over what was read from the provider's response: usage, the model that answered
    /// and the finish reason. The tap's result goes through here when the call settles; a
    /// handler that holds a whole response calls it directly.
    pub fn absorb(&mut self, found: Extracted) {
        if let Some(m) = found.model.filter(|m| !m.is_empty()) {
            self.provider_reported(&m);
            self.event.model_reported = m;
        }
        if let Some(t) = found.service_tier {
            self.price_variant = price_variant_for(&t);
        }
        let e = &mut self.event;
        if let Some(r) = found.finish_reason {
            e.finish_reason = r;
        }
        if found.has_usage {
            e.usage = found.usage;
            e.usage_source = usage_source::PROVIDER.to_owned();
        }
        if found.cut && e.status == status::OK {
            e.status = status::ERROR.to_owned();
            e.error_type = "stream".to_owned();
            e.error_message = "the provider's stream ended inside an event".to_owned();
        }
    }

    /// The provider names the model that answered. A model the repository resolved is reported
    /// under its own id; when the provider answered with another model of the repository (a
    /// fallback on its side), that model served the call and its price applies.
    fn provider_reported(&mut self, model: &str) {
        if self.event.model_served.is_empty() {
            self.event.model_served = model.to_owned();
            return;
        }
        let Some((provider, asked)) = &self.served else { return };
        if same_upstream_model(model, asked) {
            return;
        }
        let snap = self.gw.config.snapshot();
        if let Some(other) = snap.model_at_provider(provider, model) {
            self.event.model_served = other.doc.name.clone();
            self.pricing = crate::dataplane::common::pricing_for(&snap, other);
        }
    }

    /// Settles the call now.
    pub fn finish(mut self) {
        self.settle();
    }

    /// Drops the call without an event: for the gateway's own calls to a provider, such as
    /// asking about a batch, which are no workspace's usage.
    pub fn discard(mut self) {
        self.settled = true;
        self.gw.live.finished();
        if let Some(mode) = self.counted_mode.take() {
            metrics::call_finished(mode);
        }
    }

    fn settle(&mut self) {
        if self.settled {
            return;
        }
        self.settled = true;
        let now = Instant::now();

        if self.event.status.is_empty() {
            self.event.status = status::CANCELLED.to_owned();
            self.event.error_type = "cancelled".to_owned();
        }

        if let Some(tap) = self.tap.take() {
            self.absorb(tap.finish());
        }
        let reached_provider = self.event.attempts > 0 && self.event.http_status / 100 == 2;
        if self.event.usage_source != usage_source::PROVIDER {
            if reached_provider && self.usage_policy == UsagePolicy::Estimate {
                self.event.usage = estimate(self.request_bytes, self.response_bytes);
                self.event.usage_source = usage_source::ESTIMATED.to_owned();
            } else {
                self.event.usage_source = usage_source::MISSING.to_owned();
            }
        }
        if self.event.model_served.is_empty() {
            self.event.model_served = self.event.model_requested.clone();
        }

        // Cost is computed at settlement, from the price in effect when the call started.
        if self.event.usage_source != usage_source::MISSING
            && let Some(c) = self.cost_of(&self.event.usage)
        {
            let e = &mut self.event;
            e.cost = c.amount;
            e.currency = c.currency;
            e.price_version = c.price_version;
            e.price_variant = c.variant;
            e.price_missing = c.price_missing;
            // Reports and spend limits count in the reporting currency (concept section 13.1).
            if let Some(amount) = e.cost {
                let snap = self.gw.config.snapshot();
                e.reporting_currency = snap.settings.reporting_currency.clone();
                e.cost_reporting = snap.settings.to_reporting(amount, &e.currency);
            }
        }
        if !self.limits.is_empty() {
            // A cost without an exchange rate counts as it is; an alert names the currency.
            let spend = self.event.cost_reporting.or(self.event.cost);
            self.gw.limits.settle(&self.limits, &self.event.usage, spend);
        }
        self.slot = None;

        let e = &mut self.event;
        e.timing = self.timeline.timing(now);
        let ok = e.status == status::OK;
        self.gw.live.finished();
        self.gw.live.record(ok, e.timing.gateway_delay_ms);
        self.gw.live.record_window(&e.mode, &e.provider, &e.model_served, &e.status, e.timing.gateway_delay_ms);
        if e.attempts > 0 && !e.provider.is_empty() && e.status != status::CANCELLED {
            // Only calls that reached the provider say something about its health.
            self.gw.live.record_health(&e.provider, &e.model_served, ok, e.timing.upstream_first_byte_ms);
        }
        metrics::record_call(e);
        if let Some(mode) = self.counted_mode.take() {
            metrics::call_finished(mode);
        }

        if self.gw.tracer.enabled() {
            self.gw.tracer.emit(Span {
                trace_id: self.trace.trace_id,
                span_id: self.trace.span_id,
                parent_span_id: self.trace.parent_span_id,
                name: format!("{} {}", e.mode, e.model_served),
                kind: 2,
                start: self.timeline.wall_start,
                end: SystemTime::now(),
                attributes: vec![
                    ("gen_ai.system", json!(e.provider)),
                    ("gen_ai.request.model", json!(e.model_requested)),
                    ("gen_ai.response.model", json!(e.model_served)),
                    ("gen_ai.usage.input_tokens", json!(e.usage.input_tokens)),
                    ("gen_ai.usage.output_tokens", json!(e.usage.output_tokens)),
                    ("gen_ai.response.finish_reason", json!(e.finish_reason)),
                    ("gw.request_id", json!(format!("gw_{}", e.id.simple()))),
                    ("gw.workspace", json!(e.workspace_id)),
                    ("gw.mode", json!(e.mode)),
                    ("gw.template", json!(e.template)),
                    ("gw.agent_id", json!(e.agent_id)),
                    ("gw.task", json!(e.task)),
                    ("gw.complexity", json!(e.complexity)),
                    ("gw.attempts", json!(e.attempts)),
                    ("gw.gateway_delay_ms", json!(f64::from(e.timing.gateway_delay_ms))),
                    ("http.response.status_code", json!(e.http_status)),
                    ("gw.upstream_request_id", json!(e.upstream_request_id)),
                ],
                error: (!ok).then(|| e.error_type.clone()),
            });
            // A child span per provider attempt, with the provider's request id. The last
            // attempt that answered lasts until the call ends: it carried the response.
            let last = self.attempt_log.len();
            let end = SystemTime::now();
            for (i, a) in self.attempt_log.iter().enumerate() {
                let carried = i + 1 == last && a.error.is_none() && a.status / 100 == 2;
                self.gw.tracer.emit(Span {
                    trace_id: self.trace.trace_id,
                    span_id: a.span_id,
                    parent_span_id: Some(self.trace.span_id),
                    name: format!("attempt {} {}", i + 1, a.model),
                    kind: 3,
                    start: a.start,
                    end: if carried { end } else { a.end },
                    attributes: vec![
                        ("gen_ai.system", json!(a.provider)),
                        ("gen_ai.request.model", json!(a.model)),
                        ("gw.attempt", json!(i + 1)),
                        ("http.response.status_code", json!(a.status)),
                        ("gw.upstream_request_id", json!(a.request_id)),
                    ],
                    error: a.error.clone().or_else(|| (a.status >= 400).then(|| format!("HTTP {}", a.status))),
                });
            }
        }

        // The audit record is written first, so that the event names it only if it exists.
        if let Some(mut capture) = self.audit.take() {
            if !self.param_sources.is_empty() {
                capture.resolution["parameters"] = json!(self.param_sources);
            }
            e.audit_id = capture.id.clone();
            let record = capture.finish(e, self.timeline.marks(), outcome_of(e));
            let stored = serde_json::to_vec(&record).is_ok_and(|bytes| self.gw.audit_spool.try_write(bytes));
            // A spool counts what it drops itself; the metric follows that count. Without an
            // analytics service there is no spool to count it.
            if !self.gw.audit_spool.enabled() {
                metrics::record_dropped(&self.gw.boot.instance_id, "audit", 1);
            }
            if !stored || !self.gw.audit_spool.enabled() {
                e.audit_id.clear();
            }
        }

        match serde_json::to_vec(e) {
            // A dropped event is counted by the spool, and from there in the metric.
            Ok(record) => {
                self.gw.usage_spool.try_write(record);
            }
            Err(err) => tracing::error!(error = %err, "usage event could not be encoded"),
        }
    }
}

impl Drop for CallRecorder {
    fn drop(&mut self) {
        self.settle();
    }
}

/// Whether a provider's report names the model that was asked for: the same id, or that id
/// with a date the provider appends (`gpt-x-2026-08-06`, `claude-x-20260929`).
pub fn same_upstream_model(reported: &str, asked: &str) -> bool {
    let reported = reported.strip_prefix("models/").unwrap_or(reported);
    let dated = |long: &str, short: &str| {
        long.strip_prefix(short)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|date| !date.is_empty() && date.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
    };
    reported == asked || dated(reported, asked) || dated(asked, reported)
}

/// The price variant a provider's service tier calls for. The standard tiers use the standard
/// rates; any other is looked up by its name among the price's variants (`batch`, `priority`,
/// `flex`, …), and a price without it uses the standard rates.
pub fn price_variant_for(tier: &str) -> String {
    match tier {
        "" | "standard" | "default" | "auto" | "on_demand" => String::new(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_concurrency_slot_is_given_back_when_dropped() {
        let counter = Arc::new(AtomicU32::new(0));
        let a = ConcurrencySlot::take(&counter, 2).unwrap();
        let _b = ConcurrencySlot::take(&counter, 2).unwrap();
        assert!(ConcurrencySlot::take(&counter, 2).is_none());
        drop(a);
        assert!(ConcurrencySlot::take(&counter, 2).is_some());
        assert_eq!(counter.load(Ordering::Relaxed), 1, "the third slot was dropped at once");
    }

    #[test]
    fn a_dated_id_is_the_same_model_and_another_model_is_not() {
        assert!(same_upstream_model("gpt-x", "gpt-x"));
        assert!(same_upstream_model("gpt-x-2026-08-06", "gpt-x"));
        assert!(same_upstream_model("claude-x", "claude-x-20260929"));
        assert!(same_upstream_model("models/gemini-x", "gemini-x"));
        assert!(!same_upstream_model("gpt-x-mini", "gpt-x"), "a smaller model is another model");
        assert!(!same_upstream_model("claude-haiku", "claude-sonnet"));
    }

    #[test]
    fn service_tiers_select_price_variants() {
        for standard in ["standard", "default", "auto", ""] {
            assert_eq!(price_variant_for(standard), "");
        }
        assert_eq!(price_variant_for("batch"), "batch");
        assert_eq!(price_variant_for("priority"), "priority");
    }
}
