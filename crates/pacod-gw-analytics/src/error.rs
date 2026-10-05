//! The one error shape of the service: `{"error": {"code", "message"}}`.

use std::fmt::Display;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::storage::ClickHouseError;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    /// The number of the offending line of an ingest batch, counted from one.
    pub line: Option<usize>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into(), line: None }
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn invalid_parameter(name: &str, problem: impl Display) -> Self {
        Self::bad_request("invalid_parameter", format!("{name}: {problem}"))
    }

    pub fn not_found(what: impl Display) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", format!("{what} not found"))
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "missing or wrong x-gw-internal-token")
    }

    pub fn not_ready() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "not_ready", "the schema migrations have not been applied yet")
    }

    pub fn storage_unavailable() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable", "ClickHouse did not accept the request")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(line) = self.line {
            error["line"] = json!(line);
        }
        (self.status, Json(json!({ "error": error }))).into_response()
    }
}

/// A failed report query. ClickHouse's message is logged and not passed on.
impl From<ClickHouseError> for ApiError {
    fn from(error: ClickHouseError) -> Self {
        tracing::error!(%error, "ClickHouse query failed");
        match error {
            ClickHouseError::Transport(_) => Self::storage_unavailable(),
            ClickHouseError::Server { code, .. } => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                format!("ClickHouse rejected the query (code {code})"),
            ),
            ClickHouseError::Decode(_) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                "ClickHouse returned an unexpected result",
            ),
        }
    }
}
