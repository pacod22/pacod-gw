//! Storage: ClickHouse over HTTP. The schema lives in `../migrations`.

pub mod attachments;
pub mod clickhouse;
pub mod migrations;
pub mod rows;

pub use clickhouse::{Binds, ClickHouse, ClickHouseError};
