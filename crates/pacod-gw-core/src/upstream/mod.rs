//! The connection to providers: a pooled HTTP client, the egress policy and the attempt loop.

pub mod client;
/// The rules for outbound addresses, shared with the analytics service.
pub use pacod_gw_events::egress;

pub use client::{SendError, Sent, Upstream};
pub use egress::EgressPolicy;
