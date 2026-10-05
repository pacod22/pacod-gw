//! pacod-gw-analytics: stores the usage events and audit records the gateway core ships, and
//! answers the report queries the core forwards (concept sections 10.3 and 14).
//!
//! The core is the only caller. It authenticates with a shared secret and has already replaced
//! any organisation or workspace filter with the caller's scope, so this service applies the
//! filters it receives and does no authorisation of its own.
//!
//! * [`ingest`]: `POST /ingest/usage` and `POST /ingest/audit`.
//! * [`reports`]: everything under `/reports`, and `DELETE /audit`.
//! * [`storage`]: ClickHouse over HTTP, and the schema migrations.
//! * [`jobs`]: batch jobs run under a lease from the core: the price tracker.
//!
//! The HTTP API is documented in `API.md` next to this crate's `Cargo.toml`.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tokio::net::TcpListener;

mod auth;
pub mod config;
pub mod error;
pub mod ingest;
pub mod jobs;
pub mod reports;
pub mod storage;

pub use config::Config;

use error::ApiError;
use storage::{ClickHouse, ClickHouseError, migrations};

/// ClickHouse error codes that mean "the schema is not there yet".
const UNKNOWN_TABLE: i32 = 60;
const UNKNOWN_DATABASE: i32 = 81;

/// What the handlers share. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    clickhouse: ClickHouse,
    internal_token: String,
    max_batch_bytes: usize,
    migrated: AtomicBool,
}

impl AppState {
    /// Validates the configuration and prepares the ClickHouse client. Does not connect.
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        config.validate()?;
        let clickhouse = ClickHouse::new(
            &config.clickhouse_url,
            &config.clickhouse_database,
            config.clickhouse_user.as_deref(),
            config.clickhouse_password.as_deref(),
        )?;
        Ok(Self {
            inner: Arc::new(Inner {
                clickhouse,
                internal_token: config.internal_token.clone(),
                max_batch_bytes: config.max_batch_bytes,
                migrated: AtomicBool::new(false),
            }),
        })
    }

    pub fn clickhouse(&self) -> &ClickHouse {
        &self.inner.clickhouse
    }

    fn internal_token(&self) -> &str {
        &self.inner.internal_token
    }

    /// Creates the database when missing and applies the pending migrations. Until this has
    /// succeeded once, every route behind the token answers `503 not_ready`.
    pub async fn migrate(&self) -> Result<usize, ClickHouseError> {
        let applied = migrations::apply(&self.inner.clickhouse).await?;
        self.inner.migrated.store(true, Ordering::Release);
        Ok(applied)
    }

    pub fn is_migrated(&self) -> bool {
        self.inner.migrated.load(Ordering::Acquire)
    }
}

/// The complete application: `/healthz` and `/readyz` in the open, everything else behind the
/// internal token.
pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .merge(ingest::routes(state.inner.max_batch_bytes))
        .merge(reports::routes())
        .fallback(unknown_route)
        .layer(middleware::from_fn_with_state(state.clone(), require_migrated))
        // Added last, so it runs first: nothing is revealed to a caller without the token.
        .layer(middleware::from_fn_with_state(state.clone(), auth::require_token));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .merge(protected)
        .layer(middleware::from_fn(log_request))
        .with_state(state)
}

/// Serves on `listener` until `shutdown` resolves, then finishes the requests in flight.
/// The caller decides when [`AppState::migrate`] runs.
pub async fn serve(
    listener: TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    axum::serve(listener, router(state)).with_graceful_shutdown(shutdown).await?;
    Ok(())
}

/// Runs the service until SIGTERM or Ctrl-C.
///
/// The listener opens at once, so `/healthz` answers while ClickHouse is still starting. The
/// migrations are retried in the background until they succeed; `/readyz` turns 200 then.
pub async fn run(config: Config) -> anyhow::Result<()> {
    let state = AppState::new(&config)?;
    let listener = TcpListener::bind(config.listen).await?;
    tracing::info!(
        addr = %listener.local_addr()?,
        clickhouse = %config.clickhouse_url_for_log(),
        database = %config.clickhouse_database,
        "pacod-gw-analytics listening"
    );

    let migrator = tokio::spawn(migrate_until_done(state.clone()));
    // The batch jobs need the core; without its address this service only ingests and reports.
    let jobs = match &config.core_admin_url {
        Some(core) => {
            let egress = pacod_gw_events::egress::EgressPolicy::parse(&config.egress_allow_private)
                .map_err(|why| anyhow::anyhow!("PGW_EGRESS_ALLOW_PRIVATE: {why}"))?;
            Some(tokio::spawn(jobs::Runner::new(core, &config.internal_token, egress)?.run_forever()))
        }
        None => {
            tracing::info!("PGW_CORE_ADMIN_URL is not set: batch jobs (the price tracker) do not run here");
            None
        }
    };
    let served = serve(listener, state, shutdown_signal()).await;
    migrator.abort();
    if let Some(j) = jobs {
        j.abort();
    }
    tracing::info!("pacod-gw-analytics stopped");
    served
}

async fn migrate_until_done(state: AppState) {
    let mut wait = Duration::from_secs(1);
    loop {
        match state.migrate().await {
            Ok(applied) => {
                tracing::info!(applied, schema_version = migrations::latest_version(), "schema is up to date");
                return;
            }
            Err(error) => {
                tracing::warn!(%error, retry_in_s = wait.as_secs(), "migrations failed");
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(15));
            }
        }
    }
}

async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("shutdown requested, finishing requests in flight");
}

async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

/// 200 only when ClickHouse answers and the schema is at the version this build expects.
async fn readyz(State(state): State<AppState>) -> Response {
    let expected = migrations::latest_version();
    let reason = match migrations::applied_version(state.clickhouse()).await {
        Ok(version) if version >= expected => {
            return Json(json!({ "status": "ready", "schema_version": version })).into_response();
        }
        Ok(_) => "migrations_pending",
        Err(ClickHouseError::Transport(_)) => "clickhouse_unreachable",
        // ClickHouse answered, but the database or the ledger is not there yet.
        Err(ClickHouseError::Server { code: UNKNOWN_TABLE | UNKNOWN_DATABASE, .. }) => "migrations_pending",
        // Anything else, such as a rejected login. The migration loop logs the details.
        Err(_) => "clickhouse_error",
    };
    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "unavailable", "reason": reason }))).into_response()
}

async fn require_migrated(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if !state.is_migrated() {
        return ApiError::not_ready().into_response();
    }
    next.run(request).await
}

async fn unknown_route() -> ApiError {
    ApiError::not_found("route")
}

/// One log line per request: method, route, status and duration. The route is the pattern that
/// matched, not the path, so identifiers in paths stay out of the log. Query strings and bodies
/// are never logged.
async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "<unmatched>".to_owned(), |path| path.as_str().to_owned());
    let started = Instant::now();

    let response = next.run(request).await;

    let status = response.status().as_u16();
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    if route == "/healthz" || route == "/readyz" {
        tracing::debug!(%method, route, status, elapsed_ms, "request");
    } else {
        tracing::info!(%method, route, status, elapsed_ms, "request");
    }
    response
}
