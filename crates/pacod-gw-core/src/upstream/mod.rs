//! The connection to providers: a pooled HTTP client, the egress policy and the attempt loop.

pub mod client;
pub mod egress;

pub use client::{SendError, Sent, Upstream};
pub use egress::EgressPolicy;
