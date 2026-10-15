//! The load generator: many concurrent calls through the gateway and, for comparison, the
//! same calls straight to the provider. The difference is what the gateway adds, measured
//! from outside; the gateway's own `x-gw-delay-ms` header is reported next to it, so the
//! two can be checked against each other (concept sections 4.5 and 14.2).
//!
//! The calls are made by the load agents' engine (`pacod-gw-load`), so `gw-load` and a load
//! test from the console measure the same way. Its percentiles come from histograms and are
//! within 0.8% of the exact ones.

use std::sync::Arc;
use std::time::Duration;

use pacod_gw_load::client::Ready;
use pacod_gw_load::contract::{AuthHeader, HttpVersion, Mode, Profile, RequestSpec, Stage};
use pacod_gw_load::engine::{self, Job, Live};
use pacod_gw_load::histogram::Histogram;
use serde_json::Value;
use tokio::sync::watch;

/// How long one call may take.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Longer than any series: a series ends when its calls are used up.
const FOREVER_S: u64 = 24 * 3600;

/// One series of calls.
#[derive(Debug, Clone)]
pub struct Plan {
    pub url: String,
    /// Sent as a Bearer token when given.
    pub key: Option<String>,
    pub body: Value,
    pub requests: usize,
    pub concurrency: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Percentiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

impl Percentiles {
    /// In milliseconds, from a histogram in microseconds.
    fn of(h: &Histogram) -> Percentiles {
        let ms = |us: f64| us / 1000.0;
        Percentiles {
            p50: ms(h.percentile(0.50)),
            p95: ms(h.percentile(0.95)),
            p99: ms(h.percentile(0.99)),
            max: ms(h.max as f64),
        }
    }
}

/// The result of one series.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub ok: usize,
    pub failed: usize,
    pub seconds: f64,
    /// Milliseconds until the first body byte.
    pub first_byte_ms: Percentiles,
    /// Milliseconds until the last byte.
    pub total_ms: Percentiles,
    /// The gateway's own delay figure, where the response carried one.
    pub gateway_delay_ms: Option<Percentiles>,
}

impl Outcome {
    pub fn per_second(&self) -> f64 {
        if self.seconds > 0.0 { (self.ok + self.failed) as f64 / self.seconds } else { 0.0 }
    }
}

/// Runs a series: `requests` calls, `concurrency` at a time from the start.
pub async fn run(http: &reqwest::Client, plan: &Plan) -> Outcome {
    let failed = Outcome { failed: plan.requests, ..Outcome::default() };
    let Ok(url) = reqwest::Url::parse(&plan.url) else { return failed };
    let spec = RequestSpec {
        method: "POST".into(),
        path: String::new(),
        headers: [("content-type".to_owned(), "application/json".to_owned())].into(),
        body: plan.body.to_string(),
        stream: false,
    };
    let Ok(ready) = Ready::new(&url, &spec, AuthHeader::Authorization, plan.key.as_deref().unwrap_or("")) else {
        return failed;
    };
    let target = plan.concurrency.max(1) as f64;
    let profile = Profile {
        mode: Mode::Concurrency,
        // A stage of 0 seconds jumps straight to the concurrency.
        stages: vec![Stage { duration_s: 0, target }, Stage { duration_s: FOREVER_S, target }],
        max_in_flight: None,
        timeout_s: TIMEOUT.as_secs(),
        max_calls: Some(plan.requests as u64),
        drain_s: TIMEOUT.as_secs(),
        http: HttpVersion::Http1,
        warmup_connections: 0,
    };
    let job = Arc::new(Job { http: http.clone(), ready, profile, timeout: TIMEOUT });
    let live = Live::new();
    let (_stop, never) = watch::channel(None);
    engine::run(job, live.clone(), never).await;
    let t = live.totals();
    Outcome {
        ok: t.ok as usize,
        failed: (t.completed - t.ok + t.errors.cancelled) as usize,
        seconds: live.elapsed().as_secs_f64(),
        first_byte_ms: Percentiles::of(&t.ttfb),
        total_ms: Percentiles::of(&t.total),
        gateway_delay_ms: (!t.gw_delay.is_empty()).then(|| Percentiles::of(&t.gw_delay)),
    }
}
