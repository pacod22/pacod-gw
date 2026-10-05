//! End users: the list with consumption, and the drill-down for one end user.

use axum::Json;
use axum::extract::{RawQuery, State};
use chrono::{DateTime, Utc};
use pacod_gw_events::UsageEvent;
use serde::{Deserialize, Serialize};

use super::params::{Filters, Params, USAGE_FILTERS};
use super::{EVENTS, MAX_LIMIT, NEWEST_FIRST, Segment, TOTALS_SQL, Totals, events, segment, totals};
use crate::AppState;
use crate::error::ApiError;
use crate::storage::Binds;

// ---------------------------------------------------------------------------------------------
// GET /reports/end-users

#[derive(Serialize, Deserialize)]
struct EndUserRow {
    end_user: String,
    #[serde(flatten)]
    totals: Totals,
    first_request_at: DateTime<Utc>,
    last_request_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct EndUsers {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: u64,
    offset: u64,
    rows: Vec<EndUserRow>,
}

/// One row per end user, largest cost first. Events without an end user are left out.
pub async fn list(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Json<EndUsers>, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &USAGE_FILTERS)?.within(range).and("end_user != ''");
    let limit = params.limit(100, MAX_LIMIT)?;
    let offset = params.parsed::<u64>("offset", "a whole number")?.unwrap_or(0);
    params.finish()?;

    let mut binds = Binds::new();
    let sql = format!(
        "SELECT end_user, {TOTALS_SQL}, \
                min(timestamp) AS first_request_at, \
                max(timestamp) AS last_request_at \
         FROM {EVENTS} {filter} \
         GROUP BY end_user \
         ORDER BY cost DESC, requests DESC, end_user ASC \
         LIMIT {limit} OFFSET {offset}",
        filter = filters.where_sql(&mut binds),
        limit = binds.uint64(limit),
        offset = binds.uint64(offset),
    );
    let rows = state.clickhouse().fetch(&sql, &binds).await?;

    Ok(Json(EndUsers { from: range.from, to: range.to, limit, offset, rows }))
}

// ---------------------------------------------------------------------------------------------
// GET /reports/end-users/{id}

#[derive(Serialize, Deserialize)]
struct ModelRow {
    model: String,
    #[serde(flatten)]
    totals: Totals,
}

#[derive(Serialize)]
pub struct EndUser {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    end_user: String,
    totals: Totals,
    /// Largest cost first.
    by_model: Vec<ModelRow>,
    /// Newest first, at most `limit`.
    recent_requests: Vec<UsageEvent>,
}

/// The consumption of one end user in the range. An end user without events in the range is
/// reported with zero totals, not as missing.
pub async fn one(
    State(state): State<AppState>,
    end_user: Segment,
    RawQuery(query): RawQuery,
) -> Result<Json<EndUser>, ApiError> {
    let end_user = segment(end_user)?;
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &USAGE_FILTERS)?.within(range).equal("end_user", end_user.clone());
    let limit = params.limit(20, MAX_LIMIT)?;
    params.finish()?;

    let clickhouse = state.clickhouse();
    let mut binds = Binds::new();
    let by_model_sql = format!(
        "SELECT toString(model_served) AS model, {TOTALS_SQL} \
         FROM {EVENTS} {filter} \
         GROUP BY model \
         ORDER BY cost DESC, requests DESC, model ASC",
        filter = filters.where_sql(&mut binds),
    );
    let (totals, by_model, recent_requests) = tokio::try_join!(
        totals(clickhouse, &filters),
        clickhouse.fetch::<ModelRow>(&by_model_sql, &binds),
        events(clickhouse, &filters, NEWEST_FIRST, limit),
    )?;

    Ok(Json(EndUser { from: range.from, to: range.to, end_user, totals, by_model, recent_requests }))
}
