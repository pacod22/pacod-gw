//! The data plane: the listener applications call (concept section 4).

pub mod auth;
pub mod common;
pub mod errors;
pub mod native;
pub mod openai;
pub mod proxy;
pub mod shape;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::bootstrap::Bootstrap;
use crate::limits::LimitEngine;
use crate::snapshot::ConfigState;
use crate::spool::Spool;
use crate::telemetry::live::LiveStats;
use crate::telemetry::trace::Tracer;
use crate::upstream::Upstream;

/// Everything a request handler needs. Nothing in here touches a database.
pub struct Gw {
    pub boot: Arc<Bootstrap>,
    pub config: Arc<ConfigState>,
    pub upstream: Upstream,
    pub usage_spool: Spool,
    pub audit_spool: Spool,
    pub live: Arc<LiveStats>,
    pub limits: Arc<LimitEngine>,
    pub tracer: Tracer,
    pub auth_failures: auth::FailureLimiter,
    /// Calls in progress per key, for keys with a concurrency cap.
    pub key_slots: parking_lot::Mutex<HashMap<String, Arc<AtomicU32>>>,
}

pub fn router(gw: Arc<Gw>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/proxy/{provider}", any(proxy::handle))
        .route("/proxy/{provider}/{*rest}", any(proxy::handle))
        .route("/v1/chat/completions", post(openai::chat_completions))
        .route("/v1/models", get(openai::list_models))
        .route("/v1/models/{*id}", get(openai::get_model))
        .route("/gw/v1/models", get(native::list_models))
        .route("/gw/v1/quota", get(native::quota))
        // Bodies are limited by the handlers themselves, which know the configured size.
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(gw)
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Ready when a configuration snapshot is loaded and the spool accepts events.
async fn readyz(State(gw): State<Arc<Gw>>) -> (StatusCode, Json<Value>) {
    if gw.usage_spool.refuses_requests() {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "event spool full" })));
    }
    (StatusCode::OK, Json(json!({ "status": "ready", "config_version": gw.config.version() })))
}
