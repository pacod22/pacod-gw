//! Reports API consumed by the UI: usage, cost and statistics queries plus scheduled exports.

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

pub fn router() -> Router {
    Router::new().route("/summary", get(summary))
}

async fn summary() -> Json<Value> {
    Json(json!({}))
}
