//! The data plane: the listener applications call (concept section 4).

pub mod anthropic;
pub mod auth;
pub mod client_ip;
pub mod common;
pub mod errors;
pub mod files;
pub mod gemini;
pub mod managed;
pub mod native;
pub mod objects;
pub mod openai;
pub mod proxy;
pub mod responses;
pub mod shape;
pub mod tools;
pub mod translate;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

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

/// Everything a request handler needs. Requests are served from memory, with two exceptions:
/// the endpoints for Anthropic's files and batches read and write their records
/// (`objects.rs`), and a body that names a file id has its owner looked up (`files.rs`).
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
    /// How many running instances serve the data plane, this one included. A key's
    /// concurrency cap is shared out over them.
    pub data_plane_instances: AtomicUsize,
    /// Set while the instance stops: `/readyz` fails, requests are still served.
    pub draining: AtomicBool,
    /// Whether both spool directories took the last probe write (`spool_probe`).
    pub spool_writable: AtomicBool,
    /// Access tokens minted from service-account keys (Vertex AI).
    pub google: Arc<crate::upstream::google::GoogleTokens>,
    /// Told about every settled call; set by the gateway without a database, which logs them.
    pub call_sink: Option<Arc<dyn crate::relay::CallSink>>,
    /// Where the response cache keeps answers: Valkey, or this instance's memory.
    pub answers: Arc<dyn crate::answers::AnswerStore>,
}

impl Gw {
    /// A data plane on its own, for a gateway without a database or analytics service
    /// (`pacod-gw-lite`): no spools, no traces, limits counted on this instance only.
    pub fn standalone(
        boot: Arc<Bootstrap>,
        config: Arc<ConfigState>,
        upstream: Upstream,
        call_sink: Option<Arc<dyn crate::relay::CallSink>>,
    ) -> Self {
        Self {
            google: Arc::new(crate::upstream::google::GoogleTokens::new(boot.google_token_url.clone())),
            answers: Arc::new(crate::answers::MemoryStore::new(boot.cache.memory_bytes)),
            boot,
            config,
            upstream,
            usage_spool: Spool::disabled(),
            audit_spool: Spool::disabled(),
            live: Arc::new(LiveStats::default()),
            limits: Arc::new(LimitEngine::local()),
            tracer: Tracer::disabled(),
            auth_failures: Default::default(),
            key_slots: Default::default(),
            data_plane_instances: AtomicUsize::new(1),
            draining: Default::default(),
            spool_writable: AtomicBool::new(true),
            call_sink,
        }
    }

    /// Writes a probe file into both spool directories and remembers the outcome for
    /// `/readyz`. Run every few seconds, off the request path.
    pub fn spool_probe(&self) -> bool {
        let result = self.usage_spool.probe().and_then(|()| self.audit_spool.probe());
        let ok = result.is_ok();
        let was = self.spool_writable.swap(ok, Ordering::Relaxed);
        match (&result, was) {
            (Err(e), true) => tracing::error!(error = %e, "the event spool cannot be written; /readyz fails"),
            (Ok(()), false) => tracing::info!("the event spool can be written again"),
            _ => {}
        }
        ok
    }

    /// Whether a request must be refused because its usage could not be recorded: the event
    /// spool is full and the policy (the caller's resolved setting, else the installation's)
    /// is to fail closed.
    pub fn refuses_unaccounted(&self, policy: Option<crate::bootstrap::SpoolFullPolicy>) -> bool {
        self.usage_spool.is_full()
            && policy.unwrap_or(self.boot.spool_full_policy) == crate::bootstrap::SpoolFullPolicy::FailClosed
    }
}

pub fn router(gw: Arc<Gw>) -> Router {
    // Bodies are limited by the handlers themselves, which know the configured size.
    routes().merge(object_routes()).layer(axum::extract::DefaultBodyLimit::disable()).with_state(gw)
}

/// The data plane without Anthropic's files and batches, for a gateway without a database
/// (`pacod-gw-lite`): their paths answer 501, and proxy mode reaches the provider's own.
pub fn router_without_objects(gw: Arc<Gw>) -> Router {
    let not_here = || {
        any(|| async {
            errors::GwError::not_implemented(
                "not_available",
                "this gateway keeps no record of files and batches: call the provider's API through /proxy/<provider>/ instead",
            )
            .response(errors::Envelope::Anthropic, "")
        })
    };
    routes()
        .route("/v1/messages/batches", not_here())
        .route("/v1/messages/batches/{*rest}", not_here())
        .route("/v1/files", not_here())
        .route("/v1/files/{*rest}", not_here())
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(gw)
}

/// Anthropic's files and batches, which keep a record of each object in the database.
fn object_routes() -> Router<Arc<Gw>> {
    Router::new()
        .route("/v1/messages/batches", post(objects::batches_create).get(objects::batches_list))
        .route("/v1/messages/batches/{id}", get(objects::batch_get).delete(objects::batch_delete))
        .route("/v1/messages/batches/{id}/cancel", post(objects::batch_cancel))
        .route("/v1/messages/batches/{id}/results", get(objects::batch_results))
        .route("/v1/files", post(objects::files_create).get(objects::files_list))
        .route("/v1/files/{id}", get(objects::file_get).delete(objects::file_delete))
        .route("/v1/files/{id}/content", get(objects::file_content))
}

/// Everything else, served from memory.
fn routes() -> Router<Arc<Gw>> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/proxy/{provider}", any(proxy::handle))
        .route("/proxy/{provider}/{*rest}", any(proxy::handle))
        .route("/v1/chat/completions", post(managed::chat_completions))
        .route("/v1/messages", post(managed::messages))
        .route("/v1/messages/count_tokens", post(managed::count_tokens))
        .route("/v1/audio/speech", post(managed::speech))
        .route("/v1/embeddings", post(managed::embeddings))
        .route("/v1/systemone", post(managed::systemone))
        .route("/v1/responses", post(managed::responses))
        .route("/v1/responses/{*rest}", any(responses::not_stored))
        .route("/gw/v1/chat", post(managed::native_chat))
        .route("/gw/v1/speech", post(managed::native_speech))
        .route("/v1/models", get(openai::list_models))
        .route("/v1/models/{*id}", get(openai::get_model).post(gemini::v1_model))
        // Gemini's API, as its SDKs call it in both their modes.
        .route("/v1beta/models", get(gemini::list_models))
        .route("/v1beta/models/{*rest}", get(gemini::model).post(gemini::model))
        .route("/v1/projects/{*rest}", post(gemini::vertex_project))
        .route("/v1beta1/projects/{*rest}", post(gemini::vertex_project))
        .route("/v1/publishers/{*rest}", post(gemini::vertex_publisher))
        .route("/v1beta1/publishers/{*rest}", post(gemini::vertex_publisher))
        .route("/v1beta/{*rest}", any(gemini::elsewhere))
        .route("/upload/{*rest}", any(gemini::elsewhere))
        .route("/gw/v1/models", get(native::list_models))
        .route("/gw/v1/models/{*reference}", get(native::get_model))
        .route("/gw/v1/quota", get(native::quota))
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Ready when a configuration snapshot is loaded and the spool accepts events.
async fn readyz(State(gw): State<Arc<Gw>>) -> (StatusCode, Json<Value>) {
    if gw.draining.load(Ordering::Relaxed) {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "stopping" })));
    }
    if gw.refuses_unaccounted(gw.config.snapshot().settings.on_spool_full) {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "event spool full" })));
    }
    // A disk that takes no writes is this instance's fault, whatever the spool-full policy:
    // other instances can serve while it is fixed.
    if !gw.spool_writable.load(Ordering::Relaxed) {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "event spool cannot be written" })));
    }
    (StatusCode::OK, Json(json!({ "status": "ready", "config_version": gw.config.version() })))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::crypto::keys::Pepper;
    use crate::upstream::EgressPolicy;

    fn standalone() -> Arc<Gw> {
        let pepper = Pepper::new(b"0123456789abcdef0123".to_vec()).unwrap();
        let boot = Arc::new(Bootstrap::standalone("127.0.0.1:0".parse().unwrap(), pepper));
        let config = ConfigState::from_rows(Vec::new(), Vec::new());
        let upstream = Upstream::new(Arc::new(EgressPolicy::parse(&[]).unwrap())).unwrap();
        Arc::new(Gw::standalone(boot, config, upstream, None))
    }

    async fn call(router: &Router, method: &str, path: &str) -> (StatusCode, Value) {
        let req = Request::builder().method(method).uri(path).body(Body::empty()).unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = http_body_util::BodyExt::collect(res.into_body()).await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn without_a_database_files_and_batches_answer_501_and_the_rest_is_served() {
        crate::app::install_crypto_provider();
        let gw = standalone();
        assert!(gw.config.store().is_none());
        let router = router_without_objects(gw);
        for (method, path) in [
            ("GET", "/v1/files"),
            ("POST", "/v1/files"),
            ("GET", "/v1/files/file_1/content"),
            ("POST", "/v1/messages/batches"),
            ("POST", "/v1/messages/batches/b_1/cancel"),
        ] {
            let (status, body) = call(&router, method, path).await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{method} {path}");
            assert_eq!(body["type"], "error", "Anthropic's error format");
            assert!(body["error"]["message"].as_str().unwrap().contains("/proxy/"));
        }
        assert_eq!(call(&router, "GET", "/healthz").await.0, StatusCode::OK);
        let (status, body) = call(&router, "GET", "/readyz").await;
        assert_eq!((status, body["config_version"].as_i64()), (StatusCode::OK, Some(1)));
        // Without a key the data plane refuses, as with a database.
        assert_eq!(call(&router, "GET", "/v1/models").await.0, StatusCode::UNAUTHORIZED);
    }
}
