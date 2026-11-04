//! The data model of pacod-broker (docs/concept.md §2).
//!
//! A **provider** is the principal that announces resources; it has a profile with a
//! human-readable description and, optionally, an inventory of the hardware and software it runs
//! on. A **resource** is one endpoint someone can call: a model, an MCP server, an agent, a tool,
//! compute. Each resource carries its interfaces, capabilities, prices over time, availability
//! over time, data policy and who may see it.
//!
//! Everything here is pure: types, validation and the time resolution of prices and
//! availability. The index (pacod-broker-index) and the binary build on it.

pub mod availability;
pub mod ids;
pub mod limits;
pub mod pricing;
pub mod provider;
pub mod reputation;
pub mod resource;
pub mod schedule;
pub mod text;
pub mod validate;

pub use availability::{
    Availability, AvailabilitySummary, LiveStatus, Maintenance, Rule, RuleState, Schedule, Slot, SlotState,
};
pub use ids::{provider_id, resource_id};
pub use pricing::{EffectivePrice, PricePeriod, PriceSummary, Pricing, RateRange, Rates};
pub use provider::{HardwareItem, HardwareType, Inventory, ProviderProfile, SoftwareItem, SoftwareKind};
pub use reputation::{Component, Label, Reputation};
pub use resource::{
    Access, Auth, AuthScheme, Capabilities, Interface, Modalities, ModelInfo, Policy, ResourceRecord, ResourceSpec,
    RunsOn, Source, Visibility,
};
pub use text::{Example, Text};
pub use validate::Problems;

/// The time type of the whole model: an instant in UTC, RFC 3339 on the wire.
pub type Timestamp = jiff::Timestamp;
