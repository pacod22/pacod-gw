//! Reports: every query the core forwards for the UI (concept section 14.5). The HTTP contract
//! is written down in `API.md`.
//!
//! Every report reads `usage_events FINAL` (or `message_audit FINAL`). `FINAL` collapses the
//! copies of a row that at-least-once delivery can leave behind, so a batch that arrives twice
//! never counts twice. The overview, the breakdown by the rollup's columns and the end-user list
//! read the rollups for built buckets and the events for the rest (`rollup.rs`); their answers
//! are the same as from the events alone.
//!
//! A note for whoever writes the SQL here: ClickHouse substitutes a `SELECT` alias wherever its
//! name appears in the same `SELECT`. `sum(cost) AS cost` is fine on its own, but a second
//! expression that reads the column `cost` would then read the sum. Give an aggregate the name
//! of its column only when nothing else in that `SELECT` reads the column.

use axum::Router;
use axum::extract::Path;
use axum::extract::rejection::PathRejection;
use axum::routing::{delete, get};
use pacod_gw_events::UsageEvent;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::ApiError;
use crate::storage::{Binds, ClickHouse, ClickHouseError};

mod audit;
mod csv;
mod delay;
mod end_users;
mod errors;
mod params;
mod ranking;
mod requests;
mod rollup;
mod usage;

use params::Filters;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/reports/overview", get(usage::overview))
        .route("/reports/usage", get(usage::usage))
        .route("/reports/breakdown", get(usage::breakdown))
        .route("/reports/last-used", get(usage::last_used))
        .route("/reports/accuracy", get(usage::accuracy))
        .route("/reports/ranking", get(ranking::report))
        .route("/reports/requests", get(requests::list))
        .route("/reports/requests/{id}", get(requests::one))
        .route("/reports/traces/{trace_id}", get(requests::trace))
        .route("/reports/end-users", get(end_users::list))
        .route("/reports/end-users/{id}", get(end_users::one))
        .route("/reports/errors", get(errors::report))
        .route("/reports/delay", get(delay::report))
        .route("/reports/audit", get(audit::list))
        .route("/reports/audit/{audit_id}", get(audit::one))
        .route("/audit", delete(audit::erase))
}

/// Usage events without duplicates.
const EVENTS: &str = "usage_events FINAL";

/// Audit records without duplicates.
const AUDITS: &str = "message_audit FINAL";

/// Largest `limit` any report accepts.
const MAX_LIMIT: u64 = 1000;

/// The counters every aggregated row carries, under the same names in every report.
/// A failure is an event whose status is not `ok`.
///
/// Costs are in the reporting currency, as converted when the call settled
/// (`cost_reporting`). An event without a converted cost (written before conversion existed,
/// or in a currency without an exchange rate) counts with its cost as recorded; one without
/// a cost as zero. Every report that adds up costs uses this same expression.
const TOTALS_SQL: &str = "count() AS requests, \
     countIf(status != 'ok') AS errors, \
     sum(input_tokens) AS input_tokens, \
     sum(output_tokens) AS output_tokens, \
     sum(cache_read_tokens) AS cache_read_tokens, \
     sum(cache_write_tokens) AS cache_write_tokens, \
     sum(reasoning_tokens) AS reasoning_tokens, \
     sum(ifNull(cost_reporting, ifNull(cost, 0))) AS cost";

/// See [`TOTALS_SQL`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub requests: u64,
    pub errors: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
}

/// The totals of all events that match `filters`.
async fn totals(clickhouse: &ClickHouse, filters: &Filters) -> Result<Totals, ClickHouseError> {
    let mut binds = Binds::new();
    if let Some(source) = rollup::hourly(clickhouse, filters, &mut binds, chrono::Utc::now()).await? {
        let sql = format!("SELECT {} FROM ({source})", rollup::SOURCE_TOTALS);
        return clickhouse.fetch_one(&sql, &binds).await;
    }
    let mut binds = Binds::new();
    let sql = format!("SELECT {TOTALS_SQL} FROM {EVENTS} {}", filters.where_sql(&mut binds));
    clickhouse.fetch_one(&sql, &binds).await
}

/// Complete events that match `filters`, in the given fixed order.
async fn events(
    clickhouse: &ClickHouse,
    filters: &Filters,
    order: &'static str,
    limit: u64,
) -> Result<Vec<UsageEvent>, ClickHouseError> {
    let mut binds = Binds::new();
    let sql = format!(
        "SELECT * FROM {EVENTS} {} ORDER BY {order} LIMIT {}",
        filters.where_sql(&mut binds),
        binds.uint64(limit)
    );
    clickhouse.fetch(&sql, &binds).await
}

/// The one variable segment of a path, percent-decoded. Handlers take the extractor as a
/// `Result` so that a segment that does not decode is refused in the service's own error shape.
type Segment = Result<Path<String>, PathRejection>;

fn segment(path: Segment) -> Result<String, ApiError> {
    path.map(|Path(value)| value)
        .map_err(|_| ApiError::bad_request("invalid_path", "the path is not valid percent-encoded UTF-8"))
}

/// Turns rows that arrive ordered by group, then time, into one list of points per group.
/// `split` gives the group and the point of a row. A report that is not grouped (`grouped` is
/// false) has a single series without a group name.
fn series_by_group<R, P>(
    rows: Vec<R>,
    grouped: bool,
    split: impl Fn(R) -> (String, P),
) -> Vec<(Option<String>, Vec<P>)> {
    let mut series: Vec<(Option<String>, Vec<P>)> = Vec::new();
    for row in rows {
        let (group, point) = split(row);
        let group = grouped.then_some(group);
        match series.last_mut() {
            Some((current, points)) if *current == group => points.push(point),
            _ => series.push((group, vec![point])),
        }
    }
    series
}

/// Newest first. The id breaks ties, so the order is total and a cursor can follow it.
const NEWEST_FIRST: &str = "timestamp DESC, id DESC";

#[cfg(test)]
mod tests {
    use super::series_by_group;

    #[test]
    fn consecutive_rows_of_a_group_form_a_series() {
        let rows = vec![("", 1), ("a", 2), ("a", 3), ("b", 4)];
        let split = |(group, point): (&str, u32)| (group.to_owned(), point);

        assert_eq!(
            series_by_group(rows.clone(), true, split),
            vec![(Some(String::new()), vec![1]), (Some("a".to_owned()), vec![2, 3]), (Some("b".to_owned()), vec![4]),]
        );
        // Not grouped: one series, whatever the rows say.
        assert_eq!(series_by_group(rows, false, split), vec![(None, vec![1, 2, 3, 4])]);
        assert!(series_by_group(Vec::<(&str, u32)>::new(), true, split).is_empty());
    }
}
