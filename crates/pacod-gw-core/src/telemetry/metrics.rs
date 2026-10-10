//! Prometheus metrics (concept section 14.7). The series have no key or end-user labels, and
//! of the caller's attribution only `complexity` is a label: its values are a closed set. Agent,
//! user and task are reported from the usage events instead.

use std::sync::OnceLock;
use std::time::Duration;

use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use metrics_util::MetricKindMask;
use pacod_gw_events::UsageEvent;

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Buckets for the gateway delay, in seconds: from 50 microseconds to one second.
const DELAY_BUCKETS: &[f64] =
    &[0.00005, 0.0001, 0.00025, 0.0005, 0.001, 0.002, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 1.0];
/// Buckets for whole calls, in seconds.
const CALL_BUCKETS: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0];

/// A gauge that is no longer set leaves the output after this time. Every gauge is refreshed
/// every few seconds (see `app.rs`), so what goes are the series of a limit, a model or a
/// provider that no longer exists. Without this a deleted limit would hold its last value,
/// and its alert, until the process restarts.
const GAUGE_IDLE: Duration = Duration::from_secs(60);

/// Installs the recorder once per process and returns the handle that renders `/metrics`.
pub fn install() -> PrometheusHandle {
    HANDLE
        .get_or_init(|| {
            let handle = PrometheusBuilder::new()
                .idle_timeout(MetricKindMask::GAUGE, Some(GAUGE_IDLE))
                .set_buckets_for_metric(Matcher::Full("gw_delay_seconds".into()), DELAY_BUCKETS)
                .and_then(|b| {
                    b.set_buckets_for_metric(Matcher::Full("gw_request_duration_seconds".into()), CALL_BUCKETS)
                })
                .and_then(|b| b.set_buckets_for_metric(Matcher::Full("gw_first_byte_seconds".into()), CALL_BUCKETS))
                .expect("bucket lists are not empty")
                .install_recorder()
                .expect("the metrics recorder is installed once");
            // Without the exporter's own HTTP listener, histogram upkeep is ours to run.
            let upkeep = handle.clone();
            std::thread::Builder::new()
                .name("metrics-upkeep".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(Duration::from_secs(5));
                        upkeep.run_upkeep();
                    }
                })
                .expect("thread spawn");
            handle
        })
        .clone()
}

fn ms_to_s(ms: f32) -> f64 {
    f64::from(ms) / 1000.0
}

/// Records the series derived from a finished call.
pub fn record_call(e: &UsageEvent) {
    let org = e.organisation_id.clone();
    let ws = e.workspace_id.clone();
    let provider = e.provider.clone();
    let model = if e.model_served.is_empty() { e.model_requested.clone() } else { e.model_served.clone() };
    let mode = e.mode.clone();
    // An empty label is no label in Prometheus: a call without a declared complexity says so.
    let complexity = if e.complexity.is_empty() { "none".to_owned() } else { e.complexity.clone() };

    counter!("gw_requests_total",
        "organisation" => org.clone(), "workspace" => ws.clone(), "provider" => provider.clone(),
        "model" => model.clone(), "mode" => mode.clone(), "outcome" => e.status.clone(),
        "complexity" => complexity.clone())
    .increment(1);

    // Attempts on models the call moved on from are failovers, counted on their own.
    let retries = e.attempts.saturating_sub(1).saturating_sub(e.failovers);
    if retries > 0 {
        counter!("gw_retries_total", "provider" => provider.clone(), "model" => model.clone())
            .increment(u64::from(retries));
    }

    for (kind, n) in [
        ("input", e.usage.input_tokens),
        ("output", e.usage.output_tokens),
        ("cache_read", e.usage.cache_read_tokens),
        ("cache_write", e.usage.cache_write_tokens),
        ("reasoning", e.usage.reasoning_tokens),
    ] {
        if n > 0 {
            counter!("gw_tokens_total",
                "organisation" => org.clone(), "workspace" => ws.clone(), "provider" => provider.clone(),
                "model" => model.clone(), "kind" => kind)
            .increment(n);
        }
    }

    if let Some(cost) = e.cost
        && cost > 0.0
    {
        // Counters are integers: cost is kept in micro-units of the currency.
        counter!("gw_cost_micros_total",
            "organisation" => org.clone(), "workspace" => ws.clone(), "provider" => provider.clone(),
            "model" => model.clone(), "currency" => e.currency.clone())
        .increment((cost * 1_000_000.0).round() as u64);
    }

    if e.price_missing {
        // A call that could not be priced: the model has no price in effect (concept section 13.2).
        counter!("gw_price_missing_total", "provider" => provider.clone(), "model" => model.clone()).increment(1);
    }

    histogram!("gw_request_duration_seconds", "provider" => provider.clone(), "mode" => mode.clone(),
        "complexity" => complexity)
    .record(ms_to_s(e.timing.total_ms));
    if e.timing.upstream_first_byte_ms > 0.0 {
        histogram!("gw_first_byte_seconds", "provider" => provider.clone(), "mode" => mode.clone())
            .record(ms_to_s(e.timing.upstream_first_byte_ms));
    }
    for (stage, ms) in [("total", e.timing.gateway_delay_ms), ("pre", e.timing.pre_ms), ("post", e.timing.post_ms)] {
        histogram!("gw_delay_seconds", "provider" => provider.clone(), "mode" => mode.clone(), "stage" => stage)
            .record(ms_to_s(ms));
    }
    // Waiting for a connection and for retries happen only on the way to a provider.
    if e.attempts > 0 {
        for (stage, ms) in [("connect", e.timing.connect_ms), ("retry", e.timing.retry_ms)] {
            histogram!("gw_delay_seconds", "provider" => provider.clone(), "mode" => mode.clone(), "stage" => stage)
                .record(ms_to_s(ms));
        }
    }
}

/// A call moved on from a model to the next one its routing chose.
pub fn record_failover(provider: &str, model: &str, reason: &'static str) {
    counter!("gw_route_failovers_total", "provider" => provider.to_owned(), "model" => model.to_owned(),
        "reason" => reason)
    .increment(1);
}

/// 1 while routing keeps calls away from a model because it failed (its circuit breaker).
pub fn set_breaker_open(provider: &str, model: &str, open: bool) {
    gauge!("gw_breaker_open", "provider" => provider.to_owned(), "model" => model.to_owned()).set(if open {
        1.0
    } else {
        0.0
    });
}

/// A CEL condition of a routing scheme failed to evaluate; its rule did not match.
pub fn record_cel_error(scheme: &str) {
    counter!("gw_route_cel_errors_total", "scheme" => scheme.to_owned()).increment(1);
}

/// A request refused before it reached a provider.
pub fn record_rejection(reason: &'static str) {
    counter!("gw_rejections_total", "reason" => reason).increment(1);
}

/// A call of this mode started (`proxy` or `managed`): counted in `gw_streams_in_flight`
/// until [`call_finished`] with the same mode.
pub fn call_started(mode: &'static str) {
    gauge!("gw_streams_in_flight", "mode" => mode).increment(1.0);
}

pub fn call_finished(mode: &'static str) {
    gauge!("gw_streams_in_flight", "mode" => mode).decrement(1.0);
}

pub fn set_config(instance: &str, version: i64, lag_versions: i64) {
    gauge!("gw_config_version", "instance" => instance.to_owned()).set(version as f64);
    gauge!("gw_config_apply_lag_versions", "instance" => instance.to_owned()).set(lag_versions as f64);
}

/// How much of an organisation's or a workspace's limit is used: 1 is all of it. `workspace`
/// is empty for a limit that counts the whole organisation.
pub fn set_limit_used(organisation: &str, workspace: &str, limit: &str, ratio: f64) {
    gauge!("gw_limit_used_ratio", "organisation" => organisation.to_owned(), "workspace" => workspace.to_owned(), "limit" => limit.to_owned())
        .set(ratio);
}

/// Days since the price in effect for a model was set or last confirmed by the price tracker.
pub fn set_price_age(model: &str, days: f64) {
    gauge!("gw_price_age_days", "model" => model.to_owned()).set(days);
}

/// The price job's `stale_days`: after how many days without a check a price counts as stale.
/// The alert rule compares `gw_price_age_days` with it.
pub fn set_price_stale_threshold(days: u32) {
    gauge!("gw_price_stale_threshold_days").set(f64::from(days));
}

/// The target for the gateway delay's 99th percentile per mode; the alert rule compares
/// against it.
pub fn set_delay_targets(proxy_ms: f64, managed_ms: f64) {
    gauge!("gw_delay_target_seconds", "mode" => "proxy").set(proxy_ms / 1000.0);
    gauge!("gw_delay_target_seconds", "mode" => "managed").set(managed_ms / 1000.0);
}

/// The share of recent calls to a provider and model that succeeded.
pub fn set_upstream_health(provider: &str, model: &str, success_ratio: f64) {
    gauge!("gw_upstream_health", "provider" => provider.to_owned(), "model" => model.to_owned()).set(success_ratio);
}

/// Usage events or audit records that wait to be delivered to the analytics service, in bytes.
pub fn set_spool(instance: &str, kind: &'static str, bytes: u64) {
    let name = if kind == "audit" { "gw_audit_spool_bytes" } else { "gw_spool_bytes" };
    gauge!(name, "instance" => instance.to_owned()).set(bytes as f64);
}

pub fn record_dropped(instance: &str, kind: &'static str, n: u64) {
    let name = if kind == "audit" { "gw_audit_records_dropped_total" } else { "gw_events_dropped_total" };
    counter!(name, "instance" => instance.to_owned()).increment(n);
}

/// 1 while another running process presents this instance's id.
pub fn set_instance_conflict(instance: &str, conflict: bool) {
    gauge!("gw_instance_id_conflict", "instance" => instance.to_owned()).set(if conflict { 1.0 } else { 0.0 });
}

/// How many running instances serve the data plane, as this instance last counted them. A
/// key's concurrency cap is shared out over them.
pub fn set_data_plane_instances(instance: &str, n: usize) {
    gauge!("gw_cluster_data_plane_instances", "instance" => instance.to_owned()).set(n as f64);
}
