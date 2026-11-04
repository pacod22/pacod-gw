//! The Rust client of pacod-broker.
//!
//! **Consumers** discover resources, read them, watch the catalogue change, and report how calls
//! went and how good the resources were, quoting each result's ticket:
//!
//! ```no_run
//! # async fn demo() -> Result<(), pacod_broker_client::Error> {
//! use pacod_broker_client::{BrokerClient, Outcome, Query, UsageReport};
//!
//! let broker = BrokerClient::new("http://127.0.0.1:8060")?.with_client_name("my-agent");
//! let q = Query { kind: Some("model".into()), capabilities: vec!["tools".into()],
//!                 currency: Some("EUR".into()), sort: "price:input".parse().unwrap(), ..Query::default() };
//! let found = broker.discover(&q).await?;
//! if let Some(hit) = found.hits.first() {
//!     // ... call hit.interface(Some("openai-chat")) ...
//!     let ticket = hit.ticket.clone().unwrap();
//!     broker.report_usage(&[UsageReport::new(&ticket, Outcome::Ok).latency(310.0, Some(18.0))]).await?;
//!     broker.rate(&ticket, 5, None).await?;
//! }
//! # Ok(()) }
//! ```
//!
//! **Providers** announce and stay announced:
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use pacod_broker_client::{Announcement, BrokerClient, Provider};
//!
//! let broker = BrokerClient::new("http://127.0.0.1:8060")?;
//! let announcement = Announcement::parse(&std::fs::read_to_string("provider.yaml")?)?;
//! let running = Provider::new(broker, announcement).start().await?;
//! // ... serve; set live state with provider.live() before start() ...
//! running.stop().await?;
//! # Ok(()) }
//! ```

mod client;
mod provider;
mod sse;
pub mod types;

pub use client::{Auth, BrokerClient, Error, Result};
pub use provider::{LiveHandle, Provider, ProviderEvent, Running};
pub use types::{
    Accepted, Announced, Announcement, ClientReportKind, ClientReported, DiscoverResponse, Heartbeat, HeartbeatOne,
    Hit, Live, Me, Outcome, Problem, ProviderSummary, ProviderView, Query, ResourceBrief, ResourceView, TicketInfo,
    UsageReport, WatchEvent, Why,
};

/// The broker's model types, re-exported for building announcements in code.
pub use pacod_broker_model as model;
