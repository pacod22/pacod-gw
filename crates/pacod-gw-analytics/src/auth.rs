//! The shared-secret check between the core and this service.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use pacod_gw_events::ingest::INTERNAL_TOKEN_HEADER;
use subtle::ConstantTimeEq;

use crate::AppState;
use crate::error::ApiError;

/// Lets a request pass only when it carries the internal token.
pub async fn require_token(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let presented = request.headers().get(INTERNAL_TOKEN_HEADER).map(|value| value.as_bytes()).unwrap_or_default();
    if !token_matches(presented, state.internal_token().as_bytes()) {
        return ApiError::unauthorized().into_response();
    }
    next.run(request).await
}

/// Compares in time that does not depend on where the two values differ.
fn token_matches(presented: &[u8], expected: &[u8]) -> bool {
    presented.ct_eq(expected).into()
}

#[cfg(test)]
mod tests {
    use super::token_matches;

    #[test]
    fn only_the_exact_token_matches() {
        let expected = b"0123456789abcdef";
        assert!(token_matches(b"0123456789abcdef", expected));
        assert!(!token_matches(b"0123456789abcdeF", expected));
        assert!(!token_matches(b"0123456789abcde", expected));
        assert!(!token_matches(b"0123456789abcdef0", expected));
        assert!(!token_matches(b"", expected));
    }
}
