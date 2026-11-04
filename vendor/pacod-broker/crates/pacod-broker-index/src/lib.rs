//! The broker's index and query engine (docs/concept.md §1 and §4).
//!
//! The registry builds an [`Index`] from its records whenever they change (coalesced, at most
//! every few milliseconds) and swaps it in atomically; queries read it without a lock. What
//! changes often (lease expiry, live status, capacity in use, observed latency) is not in the
//! index but in each entry's [`Live`] cell of atomics, which heartbeats update in place.
//!
//! A query narrows the candidates through posting lists (kind, tag, protocol, capability,
//! region, provider, GPU model, software, text tokens), then checks each candidate against the
//! filters that need time (prices and availability at an instant or over a span) or the
//! caller (visibility, client reputation). Results are sorted within one kind, ties broken by
//! id, and each carries why it matched and the values it was ranked by.

mod build;
mod live;
mod query;
mod text;

#[doc(hidden)]
pub mod fixtures;

pub use build::{Entry, Index, ProviderEntry};
pub use live::{Live, LiveView};
pub use query::{
    AvailabilityQuery, Coverage, Ctx, HardwareQuery, Hit, ModelQuery, NoScores, Query, QueryError, Results, Scores,
    SoftwareQuery, Sort, Viewer,
};
