//! gw-broker: the gateway on pacod-broker (pacod-broker's docs/pacod-gw.md).
//!
//! - **Offerings (R1).** The templates an organisation offers, named in the bridge's file, are
//!   announced under that organisation's own broker token, with prices from the gateway's
//!   repository and live state from its breakers; changes in the gateway update them.
//! - **Evidence (R3).** Calls to upstreams that are broker resources are reported as usage.
//!
//! It reads the gateway's management API and changes nothing in the gateway.

pub mod bridge;
pub mod config;
pub mod evidence;
pub mod front_door;
pub mod gw;
pub mod metrics;
pub mod offer;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;

pub use config::Config;

/// Serves `/readyz` and `/metrics`, and runs the bridge until SIGTERM or Ctrl-C.
pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let metrics = Arc::new(metrics::Metrics::default());
    let bridge = bridge::Bridge::new(cfg.clone(), metrics.clone())?;
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .route("/metrics", get(|State(m): State<Arc<metrics::Metrics>>| async move { m.render() }))
        .with_state(metrics);
    let listener = tokio::net::TcpListener::bind(cfg.listen).await?;
    tracing::info!(listen = %cfg.listen, gateway = %cfg.gw_url, broker = %cfg.broker_url, organisations = cfg.file.organisations.len(), "gw-broker is up");
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let work = tokio::spawn(bridge.run(stopped));
    shutdown_signal().await;
    tracing::info!("shutting down: ending the leases");
    let _ = stop.send(true);
    let _ = work.await;
    server.abort();
    Ok(())
}

async fn readyz(State(m): State<Arc<metrics::Metrics>>) -> impl IntoResponse {
    if m.ready.load(Ordering::Relaxed) {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "waiting for the first refresh")
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
