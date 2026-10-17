//! The playground (concept section 15.2): try a request in a workspace, or only look at what
//! the gateway would send to the provider for it.
//!
//! Both endpoints run the same code as the data plane. A test call is a real call: it is
//! admitted against the workspace's limits, recorded as a usage event and paid for.

use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::Response;
use axum::routing::post;
use axum::{Json, Router};
use bytes::Bytes;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::AdminState;
use super::auth::Principal;
use super::crud::{Params, ScopeKind, collection_path, resolve_scope};
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use crate::dataplane::auth::Caller;
use crate::dataplane::errors::GwError;
use crate::dataplane::managed::{self, Interface};
use crate::model::{ApiKey, InspectMode, KeyStatus, UsagePolicy, all_modes};
use crate::snapshot::KeyRt;
use crate::telemetry::trace::TraceContext;

/// The interface a playground request is written for.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlaygroundInterface {
    /// `/v1/chat/completions`
    Openai,
    /// `/v1/messages`
    Anthropic,
    /// `/gw/v1/chat`
    Native,
}

impl From<PlaygroundInterface> for Interface {
    fn from(i: PlaygroundInterface) -> Self {
        match i {
            PlaygroundInterface::Openai => Interface::OpenAi,
            PlaygroundInterface::Anthropic => Interface::Anthropic,
            PlaygroundInterface::Native => Interface::Native,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaygroundInput {
    pub interface: PlaygroundInterface,
    /// The request body, as an application would send it to that interface.
    pub request: Value,
    /// Gateway headers to send with it, for example `x-gw-template` or `x-gw-end-user`.
    /// Only `x-gw-*` and `anthropic-*` headers are taken.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// What the gateway would send to the provider for a request.
#[derive(Debug, Serialize, JsonSchema)]
pub struct TransformPreview {
    /// `same_dialect`: the caller's bytes with governed fields rewritten. `translated`: through
    /// the neutral model.
    pub route: String,
    pub provider: String,
    pub model_requested: String,
    pub model_served: String,
    pub template: Option<String>,
    pub template_version: Option<u32>,
    /// The URL the request would go to.
    pub url: String,
    /// The body the provider would receive.
    pub body: Value,
    pub stream: bool,
    /// Parameters and features left out because the model does not take them.
    pub dropped_params: Vec<String>,
    /// For a template: the scheme and rule that chose the model, and the models in order.
    pub routing: serde_json::Value,
    pub warnings: Vec<String>,
}

/// A request the gateway would refuse. For the preview that is a finding about the request,
/// not a missing resource and not a failed service, whatever status an application would
/// get: it is reported as 422 with the error type and message of the data plane, and that
/// status in `details.gateway_status`.
fn api_error(e: GwError) -> ApiError {
    let mut details = e.details.map(|d| *d).filter(Value::is_object).unwrap_or_else(|| serde_json::json!({}));
    details["gateway_status"] = serde_json::json!(e.status.as_u16());
    ApiError::new(axum::http::StatusCode::UNPROCESSABLE_ENTITY, e.kind, e.message).with_details(details)
}

/// The caller a playground request runs as: the workspace, with no key restriction. The key
/// id names the administrator, so that the calls can be told apart in the request log.
fn caller(st: &AdminState, p: &Principal, params: &std::collections::HashMap<String, String>) -> ApiResult<Caller> {
    let snapshot = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, params, &snapshot)?;
    // A test call spends the workspace's money and allowance: that is an administrator's to do.
    if !p.can_admin_workspace(scope.ws(), scope.org()) {
        return Err(ApiError::forbidden());
    }
    let workspace = snapshot
        .workspaces
        .get(scope.ws())
        .filter(|w| w.enabled)
        .cloned()
        .ok_or_else(|| ApiError::conflict("the workspace or its organisation is disabled"))?;
    Ok(caller_for(snapshot, workspace, &format!("playground:{}", p.id)))
}

/// A caller that acts as a workspace without one of its API keys. `key_id` says who it is,
/// in the request log and for key limits.
pub fn caller_for(
    snapshot: Arc<crate::snapshot::Snapshot>,
    workspace: Arc<crate::snapshot::WorkspaceRt>,
    key_id: &str,
) -> Caller {
    let key = KeyRt {
        id: key_id.to_owned(),
        workspace_id: workspace.id.clone(),
        organisation_id: workspace.organisation_id.clone(),
        doc: ApiKey {
            name: "playground".into(),
            secret_hmac: String::new(),
            prev_hmac: None,
            prev_valid_until: None,
            hint: String::new(),
            allowed_modes: all_modes(),
            allowed_models: Vec::new(),
            allowed_templates: Vec::new(),
            require_template: false,
            usage_policy: Some(UsagePolicy::Estimate),
            rate_class: None,
            inspect: InspectMode::Full,
            max_concurrency: None,
            expires_at: None,
            status: KeyStatus::Active,
        },
        valid_until: None,
        class_limits: Vec::new(),
        class_max_concurrency: None,
    };
    Caller { snapshot, key: Arc::new(key), workspace, internal: None }
}

fn gateway_headers(input: &BTreeMap<String, String>) -> ApiResult<HeaderMap> {
    let mut out = HeaderMap::new();
    for (name, value) in input {
        let lower = name.to_ascii_lowercase();
        if !(lower.starts_with("x-gw-") || lower.starts_with("anthropic-")) {
            return Err(ApiError::invalid(format!("the header {name} is not taken; only x-gw-* and anthropic-* are")));
        }
        let (Ok(n), Ok(v)) = (HeaderName::from_bytes(lower.as_bytes()), HeaderValue::from_str(value)) else {
            return Err(ApiError::invalid(format!("the header {name} is not a valid header")));
        };
        out.insert(n, v);
    }
    Ok(out)
}

fn body_of(request: &Value) -> ApiResult<Bytes> {
    if !request.is_object() {
        return Err(ApiError::invalid("request must be a JSON object: the body an application would send"));
    }
    serde_json::to_vec(request).map(Bytes::from).map_err(ApiError::internal)
}

/// Shows what the gateway would send to the provider. Nothing is sent and nothing is counted.
async fn transform(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<PlaygroundInput>,
) -> ApiResult<Json<TransformPreview>> {
    let caller = caller(&st, &p, &params)?;
    let headers = gateway_headers(&input.headers)?;
    let plan =
        // Plugins are not asked: a preview sends nothing, and their engines would.
        managed::plan(&caller, input.interface.into(), &headers, &body_of(&input.request)?, None, None)
            .map_err(api_error)?;
    let template = plan.target.template.as_ref();
    Ok(Json(TransformPreview {
        route: if plan.same_dialect { "same_dialect" } else { "translated" }.into(),
        provider: plan.target.provider.doc.name.clone(),
        model_requested: plan.model_requested.clone(),
        model_served: plan.target.model.doc.name.clone(),
        template: template.map(|t| t.name.clone()),
        template_version: template.map(|t| t.version),
        url: plan.url(),
        body: serde_json::from_slice(&plan.shaped.body).map_err(ApiError::internal)?,
        stream: plan.shaped.stream,
        dropped_params: plan.shaped.dropped.clone(),
        routing: managed::route_resolution(&plan.target, plan.route.as_ref()),
        warnings: plan.warnings,
    }))
}

/// Makes the call. The answer is the data plane's, stream or not, with its `x-gw-*` headers.
async fn chat(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<PlaygroundInput>,
) -> ApiResult<Response> {
    let started = Instant::now();
    let caller = caller(&st, &p, &params)?;
    let mut headers = gateway_headers(&input.headers)?;
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    let body = Body::from(body_of(&input.request)?);
    let trace = TraceContext::from_traceparent(None);
    Ok(managed::serve(st.gw.clone(), caller, started, trace, headers, body, input.interface.into(), None).await)
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let base = collection_path(ScopeKind::Workspace, "playground");
    let input = api.schema::<PlaygroundInput>();
    let preview = api.schema::<TransformPreview>();
    api.add(
        api.op(
            "post",
            &format!("{base}/transform"),
            "playgroundTransform",
            "playground",
            "Show what the gateway would send to the provider",
        )
        .body(input.clone())
        .ok(preview),
    );
    api.add(
        api.op("post", &format!("{base}/chat"), "playgroundChat", "playground", "Make a test call in the workspace")
            .body(input)
            .ok(serde_json::json!({ "description": "The answer of the chosen interface, as a JSON document or an event stream." })),
    );
    router.route(&format!("{base}/transform"), post(transform)).route(&format!("{base}/chat"), post(chat))
}
