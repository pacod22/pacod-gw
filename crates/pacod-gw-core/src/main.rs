mod config;
mod events;
mod middleware;
mod providers;
mod routing;
mod telemetry;

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init();
    let config = config::Config::load()?;

    let app = Router::new().route("/healthz", get(healthz));

    let listener = tokio::net::TcpListener::bind(&config.server.listen).await?;
    tracing::info!(addr = %config.server.listen, "pacod-gw-core listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
