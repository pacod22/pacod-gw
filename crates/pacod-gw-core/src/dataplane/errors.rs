//! Errors the gateway itself raises on the data plane (concept sections 9.2, 9.4 and 9.5).
//!
//! The error is the same whatever the interface; only its envelope differs, so that the SDK of
//! the dialect the caller speaks handles it as it would handle the provider's own errors.

use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use serde_json::{Value, json};

/// The envelope an error is rendered in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Envelope {
    /// `{"error": {"type", "message", "source", "retryable", "request_id", "details"}}`
    Gateway,
    /// `{"error": {"message", "type", "param", "code"}}`
    OpenAi,
    /// `{"type": "error", "error": {"type", "message"}}`
    Anthropic,
}

#[derive(Debug, Clone)]
pub struct GwError {
    pub status: StatusCode,
    /// The gateway's error type, for example `quota_exceeded`.
    pub kind: &'static str,
    pub message: String,
    /// `gateway` or `upstream`.
    pub source: &'static str,
    pub retryable: bool,
    pub retry_after_seconds: Option<u32>,
    /// Boxed: errors travel in `Result`s on the request path and should stay small.
    pub details: Option<Box<Value>>,
}

impl GwError {
    fn new(status: StatusCode, kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            kind,
            message: message.into(),
            source: "gateway",
            retryable: false,
            retry_after_seconds: None,
            details: None,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    pub fn unsupported_feature(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "unsupported_feature", message)
    }

    pub fn usage_required() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "usage_required",
            "this key requires usage reporting: set stream_options.include_usage to true on streamed requests",
        )
    }

    /// Proxy mode: the template's model is at another provider than the one in the URL.
    pub fn template_provider_mismatch(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "template_provider_mismatch", message)
    }

    /// Proxy mode: a `check` template's rules are not met by the request.
    pub fn template_violation(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "template_violation", message)
    }

    pub fn invalid_key() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid_key", "the API key is unknown, expired or revoked")
    }

    pub fn not_allowed(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "not_allowed", message)
    }

    /// Proxy mode: a call the gateway cannot check against the workspace's grants may not use
    /// the installation's shared credential.
    pub fn own_credential_required(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "own_credential_required", message)
    }

    /// Proxy mode: a model call whose body the gateway cannot read.
    pub fn unsupported_media_type(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type", message)
    }

    pub fn quota_exceeded(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYMENT_REQUIRED, "quota_exceeded", message)
    }

    pub fn not_found(kind: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, kind, message)
    }

    pub fn model_retired(message: impl Into<String>) -> Self {
        Self::new(StatusCode::GONE, "model_retired", message)
    }

    pub fn too_large(limit: usize) -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request",
            format!("the request body exceeds the limit of {limit} bytes"),
        )
    }

    pub fn rate_limited(message: impl Into<String>, retry_after_seconds: u32) -> Self {
        Self {
            retryable: true,
            retry_after_seconds: Some(retry_after_seconds),
            ..Self::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", message)
        }
    }

    pub fn overloaded(message: impl Into<String>) -> Self {
        Self {
            retryable: true,
            retry_after_seconds: Some(1),
            ..Self::new(StatusCode::SERVICE_UNAVAILABLE, "gateway_overloaded", message)
        }
    }

    pub fn unavailable(kind: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, kind, message)
    }

    /// The provider could not be reached or failed before answering.
    pub fn upstream_error(message: impl Into<String>) -> Self {
        Self { source: "upstream", retryable: true, ..Self::new(StatusCode::BAD_GATEWAY, "upstream_error", message) }
    }

    /// A limit refused the request: a throttle is `429`, a used-up allowance is `402`
    /// (concept section 9.2: a spent quota does not recover by retrying).
    pub fn from_refusal(r: &crate::limits::Refusal) -> Self {
        let subject = match r.subject_kind {
            crate::model::LimitSubject::Organisation => "the organisation",
            crate::model::LimitSubject::Workspace => "the workspace",
            crate::model::LimitSubject::Key => "this key",
            crate::model::LimitSubject::EndUser => "this end user",
        };
        let mut details = json!({ "limit": r.limit_name });
        if let Some(t) = r.resets_at {
            details["resets_at"] = json!(t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        }
        match r.retry_after_seconds {
            Some(s) => {
                Self::rate_limited(format!("the limit \"{}\" for {subject} is reached; retry later", r.limit_name), s)
            }
            None => Self::quota_exceeded(format!("the limit \"{}\" for {subject} is used up", r.limit_name)),
        }
        .with_details(details)
    }

    /// The provider's own rate limit, after the gateway's retries.
    pub fn upstream_rate_limited(message: impl Into<String>, retry_after_seconds: Option<u32>) -> Self {
        Self {
            source: "upstream",
            retryable: true,
            retry_after_seconds,
            ..Self::new(StatusCode::TOO_MANY_REQUESTS, "upstream_rate_limited", message)
        }
    }

    /// The provider refused the request itself (a 4xx other than its rate limit), and its own
    /// error body is in a dialect the caller does not speak. The status is the provider's.
    pub fn upstream_refused(status: StatusCode, message: impl Into<String>) -> Self {
        Self { source: "upstream", ..Self::new(status, "upstream_error", message) }
    }

    /// A timeout is never retried: the provider may have completed and billed the call.
    pub fn upstream_timeout(message: impl Into<String>) -> Self {
        Self { source: "upstream", ..Self::new(StatusCode::GATEWAY_TIMEOUT, "upstream_timeout", message) }
    }

    pub fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!(error = %err, "data plane internal error");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal gateway error")
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    /// The error type in Anthropic's vocabulary (concept section 9.4).
    fn anthropic_type(&self) -> &'static str {
        match self.status.as_u16() {
            400 | 413 | 415 => "invalid_request_error",
            401 => "authentication_error",
            402 => "billing_error",
            403 => "permission_error",
            404 | 410 => "not_found_error",
            429 => "rate_limit_error",
            _ => "api_error",
        }
    }

    /// The error type in OpenAI's vocabulary.
    fn openai_type(&self) -> &'static str {
        match self.status.as_u16() {
            400 | 404 | 410 | 413 | 415 => "invalid_request_error",
            401 => "authentication_error",
            402 => "insufficient_quota",
            403 => "permission_error",
            429 => "rate_limit_error",
            _ => "api_error",
        }
    }

    pub fn body(&self, envelope: Envelope, request_id: &str) -> Value {
        match envelope {
            Envelope::Gateway => {
                let mut e = json!({
                    "type": self.kind,
                    "message": self.message,
                    "source": self.source,
                    "retryable": self.retryable,
                    "request_id": request_id,
                });
                if let Some(d) = &self.details {
                    e["details"] = (**d).clone();
                }
                json!({ "error": e })
            }
            Envelope::OpenAi => json!({ "error": {
                "message": self.message,
                "type": self.openai_type(),
                "param": null,
                "code": self.kind,
            } }),
            Envelope::Anthropic => json!({
                "type": "error",
                "error": { "type": self.anthropic_type(), "message": self.message },
                "request_id": request_id,
            }),
        }
    }

    pub fn response(&self, envelope: Envelope, request_id: &str) -> Response {
        let body = serde_json::to_vec(&self.body(envelope, request_id)).unwrap_or_default();
        let mut r = Response::new(axum::body::Body::from(body));
        *r.status_mut() = self.status;
        let h = r.headers_mut();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert("x-gw-error-type", HeaderValue::from_static(self.kind));
        if let Ok(v) = HeaderValue::from_str(request_id) {
            h.insert("x-gw-request-id", v);
        }
        if !self.retryable {
            // Honoured by the OpenAI and Anthropic SDKs: do not retry this response.
            h.insert("x-should-retry", HeaderValue::from_static("false"));
        }
        if let Some(s) = self.retry_after_seconds {
            h.insert(RETRY_AFTER, HeaderValue::from(s));
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_envelope_carries_everything() {
        let e = GwError::quota_exceeded("Daily token limit reached.")
            .with_details(json!({ "limit": "end-user-daily-tokens" }));
        let b = e.body(Envelope::Gateway, "gw_1");
        assert_eq!(b["error"]["type"], "quota_exceeded");
        assert_eq!(b["error"]["source"], "gateway");
        assert_eq!(b["error"]["retryable"], false);
        assert_eq!(b["error"]["request_id"], "gw_1");
        assert_eq!(b["error"]["details"]["limit"], "end-user-daily-tokens");
        assert_eq!(e.status, StatusCode::PAYMENT_REQUIRED);
    }

    #[test]
    fn anthropic_envelope_uses_anthropics_type_names() {
        for (err, ty) in [
            (GwError::invalid_request("x"), "invalid_request_error"),
            (GwError::unsupported_feature("x"), "invalid_request_error"),
            (GwError::invalid_key(), "authentication_error"),
            (GwError::quota_exceeded("x"), "billing_error"),
            (GwError::not_allowed("x"), "permission_error"),
            (GwError::not_found("model_not_found", "x"), "not_found_error"),
            (GwError::model_retired("x"), "not_found_error"),
            (GwError::rate_limited("x", 3), "rate_limit_error"),
            (GwError::upstream_error("x"), "api_error"),
            (GwError::upstream_timeout("x"), "api_error"),
            (GwError::overloaded("x"), "api_error"),
        ] {
            let b = err.body(Envelope::Anthropic, "gw_1");
            assert_eq!(b["type"], "error");
            assert_eq!(b["error"]["type"], ty, "{}", err.kind);
        }
    }

    #[test]
    fn openai_envelope_puts_the_gateway_type_in_code() {
        let b = GwError::usage_required().body(Envelope::OpenAi, "gw_1");
        assert_eq!(b["error"]["code"], "usage_required");
        assert_eq!(b["error"]["type"], "invalid_request_error");
    }

    #[test]
    fn headers_mark_what_must_not_be_retried() {
        let r = GwError::upstream_timeout("no answer").response(Envelope::Gateway, "gw_9");
        assert_eq!(r.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(r.headers()["x-should-retry"], "false");
        assert_eq!(r.headers()["x-gw-error-type"], "upstream_timeout");
        assert_eq!(r.headers()["x-gw-request-id"], "gw_9");

        let r = GwError::rate_limited("slow down", 7).response(Envelope::OpenAi, "gw_9");
        assert_eq!(r.headers()[RETRY_AFTER], "7");
        assert!(r.headers().get("x-should-retry").is_none(), "a throttle may be retried");

        let r = GwError::quota_exceeded("spent").response(Envelope::Anthropic, "gw_9");
        assert_eq!(r.status().as_u16(), 402);
        assert_eq!(r.headers()["x-should-retry"], "false", "a spent quota does not recover by retrying");
    }
}
