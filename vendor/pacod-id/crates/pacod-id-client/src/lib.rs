//! pacod-id for a product's Rust service.
//!
//! - [`IdClient`]: the internal API with the service's client credentials: the catalogue,
//!   organisations and scopes, the feed, introspection, session revocation, token exchange.
//! - [`Verifier`]: checks the access tokens browsers and apps bring (`at+jwt`, issuer, audience).
//! - [`AuthzCache`]: the product's copy of the bindings, kept current by the feed.
//! - [`Introspector`]: service tokens (`pidt_`, imported `pgwa_`), cached 60 seconds.
//! - [`Authenticator`]: all of the above for one request: who is calling, refused when the session
//!   ended or the principal is disabled.
//! - [`TokenExchanger`]: the short tokens a product's facade calls pacod-id's admin API with.
//!
//! Tokens are checked with `pacod-id-jose` (ring), not `jsonwebtoken`: one implementation of JWS
//! for the server and its clients.
//!
//! Two parts live elsewhere:
//! - The scope outbox is the product's own table, written in the product's transactions, next to
//!   its other tables. This crate gives it the calls
//!   ([`IdClient::put_organisation`], [`IdClient::put_scope`] …) and their error codes. A
//!   delivery must go in order and stop at `scope_unknown` or `catalog_missing` rather than
//!   skip ahead: a workspace sent before its organisation fails.
//! - No axum extractor: a product keeps its own `Principal` extractor and calls
//!   [`Authenticator::authenticate`] from it.

mod authenticate;
mod cache;
mod client;
mod introspect;
mod verifier;

pub use authenticate::{AuthError, Authenticator, Caller};
pub use cache::{AuthzCache, Event, Held, Snapshot};
pub use client::{ApiError, IdClient, IdConfig, Pushed, TokenExchanger, Tokens};
pub use introspect::{Introspector, ServicePrincipal};
pub use verifier::{AccessToken, Verifier, VerifyError};

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    let d = ring::digest::digest(&ring::digest::SHA256, data);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
