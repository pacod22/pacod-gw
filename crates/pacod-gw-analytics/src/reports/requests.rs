//! The request log: a page of events, one event, and the events of one trace.

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use pacod_gw_events::UsageEvent;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::params::{Cursor, Filters, Format, Params, USAGE_FILTERS, USAGE_SCOPE};
use super::{EVENTS, MAX_LIMIT, NEWEST_FIRST, Segment, TOTALS_SQL, Totals, csv, events, segment};
use crate::AppState;
use crate::error::ApiError;
use crate::storage::Binds;

/// Response header that carries the cursor of the next page of a CSV download.
const NEXT_CURSOR_HEADER: &str = "x-next-cursor";

// ---------------------------------------------------------------------------------------------
// GET /reports/requests

#[derive(Serialize)]
struct RequestPage {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: u64,
    requests: Vec<UsageEvent>,
    /// Pass as `cursor` to get the next page; `null` on the last page.
    next_cursor: Option<String>,
}

pub async fn list(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Response, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let mut filters = Filters::take(&mut params, &USAGE_FILTERS)?.within(range);
    if let Some(trace_id) = params.one("trace_id")? {
        filters = filters.equal("trace_id", trace_id);
    }
    if let Some(session_id) = params.one("session_id")? {
        filters = filters.equal("session_id", session_id);
    }
    match params.flag("audited")? {
        Some(true) => filters = filters.and("audit_id != ''"),
        Some(false) => filters = filters.and("audit_id = ''"),
        None => {}
    }
    let limit = params.limit(50, MAX_LIMIT)?;
    let cursor = Cursor::take(&mut params)?;
    let format = params.format()?;
    params.finish()?;

    let mut binds = Binds::new();
    let after_cursor = cursor.map(|cursor| cursor.before_uuid("id", &mut binds)).transpose()?;
    // One row more than asked for tells whether another page follows.
    let sql = format!(
        "SELECT * FROM {EVENTS} {filter} ORDER BY {NEWEST_FIRST} LIMIT {limit}",
        filter = filters.where_with(&mut binds, after_cursor),
        limit = binds.uint64(limit + 1),
    );
    let mut requests: Vec<UsageEvent> = state.clickhouse().fetch(&sql, &binds).await?;

    let has_more = requests.len() as u64 > limit;
    requests.truncate(limit as usize);
    let next_cursor = match (has_more, requests.last()) {
        (true, Some(last)) => Some(Cursor::encode(last.timestamp, &last.id.to_string())),
        _ => None,
    };

    if format == Format::Csv {
        let sample = UsageEvent::new(Uuid::nil(), "");
        let mut response = csv::download("requests", &sample, &requests)?;
        if let Some(cursor) = next_cursor
            && let Ok(value) = HeaderValue::from_str(&cursor)
        {
            response.headers_mut().insert(NEXT_CURSOR_HEADER, value);
        }
        return Ok(response);
    }
    Ok(Json(RequestPage { from: range.from, to: range.to, limit, requests, next_cursor }).into_response())
}

// ---------------------------------------------------------------------------------------------
// GET /reports/requests/{id}

/// One event by id. Restricted by scope only; a time range is applied only when given.
pub async fn one(
    State(state): State<AppState>,
    id: Segment,
    RawQuery(query): RawQuery,
) -> Result<Json<UsageEvent>, ApiError> {
    let id = Uuid::parse_str(&segment(id)?)
        .map_err(|_| ApiError::bad_request("invalid_id", "the request id must be a UUID"))?;
    let mut params = Params::parse(query.as_deref());
    let range = params.optional_range()?;
    let filters = Filters::take(&mut params, &USAGE_SCOPE)?.between(range);
    params.finish()?;

    let mut binds = Binds::new();
    let by_id = format!("id = {}", binds.uuid(id));
    let sql = format!("SELECT * FROM {EVENTS} {filter} LIMIT 1", filter = filters.where_with(&mut binds, Some(by_id)),);
    let mut found: Vec<UsageEvent> = state.clickhouse().fetch(&sql, &binds).await?;
    found.pop().map(Json).ok_or_else(|| ApiError::not_found("request"))
}

// ---------------------------------------------------------------------------------------------
// GET /reports/traces/{trace_id}

#[derive(Serialize, Deserialize)]
struct TraceTotals {
    #[serde(flatten)]
    totals: Totals,
    /// Sum of the gateway delay of every call of the trace.
    gateway_delay_ms: f64,
    /// Time stamp of the first and of the last call.
    first_request_at: DateTime<Utc>,
    last_request_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct Trace {
    trace_id: String,
    totals: TraceTotals,
    /// The calls in time order, at most 1000.
    requests: Vec<UsageEvent>,
}

/// The calls of one trace with their totals. Restricted by scope only; a time range is applied
/// only when given.
pub async fn trace(
    State(state): State<AppState>,
    trace_id: Segment,
    RawQuery(query): RawQuery,
) -> Result<Response, ApiError> {
    let trace_id = segment(trace_id)?;
    let mut params = Params::parse(query.as_deref());
    let range = params.optional_range()?;
    let filters = Filters::take(&mut params, &USAGE_SCOPE)?.between(range).equal("trace_id", trace_id.clone());
    let format = params.format()?;
    params.finish()?;

    let clickhouse = state.clickhouse();
    let mut binds = Binds::new();
    let totals_sql = format!(
        "SELECT {TOTALS_SQL}, \
                sum(gateway_delay_ms) AS gateway_delay_ms, \
                min(timestamp) AS first_request_at, \
                max(timestamp) AS last_request_at \
         FROM {EVENTS} {filter}",
        filter = filters.where_sql(&mut binds),
    );
    let (totals, requests) = tokio::try_join!(
        clickhouse.fetch_one::<TraceTotals>(&totals_sql, &binds),
        events(clickhouse, &filters, "timestamp ASC, id ASC", MAX_LIMIT),
    )?;
    if totals.totals.requests == 0 {
        return Err(ApiError::not_found("trace"));
    }
    if format == Format::Csv {
        // One line per call, as the request log writes them.
        return csv::download("trace", &UsageEvent::new(Uuid::nil(), ""), &requests);
    }

    Ok(Json(Trace { trace_id, totals, requests }).into_response())
}
