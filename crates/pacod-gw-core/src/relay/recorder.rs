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
    settled: bool,
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
            limits: Vec::new(),
            slot: None,
            audit: None,
            route_header: None,
            in_flight: None,
            settled: false,
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
        let e = &mut self.event;
        // A model the repository resolved is reported under its own id.
        if e.model_served.is_empty()
            && let Some(m) = found.model
        {
            e.model_served = m;
        }
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

    /// Settles the call now.
    pub fn finish(mut self) {
        self.settle();
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
        }
        if !self.limits.is_empty() {
            self.gw.limits.settle(&self.limits, &self.event.usage, self.event.cost);
        }
        self.slot = None;

        let e = &mut self.event;
        e.timing = self.timeline.timing(now);
        let ok = e.status == status::OK;
        let in_flight = self.gw.live.finished();
        self.gw.live.record(ok, e.timing.gateway_delay_ms);
        if e.attempts > 0 && !e.provider.is_empty() && e.status != status::CANCELLED {
            // Only calls that reached the provider say something about its health.
            self.gw.live.record_health(&e.provider, &e.model_served, ok, e.timing.upstream_first_byte_ms);
        }
        metrics::record_call(e);
        metrics::set_streams_in_flight(in_flight);

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
        }

        // The audit record is written first, so that the event names it only if it exists.
        if let Some(capture) = self.audit.take() {
            e.audit_id = capture.id.clone();
            let record = capture.finish(e, self.timeline.marks(), outcome_of(e));
            let stored = serde_json::to_vec(&record).is_ok_and(|bytes| self.gw.audit_spool.try_write(bytes));
            if !stored || !self.gw.audit_spool.enabled() {
                metrics::record_dropped(&self.gw.boot.instance_id, "audit", 1);
                e.audit_id.clear();
            }
        }

        match serde_json::to_vec(e) {
            Ok(record) => {
                if !self.gw.usage_spool.try_write(record) {
                    metrics::record_dropped(&self.gw.boot.instance_id, "usage", 1);
                }
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
}
