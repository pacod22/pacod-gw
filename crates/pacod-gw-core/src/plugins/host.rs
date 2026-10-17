//! What the core does for a plugin: model calls on the workspace's behalf
//! (`docs/plugin-concept.md`, section 3.8).
//!
//! A plugin's calls are the gateway's own. They run as an internal caller of the classified
//! call's workspace, so they are accounted, limited and priced there, and plugins never look
//! at them. Their usage events name the classified call in `parent_id`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Response};
use bytes::Bytes;
use http_body_util::BodyExt;
use pacod_gw_events::Usage;
use pacod_gw_events::usage::{status, usage_source};
use pacod_gw_plugin::{BoxFuture, Host, HostError, Refused};
use serde_json::{Value, json};

use crate::dataplane::Gw;
use crate::dataplane::auth::{Caller, Internal};
use crate::dataplane::managed::{self, Interface};
use crate::model::{Family, ModelKind, Status, UsagePolicy};
use crate::relay::CallRecorder;
use crate::telemetry::trace::TraceContext;
use crate::upstream::client::SendError;

/// The largest answer a plugin's call may return.
const MAX_ANSWER: usize = 4 << 20;

pub struct CoreHost {
    gw: Arc<Gw>,
    caller: Caller,
    plugin: String,
    /// The request id of the call being looked at.
    parent: String,
    /// The call's trace, so that a plugin's calls show as its children.
    traceparent: String,
}

impl CoreHost {
    pub fn new(gw: Arc<Gw>, caller: Caller, plugin: &str, parent: &str, traceparent: String) -> Self {
        Self { gw, caller, plugin: plugin.to_owned(), parent: parent.to_owned(), traceparent }
    }

    /// The caller a plugin's call runs as: the workspace, without one of its keys.
    fn internal_caller(&self) -> Caller {
        let mut c = crate::mgmt::playground::caller_for(
            self.caller.snapshot.clone(),
            self.caller.workspace.clone(),
            &format!("plugin:{}", self.plugin),
        );
        c.internal = Some(Internal::Plugin { plugin: self.plugin.clone(), parent: self.parent.clone() });
        c
    }

    async fn chat(&self, mut request: Value) -> Result<Value, HostError> {
        let Some(top) = request.as_object_mut() else {
            return Err(HostError::Invalid("a plugin's request must be a JSON object".into()));
        };
        // The gateway's own work, told apart from the workspace's in reports.
        let gateway = top.entry("gateway").or_insert_with(|| json!({}));
        if let Some(g) = gateway.as_object_mut() {
            g.insert("task".into(), json!(format!("plugin.{}", self.plugin)));
        }
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        let trace = TraceContext::from_traceparent(Some(&self.traceparent));
        let response = managed::serve(
            self.gw.clone(),
            self.internal_caller(),
            Instant::now(),
            trace,
            headers,
            Body::from(request.to_string()),
            Interface::Native,
            None,
        )
        .await;
        answer(response).await
    }
}

/// The JSON of a data-plane answer, or what went wrong, from its status and its error type.
async fn answer(response: Response<Body>) -> Result<Value, HostError> {
    let status = response.status().as_u16();
    let retry_after =
        response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u32>().ok());
    let bytes = axum::body::to_bytes(response.into_body(), MAX_ANSWER)
        .await
        .map_err(|e| HostError::Upstream(format!("the answer could not be read: {e}")))?;
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        return Ok(body);
    }
    let kind = body.pointer("/error/type").and_then(Value::as_str).unwrap_or_default().to_owned();
    let message = body.pointer("/error/message").and_then(Value::as_str).unwrap_or("the call failed").to_owned();
    Err(match (status, kind.as_str()) {
        (402 | 429, "rate_limited" | "quota_exceeded") => {
            HostError::Limited(Refused { status, kind, message, retry_after_seconds: retry_after })
        }
        (
            _,
            "model_not_found"
            | "template_not_found"
            | "model_retired"
            | "provider_unavailable"
            | "not_allowed"
            | "own_credential_required",
        ) => HostError::Unavailable(message),
        (400..=499, _) => HostError::Invalid(format!("{kind}: {message}")),
        _ => HostError::Upstream(format!("{status} {kind}: {message}")),
    })
}

impl Host for CoreHost {
    fn complete<'a>(&'a self, request: Value) -> BoxFuture<'a, Result<Value, HostError>> {
        Box::pin(self.chat(request))
    }

    fn decide<'a>(&'a self, model: &'a str, request: Value) -> BoxFuture<'a, Result<Value, HostError>> {
        Box::pin(self.decision(model, request))
    }
}

/// How long a decision call may take at most. The binding's own time is usually shorter and
/// ends the call first.
const DECISION_TIMEOUT: Duration = Duration::from_secs(30);

impl CoreHost {
    /// A call to a decision model's provider (TypeSafe's System One API), recorded as a usage
    /// event of the workspace like a managed call: priced, counted against its token and spend
    /// limits, linked to the call the plugin looks at.
    async fn decision(&self, model: &str, mut request: Value) -> Result<Value, HostError> {
        let snap = self.caller.snapshot.clone();
        let ws = &self.caller.workspace;
        let m = snap
            .model_by_name(model)
            .filter(|m| m.doc.kind == ModelKind::Decision && m.doc.state.callable())
            .cloned()
            .ok_or_else(|| HostError::Unavailable(format!("no decision model \"{model}\" is in service")))?;
        if snap.grant_terms(ws, &m.id).is_none() {
            return Err(HostError::Unavailable(format!("the model \"{model}\" is not granted here")));
        }
        let provider = snap
            .providers
            .get(&m.doc.provider_id)
            .filter(|p| p.doc.status == Status::Active && p.doc.family == Family::Typesafe)
            .cloned()
            .ok_or_else(|| {
                HostError::Unavailable(format!("the provider of \"{model}\" is not an active decision provider"))
            })?;
        let (secret, _) = provider.credential(&ws.id, &ws.organisation_id).ok_or_else(|| {
            HostError::Unavailable(format!("no credential for the provider \"{}\"", provider.doc.name))
        })?;
        if let Some(top) = request.as_object_mut() {
            top.insert("model".into(), json!(m.doc.upstream_id));
        }
        let body = Bytes::from(request.to_string());

        let caller = self.internal_caller();
        let mut rec = CallRecorder::new(self.gw.clone(), TraceContext::from_traceparent(Some(&self.traceparent)));
        rec.event.organisation_id = ws.organisation_id.clone();
        rec.event.workspace_id = ws.id.clone();
        rec.event.key_id = caller.key.id.clone();
        rec.event.task = format!("plugin.{}", self.plugin);
        rec.event.parent_id = self.parent.clone();
        rec.set_mode("managed");
        rec.event.interface = "decision".into();
        rec.event.path = "/v1/systemone".into();
        rec.event.model_requested = model.to_owned();
        rec.event.model_served = m.doc.name.clone();
        rec.event.provider = provider.doc.name.clone();
        rec.served = Some((provider.id.clone(), m.doc.upstream_id.clone()));
        rec.pricing = crate::dataplane::common::pricing_for(&snap, &m);
        rec.usage_policy = UsagePolicy::Ignore;
        rec.request_bytes = body.len() as u64;
        if let Err((e, _)) = crate::dataplane::common::admit_limits(&self.gw, &caller, &mut rec) {
            rec.outcome(status::REJECTED, e.status.as_u16(), e.kind, &e.message);
            rec.finish();
            return Err(HostError::Limited(Refused {
                status: e.status.as_u16(),
                kind: e.kind.to_owned(),
                message: e.message,
                retry_after_seconds: e.retry_after_seconds,
            }));
        }
        let auth = crate::dataplane::common::auth_header(
            &self.gw.google,
            &self.gw.upstream,
            provider.doc.managed_auth_scheme(),
            Some(&secret),
        )
        .await
        .map_err(HostError::Unavailable)?;
        let url = provider.doc.managed_url("systemone");

        // One more attempt when TypeSafe is busy (429, 529) or the connection failed before
        // anything was sent; the binding's time bounds both.
        for attempt in 1..=2 {
            let mut req = http::Request::builder()
                .method("POST")
                .uri(&url)
                .header("content-type", "application/json")
                .body(Body::from(body.clone()))
                .map_err(|e| HostError::Invalid(e.to_string()))?;
            if let Some((name, value)) = &auth {
                req.headers_mut().insert(name.clone(), value.clone());
            }
            rec.timeline.attempt();
            rec.event.attempts = rec.event.attempts.saturating_add(1);
            let sent = match self.gw.upstream.send(req, DECISION_TIMEOUT).await {
                Ok(sent) => sent,
                Err(SendError::Connect(_)) if attempt == 1 => continue,
                Err(e) => {
                    let message = e.to_string();
                    let timeout = matches!(e, SendError::Timeout(_));
                    let (outcome, kind) =
                        if timeout { (status::TIMEOUT, "upstream_timeout") } else { (status::ERROR, "upstream_error") };
                    rec.outcome(outcome, 0, kind, &message);
                    rec.finish();
                    return Err(HostError::Upstream(message));
                }
            };
            rec.timeline.upstream_head(sent.head_at);
            let code = sent.response.status().as_u16();
            if matches!(code, 429 | 529) && attempt == 1 {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            let bytes = match http_body_util::Limited::new(sent.response.into_body(), MAX_ANSWER).collect().await {
                Ok(b) => b.to_bytes(),
                Err(e) => {
                    rec.outcome(status::ERROR, code, "upstream_error", &format!("the answer could not be read: {e}"));
                    rec.finish();
                    return Err(HostError::Upstream("the answer could not be read".into()));
                }
            };
            rec.response_bytes = bytes.len() as u64;
            let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if (200..300).contains(&code) {
                let n = |k: &str| answer.pointer(&format!("/usage/{k}")).and_then(Value::as_u64);
                if let Some(input) = n("input_tokens") {
                    rec.event.usage = Usage {
                        input_tokens: input,
                        output_tokens: n("output_tokens").unwrap_or(0),
                        ..Default::default()
                    };
                    rec.event.usage_source = usage_source::PROVIDER.to_owned();
                }
                if let Some(answered) = answer.get("model").and_then(Value::as_str) {
                    rec.event.model_reported = answered.to_owned();
                }
                rec.outcome(status::OK, code, "", "");
                rec.finish();
                return Ok(answer);
            }
            let said =
                answer.get("message").and_then(Value::as_str).unwrap_or("the provider refused the call").to_owned();
            rec.outcome(status::ERROR, code, "upstream_error", &said);
            rec.finish();
            return Err(match code {
                401 | 403 => HostError::Unavailable(format!("the provider refused the credential: {said}")),
                400 | 413 | 415 | 422 => HostError::Invalid(format!("{code}: {said}")),
                _ => HostError::Upstream(format!("{code}: {said}")),
            });
        }
        rec.outcome(status::ERROR, 0, "upstream_error", "the provider stayed busy");
        rec.finish();
        Err(HostError::Upstream("the provider stayed busy".into()))
    }
}
