mod aggregate;
mod ingest;
mod reports;
mod storage;

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let listen = std::env::var("ANALYTICS_LISTEN").unwrap_or_else(|_| "0.0.0.0:8090".into());
    let app = Router::new()
        .route("/healthz", get(healthz))
        .nest("/api/reports", reports::router());

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!(addr = %listen, "pacod-gw-analytics listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
