//! The three listeners: public (OIDC, the hosted UI, /ui-api), internal (product
//! backends: /internal/v1, /admin/v1, and the OIDC endpoints a server calls), and metrics.

pub mod caller;
mod discovery;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use metrics_exporter_prometheus::PrometheusHandle;

use crate::bootstrap::Bootstrap;
use crate::config::Config;
use crate::error::ApiError;
use crate::keys::KeyRing;
use crate::store::Store;

/// What every handler shares.
pub struct AppState {
    pub boot: Arc<Bootstrap>,
    pub config: Arc<Config>,
    pub store: Store,
    pub keys: Arc<KeyRing>,
    pub metrics: PrometheusHandle,
    /// False until start-up is done, and again while shutting down.
    pub ready: AtomicBool,
    pub mail: Option<crate::mail::Transport>,
    /// Bounds the password hashes computed at once: each takes 19 MiB or more.
    pub hashing: tokio::sync::Semaphore,
    /// Checked for unknown accounts, so they take as long as known ones.
    pub dummy_hash: String,
    /// The products' manifests and pacod-id's own.
    pub catalog: crate::authz::CatalogCell,
    /// The feed's newest version, for the long polls waiting on it.
    pub feed: crate::authz::feed::Feed,
    /// True once the service stops: long polls answer at once.
    pub stopping: tokio::sync::watch::Receiver<bool>,
    /// The upstream providers' discovery documents and keys (Google).
    pub upstreams: crate::federation::Upstreams,
    /// The hosted pages (PID_UI_DIR).
    pub ui: crate::ui::Ui,
}

pub type Shared = Arc<AppState>;

/// Requests and their durations by listener and status class, and no-store on every answer:
/// nothing pacod-id says is to be cached by a proxy (the JWKS sets its own header).
async fn observe(listener: &'static str, req: Request, next: Next) -> Response {
    let started = Instant::now();
    // The OAuth endpoints answer in their own shapes (RFC 6749) or with pages; the APIs in ours.
    let oauth = req.uri().path().starts_with("/oidc/");
    let response = next.run(req).await;
    let mut response = if oauth { response } else { rejections_as_errors(response).await };
    let class = format!("{}xx", response.status().as_u16() / 100);
    metrics::counter!("pid_http_requests_total", "listener" => listener, "status" => class.clone()).increment(1);
    metrics::histogram!("pid_http_request_seconds", "listener" => listener).record(started.elapsed().as_secs_f64());
    let headers = response.headers_mut();
    headers.entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static("no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

/// axum's extractors refuse a malformed body or query in plain text; the API answers every
/// error in its JSON shape, so callers parse one shape (`details.code` = `bad_request`). Only
/// those answers are plain text with these statuses: handlers answer JSON or HTML pages.
async fn rejections_as_errors(response: Response) -> Response {
    let status = response.status();
    let plain = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/plain"));
    if !plain || !matches!(status.as_u16(), 400 | 413 | 415 | 422) {
        return response;
    }
    let text = axum::body::to_bytes(response.into_body(), 4096).await.unwrap_or_default();
    let message = String::from_utf8_lossy(&text).into_owned();
    let mut e = ApiError::new(status, "bad_request", message);
    e.details = Some(serde_json::json!({ "code": "bad_request" }));
    e.into_response()
}

async fn not_found() -> ApiError {
    ApiError::not_found("no such endpoint")
}

pub fn public(state: Shared) -> Router {
    Router::new()
        .route("/.well-known/openid-configuration", get(discovery::configuration))
        .route("/oidc/jwks", get(discovery::jwks))
        .route("/oidc/authorize", get(crate::oidc::authorize::get).post(crate::oidc::authorize::post))
        .route("/oidc/token", post(crate::oidc::token::token))
        .merge(crate::oidc::flows::routes())
        .merge(crate::oidc::account::routes())
        .merge(crate::oidc::logout::public_routes())
        .merge(crate::oidc::userinfo::public_routes())
        .merge(crate::admin::public_routes())
        .merge(crate::federation::routes())
        .merge(crate::me::routes())
        .fallback(crate::ui::serve)
        .layer(middleware::from_fn(|req, next| observe("public", req, next)))
        .with_state(state)
}

pub fn internal(state: Shared) -> Router {
    Router::new()
        .route("/oidc/jwks", get(discovery::jwks))
        .route("/oidc/token", post(crate::oidc::token::token))
        .merge(crate::oidc::logout::internal_routes())
        .merge(crate::oidc::userinfo::internal_routes())
        .merge(crate::authz::internal::routes())
        .merge(crate::admin::internal_routes())
        .fallback(not_found)
        .layer(middleware::from_fn(|req, next| observe("internal", req, next)))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(st): State<Shared>) -> Response {
    if st.ready.load(Ordering::Relaxed) && st.store.ping().await && st.keys.signer(pacod_id_jose::Alg::RS256).is_some()
    {
        "ready".into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
    }
}

async fn metrics_page(State(st): State<Shared>) -> Response {
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], st.metrics.render()).into_response()
}

pub fn metrics(state: Shared) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_page))
        .with_state(state)
}
