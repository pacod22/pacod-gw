//! What the core does for a plugin: model calls on the workspace's behalf
//! (`docs/plugin-concept.md`, section 3.8).
//!
//! A plugin's calls are the gateway's own. They run as an internal caller of the classified
//! call's workspace, so they are accounted, limited and priced there, and plugins never look
//! at them. Their usage events name the classified call in `parent_id`.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Response};
use pacod_gw_plugin::{BoxFuture, Host, HostError, Refused};
use serde_json::{Value, json};

use crate::dataplane::Gw;
use crate::dataplane::auth::{Caller, Internal};
use crate::dataplane::managed::{self, Interface};
use crate::telemetry::trace::TraceContext;

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

    fn decide<'a>(&'a self, model: &'a str, _request: Value) -> BoxFuture<'a, Result<Value, HostError>> {
        Box::pin(async move { Err(HostError::Unavailable(format!("the model \"{model}\" is not a decision model"))) })
    }
}
