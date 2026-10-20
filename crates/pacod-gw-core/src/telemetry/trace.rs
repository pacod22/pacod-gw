//! Trace export to an OpenTelemetry collector over OTLP/HTTP with JSON encoding.
//!
//! The core emits two kinds of spans: one per request and one per provider attempt, with the
//! GenAI semantic-convention attributes. Prompt and response content is never put in a span.
//! Export is a small batching task; it needs no OpenTelemetry SDK in the binary.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::crypto::random_bytes;

/// Trace context of a request: either continued from the caller's `traceparent` or started here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceContext {
    pub trace_id: [u8; 16],
    /// The caller's span, when the request carried a `traceparent`.
    pub parent_span_id: Option<[u8; 8]>,
    pub span_id: [u8; 8],
}

impl TraceContext {
    /// Continues the caller's trace when `traceparent` is a valid W3C header, starts a new
    /// one otherwise.
    pub fn from_traceparent(header: Option<&str>) -> Self {
        let parsed = header.and_then(|h| {
            let mut parts = h.trim().split('-');
            let (version, trace, span, _flags) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
            if version != "00" || trace.len() != 32 || span.len() != 16 {
                return None;
            }
            let mut t = [0u8; 16];
            let mut s = [0u8; 8];
            hex::decode_to_slice(trace, &mut t).ok()?;
            hex::decode_to_slice(span, &mut s).ok()?;
            (t != [0u8; 16] && s != [0u8; 8]).then_some((t, s))
        });
        match parsed {
            Some((trace_id, parent)) => Self { trace_id, parent_span_id: Some(parent), span_id: random_bytes() },
            None => Self { trace_id: random_bytes(), parent_span_id: None, span_id: random_bytes() },
        }
    }

    /// The header to send to the provider, so its side of the call joins the trace.
    pub fn traceparent(&self, span_id: [u8; 8]) -> String {
        format!("00-{}-{}-01", hex::encode(self.trace_id), hex::encode(span_id))
    }

    pub fn child(&self) -> [u8; 8] {
        random_bytes()
    }
}

#[derive(Debug, Clone)]
pub struct Span {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent_span_id: Option<[u8; 8]>,
    pub name: String,
    /// 2 = server (the request), 3 = client (a provider attempt).
    pub kind: u8,
    pub start: SystemTime,
    pub end: SystemTime,
    pub attributes: Vec<(&'static str, Value)>,
    pub error: Option<String>,
}

fn nanos(t: SystemTime) -> String {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).to_string()
}

fn attr(key: &str, v: &Value) -> Option<Value> {
    let value = match v {
        Value::String(s) if s.is_empty() => return None,
        Value::String(s) => json!({ "stringValue": s }),
        Value::Bool(b) => json!({ "boolValue": b }),
        Value::Number(n) if n.is_f64() => json!({ "doubleValue": n }),
        // OTLP/JSON carries 64-bit integers as strings.
        Value::Number(n) => json!({ "intValue": n.to_string() }),
        _ => return None,
    };
    Some(json!({ "key": key, "value": value }))
}

fn encode(span: &Span) -> Value {
    let mut out = json!({
        "traceId": hex::encode(span.trace_id),
        "spanId": hex::encode(span.span_id),
        "name": span.name,
        "kind": span.kind,
        "startTimeUnixNano": nanos(span.start),
        "endTimeUnixNano": nanos(span.end),
        "attributes": span.attributes.iter().filter_map(|(k, v)| attr(k, v)).collect::<Vec<_>>(),
        "status": match &span.error {
            Some(m) => json!({ "code": 2, "message": m }),
            None => json!({ "code": 1 }),
        },
    });
    if let Some(p) = span.parent_span_id {
        out["parentSpanId"] = json!(hex::encode(p));
    }
    out
}

fn envelope(instance_id: &str, spans: &[Span]) -> Value {
    json!({ "resourceSpans": [{
        "resource": { "attributes": [
            { "key": "service.name", "value": { "stringValue": "pacod-gw-core" } },
            { "key": "service.version", "value": { "stringValue": env!("CARGO_PKG_VERSION") } },
            { "key": "service.instance.id", "value": { "stringValue": instance_id } },
        ] },
        "scopeSpans": [{
            "scope": { "name": "pacod-gw" },
            "spans": spans.iter().map(encode).collect::<Vec<_>>(),
        }]
    }] })
}

/// Hands spans to the export task. Cheap to clone; a disabled tracer drops spans.
#[derive(Clone, Default)]
pub struct Tracer {
    tx: Option<mpsc::Sender<Span>>,
}

impl Tracer {
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    pub fn enabled(&self) -> bool {
        self.tx.is_some()
    }

    /// Never blocks: when the exporter is behind, the span is dropped.
    pub fn emit(&self, span: Span) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(span);
        }
    }

    /// Starts the export task. `endpoint` is the collector's OTLP/HTTP base URL, for example
    /// `http://pacod-otel:4318`.
    pub fn spawn(endpoint: &str, instance_id: String) -> Self {
        let (tx, mut rx) = mpsc::channel::<Span>(4096);
        let url = format!("{}/v1/traces", endpoint.trim_end_matches('/'));
        let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap_or_default();
        tokio::spawn(async move {
            let mut batch: Vec<Span> = Vec::with_capacity(256);
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                let flush = tokio::select! {
                    span = rx.recv() => match span {
                        Some(s) => { batch.push(s); batch.len() >= 256 }
                        None => break,
                    },
                    _ = tick.tick() => !batch.is_empty(),
                };
                if flush {
                    let body = envelope(&instance_id, &batch);
                    batch.clear();
                    if let Err(e) = client.post(&url).json(&body).send().await.and_then(|r| r.error_for_status()) {
                        tracing::debug!(error = %e, "trace export failed; spans dropped");
                    }
                }
            }
            if !batch.is_empty() {
                let _ = client.post(&url).json(&envelope(&instance_id, &batch)).send().await;
            }
        });
        Self { tx: Some(tx) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continues_a_valid_traceparent() {
        let h = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let c = TraceContext::from_traceparent(Some(h));
        assert_eq!(hex::encode(c.trace_id), "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(c.parent_span_id.map(hex::encode).as_deref(), Some("00f067aa0ba902b7"));
        assert_ne!(Some(c.span_id), c.parent_span_id);
        assert_eq!(
            c.traceparent(c.span_id),
            format!("00-4bf92f3577b34da6a3ce929d0e0e4736-{}-01", hex::encode(c.span_id))
        );
    }

    #[test]
    fn starts_a_new_trace_on_missing_or_malformed_headers() {
        for bad in [
            None,
            Some(""),
            Some("garbage"),
            Some("01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"), // unknown version
            Some("00-00000000000000000000000000000000-00f067aa0ba902b7-01"), // all-zero trace id
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01"), // all-zero span id
            Some("00-zzf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        ] {
            let c = TraceContext::from_traceparent(bad);
            assert!(c.parent_span_id.is_none(), "{bad:?}");
            assert_ne!(c.trace_id, [0u8; 16]);
        }
    }

    #[test]
    fn spans_are_encoded_as_otlp_json() {
        let start = UNIX_EPOCH + Duration::from_nanos(1_700_000_000_123_456_789);
        let span = Span {
            trace_id: [1; 16],
            span_id: [2; 8],
            parent_span_id: Some([3; 8]),
            name: "chat claude".into(),
            kind: 3,
            start,
            end: start + Duration::from_millis(5),
            attributes: vec![
                ("gen_ai.request.model", json!("claude")),
                ("gen_ai.usage.input_tokens", json!(812)),
                ("gw.gateway_delay_ms", json!(1.25)),
                ("stream", json!(true)),
                ("empty", json!("")),
            ],
            error: Some("upstream_timeout".into()),
        };
        let v = envelope("i-1", &[span]);
        let s = &v["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(s["traceId"], "01010101010101010101010101010101");
        assert_eq!(s["parentSpanId"], "0303030303030303");
        assert_eq!(s["startTimeUnixNano"], "1700000000123456789");
        assert_eq!(s["endTimeUnixNano"], "1700000000128456789");
        assert_eq!(s["status"]["code"], 2);
        let attrs = s["attributes"].as_array().unwrap();
        assert_eq!(attrs.len(), 4, "empty strings are left out");
        assert_eq!(attrs[1]["value"]["intValue"], "812");
        assert_eq!(attrs[2]["value"]["doubleValue"], 1.25);
        assert_eq!(attrs[3]["value"]["boolValue"], true);
    }

    #[test]
    fn a_disabled_tracer_accepts_and_drops() {
        let t = Tracer::disabled();
        assert!(!t.enabled());
        t.emit(Span {
            trace_id: [0; 16],
            span_id: [0; 8],
            parent_span_id: None,
            name: String::new(),
            kind: 2,
            start: UNIX_EPOCH,
            end: UNIX_EPOCH,
            attributes: vec![],
            error: None,
        });
    }
}
