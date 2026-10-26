//! The connection to providers: a pooled HTTP client, the egress policy and the attempt loop.

pub mod client;
pub mod google;
/// The rules for outbound addresses, shared with the analytics service.
pub use pacod_gw_events::egress;

pub use client::{SendError, Sent, Upgrade, Upstream};
pub use egress::EgressPolicy;
