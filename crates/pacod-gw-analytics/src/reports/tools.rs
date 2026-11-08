//! The records of tool and agent calls (catalogue MCP-14): a page of them, and how they add up
//! by one of their columns. They are a table of their own (`tool_calls`), so that no report of
//! tokens, cost or model calls counts them.

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use pacod_gw_events::ToolCallRecord;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::params::{Cursor, Filters, Format, Group, GroupColumns, Params, TOOL_FILTERS};
use super::{MAX_LIMIT, NEWEST_FIRST, csv};
use crate::AppState;
use crate::error::ApiError;
use crate::storage::{Binds, ClickHouse, ClickHouseError};

/// Records without duplicates.
pub(super) const TOOL_CALLS: &str = "tool_calls FINAL";

// ---------------------------------------------------------------------------------------------
// GET /reports/tool-calls

#[derive(Serialize)]
struct ToolCallPage {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: u64,
    tool_calls: Vec<ToolCallRecord>,
    /// Pass as `cursor` to get the next page; `null` on the last page.
    next_cursor: Option<String>,
}

pub async fn list(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Response, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &TOOL_FILTERS)?.within(range);
    let limit = params.limit(50, MAX_LIMIT)?;
    let cursor = Cursor::take(&mut params)?;
    let format = params.format()?;
    params.finish()?;

    let mut binds = Binds::new();
    let after_cursor = cursor.map(|cursor| cursor.before_uuid("id", &mut binds)).transpose()?;
    // One row more than asked for tells whether another page follows.
    let sql = format!(
        "SELECT * FROM {TOOL_CALLS} {filter} ORDER BY {NEWEST_FIRST} LIMIT {limit}",
        filter = filters.where_with(&mut binds, after_cursor),
        limit = binds.uint64(limit + 1),
    );
    let mut tool_calls: Vec<ToolCallRecord> = state.clickhouse().fetch(&sql, &binds).await?;

    let has_more = tool_calls.len() as u64 > limit;
    tool_calls.truncate(limit as usize);
    let next_cursor = match (has_more, tool_calls.last()) {
        (true, Some(last)) => Some(Cursor::encode(last.timestamp, &last.id.to_string())),
        _ => None,
    };
    if format == Format::Csv {
        return csv::download("tool-calls", &ToolCallRecord::new(Uuid::nil(), ""), &tool_calls);
    }
    Ok(Json(ToolCallPage { from: range.from, to: range.to, limit, tool_calls, next_cursor }).into_response())
}

/// The records of one trace in time order, at most [`MAX_LIMIT`]: what `/reports/traces` shows
/// beside the model calls.
pub(super) async fn of_trace(
    clickhouse: &ClickHouse,
    filters: &Filters,
) -> Result<Vec<ToolCallRecord>, ClickHouseError> {
    let mut binds = Binds::new();
    let sql = format!(
        "SELECT * FROM {TOOL_CALLS} {filter} ORDER BY timestamp ASC, id ASC LIMIT {limit}",
        filter = filters.where_sql(&mut binds),
        limit = binds.uint64(MAX_LIMIT),
    );
    clickhouse.fetch(&sql, &binds).await
}

// ---------------------------------------------------------------------------------------------
// GET /reports/tool-calls/breakdown

const TOOL_GROUPS: GroupColumns = &[
    ("organisation_id", "organisation_id"),
    ("workspace_id", "workspace_id"),
    ("key_id", "key_id"),
    ("end_user", "end_user"),
    ("agent_id", "agent_id"),
    ("user_id", "user_id"),
    ("task", "task"),
    ("protocol", "protocol"),
    ("target", "target"),
    ("operation", "operation"),
    ("tool", "exposed_name"),
    ("verdict", "verdict"),
    ("status", "status"),
    ("error_type", "error_type"),
];

/// The counters of a group of calls. A failure is a call whose status is not `ok`; a refusal
/// one the gateway did not pass on (its verdict is not `allowed`), which is a failure too.
/// Percentiles of `total_ms` are exact, as everywhere in these reports.
#[derive(Default, Serialize, Deserialize)]
struct ToolRow {
    group: String,
    calls: u64,
    errors: u64,
    refused: u64,
    result_errors: u64,
    request_bytes: u64,
    response_bytes: u64,
    p50_ms: f64,
    p95_ms: f64,
}

#[derive(Serialize)]
struct ToolBreakdown {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    by: String,
    limit: u64,
    rows: Vec<ToolRow>,
}

pub async fn breakdown(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Response, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &TOOL_FILTERS)?.within(range);
    let by = Group::take(&mut params, "by", TOOL_GROUPS, true, None)?;
    let limit = params.limit(50, MAX_LIMIT)?;
    let format = params.format()?;
    params.finish()?;

    let mut binds = Binds::new();
    let sql = format!(
        "WITH quantilesExact(0.5, 0.95)(total_ms) AS q \
         SELECT {group} AS `group`, \
                count() AS calls, \
                countIf(status != 'ok') AS errors, \
                countIf(verdict != 'allowed') AS refused, \
                countIf(result_error) AS result_errors, \
                sum(request_bytes) AS request_bytes, \
                sum(response_bytes) AS response_bytes, \
                q[1] AS p50_ms, \
                q[2] AS p95_ms \
         FROM {TOOL_CALLS} {filter} \
         GROUP BY `group` \
         ORDER BY calls DESC, `group` ASC \
         LIMIT {limit}",
        group = by.sql(&mut binds),
        filter = filters.where_sql(&mut binds),
        limit = binds.uint64(limit),
    );
    let rows: Vec<ToolRow> = state.clickhouse().fetch(&sql, &binds).await?;
    if format == Format::Csv {
        return csv::download("tool-calls-breakdown", &ToolRow::default(), &rows);
    }
    Ok(Json(ToolBreakdown { from: range.from, to: range.to, by: by.label(), limit, rows }).into_response())
}
