//! The log: one line per call, optionally one more with its bodies, and one per refused request.
//!
//! - `pacod_gw_lite::call`: every call that got as far as the gateway's accounting, which is
//!   every authenticated call: served, failed, refused by a limit or cancelled. Its fields come
//!   from the full gateway's usage event.
//! - `pacod_gw_lite::body`: the bodies the client sent and received, when `PGW_LOG_BODIES`
//!   asks for them (the full gateway's audit record, cut at `PGW_LOG_BODY_MAX_BYTES`).
//! - `pacod_gw_lite::request`: requests refused before that, a bad key above all. They carry
//!   the method, path, client address, status and the name part of the key, never the key.
//!
//! When the intent plugin is on, a call it looked at carries `intent`, `intent_confidence`,
//! `intent_engine`, `verdict` and `plugin_ms`, and each classification is a call line of its own
//! with `task` and the classified call's id as `parent_id`.
//!
//! In `json` format each line is one JSON object with the fields at the top level.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use pacod_gw_core::crypto::keys::{API_KEY_PREFIX, id_of};
use pacod_gw_core::dataplane::Gw;
use pacod_gw_core::relay::CallSink;
use pacod_gw_events::usage::status;
use pacod_gw_events::{AuditPart, AuditRecord, UsageEvent};
use tracing::field::{DisplayValue, display};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

use crate::config::LogFormat;

/// Sets up the log on standard output. `filter` is `RUST_LOG` as configured; unset logs at
/// `info`. A second call keeps the first set-up.
pub fn init(format: LogFormat, filter: Option<&str>) {
    // Colours only on a terminal: `docker logs` and log collectors get plain text.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let _ = tracing::subscriber::set_global_default(subscriber(format, filter, ansi, std::io::stdout));
}

/// The log's subscriber, writing to `writer`.
pub fn subscriber<W>(
    format: LogFormat,
    filter: Option<&str>,
    ansi: bool,
    writer: W,
) -> Box<dyn tracing::Subscriber + Send + Sync>
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let filter = filter.and_then(|f| EnvFilter::try_new(f).ok()).unwrap_or_else(|| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(ansi).with_writer(writer);
    match format {
        LogFormat::Json => Box::new(builder.json().flatten_event(true).with_current_span(false).finish()),
        LogFormat::Text => Box::new(builder.finish()),
    }
}

/// Writes the call and body lines.
pub struct LogSink;

/// A text value that is absent when empty, so that neither format shows empty fields. Written
/// as it is, without the quotes the text format puts around other strings.
fn some(s: &str) -> Option<DisplayValue<&str>> {
    (!s.is_empty()).then(|| display(s))
}

/// Milliseconds to a tenth.
fn ms(v: f32) -> f64 {
    (f64::from(v) * 10.0).round() / 10.0
}

/// A body as text: UTF-8 as it is, anything else as its size.
fn body_text(part: &AuditPart, record: &AuditRecord) -> String {
    let part = match part.same_as.as_str() {
        "" => part,
        // Stored once and named by the other part: the client's request as sent on, or the
        // provider's answer as relayed.
        "client_request" => &record.client_request,
        other => other
            .strip_prefix("provider_responses[")
            .and_then(|i| i.strip_suffix(']'))
            .and_then(|i| i.parse::<usize>().ok())
            .and_then(|i| record.provider_responses.get(i))
            .unwrap_or(part),
    };
    match part.body_encoding.as_str() {
        "utf8" | "" => part.body.clone(),
        other => format!("<{} bytes, {other}>", part.body_bytes),
    }
}

impl CallSink for LogSink {
    fn settled(&self, e: &UsageEvent, audit: Option<&AuditRecord>) {
        let request_id = format!("gw_{}", e.id.simple());
        let u = &e.usage;
        let route = match (e.routing_scheme.as_str(), e.routing_rule.as_str()) {
            ("", _) => None,
            (scheme, "") => Some(scheme.to_owned()),
            (scheme, rule) => Some(format!("{scheme}/{rule}")),
        };
        let models_tried = (e.models_tried.len() > 1).then(|| e.models_tried.join(","));
        let dropped = (!e.dropped_params.is_empty()).then(|| e.dropped_params.join(","));
        // The intent plugin's fields appear only on calls it looked at.
        let classified = !e.intent.is_empty();
        let plugin_ms = (e.timing.plugin_ms > 0.0).then(|| ms(e.timing.plugin_ms));
        macro_rules! call_line {
            ($level:ident) => {
                tracing::$level!(
                    target: "pacod_gw_lite::call",
                    request_id = %request_id,
                    key = %e.key_id,
                    mode = %e.mode,
                    interface = some(&e.interface),
                    path = some(&e.path),
                    model = %e.model_requested,
                    served = some(&e.model_served),
                    provider = some(&e.provider),
                    pool = some(&e.template),
                    route = route.as_deref().map(display),
                    status = %e.status,
                    http = e.http_status,
                    attempts = e.attempts,
                    failovers = e.failovers,
                    models_tried = models_tried.as_deref().map(display),
                    stream = e.stream,
                    finish_reason = some(&e.finish_reason),
                    input_tokens = u.input_tokens,
                    output_tokens = u.output_tokens,
                    cache_read_tokens = u.cache_read_tokens,
                    cache_write_tokens = u.cache_write_tokens,
                    reasoning_tokens = u.reasoning_tokens,
                    usage = %e.usage_source,
                    cost = e.cost,
                    currency = some(&e.currency),
                    price_missing = e.price_missing,
                    latency_ms = ms(e.timing.total_ms),
                    ttfb_ms = ms(e.timing.upstream_first_byte_ms),
                    gateway_ms = ms(e.timing.gateway_delay_ms),
                    error_type = some(&e.error_type),
                    error = some(&e.error_message),
                    upstream_request_id = some(&e.upstream_request_id),
                    end_user = some(&e.end_user),
                    session_id = some(&e.session_id),
                    dropped_params = dropped.as_deref().map(display),
                    intent = some(&e.intent),
                    intent_confidence = classified.then(|| (f64::from(e.intent_confidence) * 1000.0).round() / 1000.0),
                    intent_engine = some(&e.intent_engine),
                    verdict = some(&e.plugin_verdict),
                    plugin_error = some(&e.plugin_error),
                    plugin_ms = plugin_ms,
                    task = some(&e.task),
                    parent_id = some(&e.parent_id),
                    "call"
                )
            };
        }
        if e.status == status::OK || e.status == status::REJECTED || e.status == status::CANCELLED {
            call_line!(info);
        } else {
            call_line!(warn);
        }

        if let Some(r) = audit {
            tracing::info!(
                target: "pacod_gw_lite::body",
                request_id = %request_id,
                key = %e.key_id,
                audit_id = %r.audit_id,
                request = %body_text(&r.client_request, r),
                response = %body_text(&r.client_response, r),
                request_bytes = r.client_request.body_bytes,
                response_bytes = r.client_response.body_bytes,
                truncated = r.truncated,
                "body"
            );
        }
    }
}

/// The name part of a presented client key (`pacod` in `pgw_pacod_…`), never its secret.
fn key_hint(headers: &HeaderMap) -> Option<String> {
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()))
        .or_else(|| headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()))?;
    let id = id_of(presented.trim(), API_KEY_PREFIX)?;
    // A name, not something pasted in its place.
    (id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| id.to_owned())
}

/// Logs the requests the call log does not see: those answered without a request id. A
/// refusal (status 400 and above) is logged at `info`, anything else (model lists, health
/// checks) at `debug`.
pub async fn requests(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let client = gw.boot.trusted_proxies.client_ip(peer.ip(), req.headers());
    let hint = key_hint(req.headers());
    let res = next.run(req).await;
    // A refusal before the call's accounting carries the header empty.
    if res.headers().get("x-gw-request-id").is_some_and(|v| !v.is_empty()) {
        return res;
    }
    let status = res.status().as_u16();
    let error_type = res.headers().get("x-gw-error-type").and_then(|v| v.to_str().ok());
    if status >= 400 {
        tracing::info!(
            target: "pacod_gw_lite::request",
            method = %method,
            path = %path,
            client = %client,
            http = status,
            error_type = error_type.map(display),
            key = hint.as_deref().map(display),
            "rejected"
        );
    } else {
        tracing::debug!(target: "pacod_gw_lite::request", method = %method, path = %path, client = %client, http = status, "request");
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_hint_is_the_name_part_only() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer pgw_pacod_s3cretS3cretS3cret".parse().unwrap());
        assert_eq!(key_hint(&h).as_deref(), Some("pacod"));
        let mut h = HeaderMap::new();
        h.insert("x-api-key", "sk-ant-something".parse().unwrap());
        assert_eq!(key_hint(&h), None);
        let mut h = HeaderMap::new();
        h.insert("x-goog-api-key", "pgw_x-y_secret".parse().unwrap());
        assert_eq!(key_hint(&h), None, "only names the configuration can hold");
        assert_eq!(key_hint(&HeaderMap::new()), None);
    }
}
