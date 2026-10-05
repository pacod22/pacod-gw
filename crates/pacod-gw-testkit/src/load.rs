//! The load generator: many concurrent calls through the gateway and, for comparison, the
//! same calls straight to the provider. The difference is what the gateway adds, measured
//! from outside; the gateway's own `x-gw-delay-ms` header is reported next to it, so the
//! two can be checked against each other (concept sections 4.5 and 14.2).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::Semaphore;

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

/// What one call took.
#[derive(Debug, Clone, Copy)]
struct Sample {
    /// Until the first byte of the response body.
    first_byte: Duration,
    total: Duration,
    /// The gateway's own figure, from `x-gw-delay-ms`.
    gateway_delay_ms: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Percentiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

/// The value below which `q` of the sorted samples lie (nearest rank).
pub fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

pub fn percentiles(mut values: Vec<f64>) -> Percentiles {
    values.sort_by(f64::total_cmp);
    Percentiles {
        p50: percentile(&values, 0.50),
        p95: percentile(&values, 0.95),
        p99: percentile(&values, 0.99),
        max: values.last().copied().unwrap_or(0.0),
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

async fn one(http: &reqwest::Client, plan: &Plan) -> Option<Sample> {
    let started = Instant::now();
    let mut req = http.post(&plan.url).header("content-type", "application/json").body(plan.body.to_string());
    if let Some(k) = &plan.key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let gateway_delay_ms =
        resp.headers().get("x-gw-delay-ms").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let mut body = resp.bytes_stream();
    let mut first_byte = None;
    while let Some(chunk) = body.next().await {
        chunk.ok()?;
        first_byte.get_or_insert_with(|| started.elapsed());
    }
    let total = started.elapsed();
    Some(Sample { first_byte: first_byte.unwrap_or(total), total, gateway_delay_ms })
}

/// Runs a series: `requests` calls, at most `concurrency` at a time.
pub async fn run(http: &reqwest::Client, plan: &Plan) -> Outcome {
    let gate = Arc::new(Semaphore::new(plan.concurrency.max(1)));
    let plan = Arc::new(plan.clone());
    let started = Instant::now();
    let mut tasks = Vec::with_capacity(plan.requests);
    for _ in 0..plan.requests {
        let (gate, plan, http) = (gate.clone(), plan.clone(), http.clone());
        tasks.push(tokio::spawn(async move {
            let _permit = gate.acquire_owned().await.ok()?;
            one(&http, &plan).await
        }));
    }
    let mut samples = Vec::with_capacity(tasks.len());
    let mut failed = 0;
    for t in tasks {
        match t.await {
            Ok(Some(s)) => samples.push(s),
            _ => failed += 1,
        }
    }
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let delays: Vec<f64> = samples.iter().filter_map(|s| s.gateway_delay_ms).collect();
    Outcome {
        ok: samples.len(),
        failed,
        seconds: started.elapsed().as_secs_f64(),
        first_byte_ms: percentiles(samples.iter().map(|s| ms(s.first_byte)).collect()),
        total_ms: percentiles(samples.iter().map(|s| ms(s.total)).collect()),
        gateway_delay_ms: (!delays.is_empty()).then(|| percentiles(delays)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.50), 50.0);
        assert_eq!(percentile(&v, 0.99), 99.0);
        assert_eq!(percentile(&v, 1.0), 100.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
        assert_eq!(percentile(&[7.0], 0.99), 7.0);
        let p = percentiles(vec![3.0, 1.0, 2.0]);
        assert_eq!((p.p50, p.max), (2.0, 3.0));
    }
}
