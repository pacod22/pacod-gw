//! Integration tests: the service in-process on an ephemeral port, against a real ClickHouse.
//!
//! Set `GW_TEST_CLICKHOUSE_URL` to ClickHouse's HTTP interface (see `support::clickhouse_url`).
//! Every test creates a database of its own and drops it at the end, so the tests run in
//! parallel. Without the variable every test that needs ClickHouse fails.

mod api_doc;
mod audit;
mod ingest;
mod reports;
mod security;
mod service;
mod support;
