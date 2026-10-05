//! The contract between pacod-gw-core (producer) and pacod-gw-analytics (consumer).
//!
//! Everything that crosses between the two services is defined here: the usage event written
//! for every call, the audit record written for audited calls, the ingest batch framing, and
//! the messages of the internal job API. The ClickHouse schema in
//! `crates/pacod-gw-analytics/migrations` mirrors these structs field by field.
//!
//! It also holds the one rule both services must apply alike: which addresses they may
//! connect to on an administrator's word (`egress`).

pub mod audit;
pub mod egress;
pub mod ingest;
pub mod jobs;
pub mod usage;

pub use audit::{AuditPart, AuditRecord};
pub use usage::{Timing, Usage, UsageEvent};
