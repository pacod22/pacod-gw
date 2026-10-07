//! pacod-gw-core: the gateway's data plane and management API.
//!
//! See `docs/ai-gateway-concept.md` for the design. The crate is a library so that tests and
//! the tools in `pacod-gw-testkit` can start a gateway in-process.

pub mod app;
pub mod bootstrap;
pub mod cluster;
pub mod cost;
pub mod crypto;
pub mod dataplane;
pub mod extract;
pub mod json;
pub mod limits;
pub mod mail;
pub mod mgmt;
pub mod model;
pub mod params;
pub mod relay;
pub mod resolve;
pub mod routing;
pub mod snapshot;
pub mod spool;
pub mod store;
pub mod telemetry;
pub mod upstream;

/// Server-sent event framing lives with the dialect translators, which need it too.
pub use pacod_gw_dialects::sse;
