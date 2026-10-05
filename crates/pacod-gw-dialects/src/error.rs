//! What a translation can object to.

/// Why a request or a provider answer could not be translated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DialectError {
    /// The request is malformed or contradicts itself. `400 invalid_request`.
    #[error("{0}")]
    InvalidRequest(String),
    /// The request asks for something the target cannot deliver. `400 unsupported_feature`.
    /// Never silently ignored (concept section 8.2).
    #[error("{0}")]
    Unsupported(String),
    /// The provider's answer is not what its API documents. `502 upstream_error`.
    #[error("{0}")]
    BadProviderResponse(String),
}

impl DialectError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidRequest(message.into())
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }

    pub fn bad_response(message: impl Into<String>) -> Self {
        Self::BadProviderResponse(message.into())
    }
}
