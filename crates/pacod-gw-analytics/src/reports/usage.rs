//! Overview, usage over time, breakdown and accuracy.

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::params::{Bucket, Filters, Format, Group, GroupColumns, Params, Range, USAGE_FILTERS};
use super::{EVENTS, MAX_LIMIT, TOTALS_SQL, Totals, csv, series_by_group, totals};
use crate::AppState;
use crate::error::ApiError;
use crate::storage::{Binds, ClickHouseError};

// ---------------------------------------------------------------------------------------------
// GET /reports/overview

#[derive(Serialize)]
pub struct Overview {
    /// The instant the periods end at.
    to: DateTime<Utc>,
    periods: Periods,
}

#[derive(Serialize)]
struct Periods {
    hour: Period,
    day: Period,
    week: Period,
    month: Period,
}

#[derive(Serialize)]
struct Period {
    from: DateTime<Utc>,
    #[serde(flatten)]
    totals: Totals,
}

/// The hour, day, week and month that contain `to` (default: now), each from its start to `to`.
pub async fn overview(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Json<Overview>, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let to = params.time("to")?.unwrap_or_else(Utc::now);
    let filters = Filters::take(&mut params, &USAGE_FILTERS)?;
    params.finish()?;

    let clickhouse = state.clickhouse();
    let period = |bucket: Bucket| {
        let from = bucket.start_of(to);
        let filters = filters.clone().within(Range { from, to });
        async move {
            let totals = totals(clickhouse, &filters).await?;
            Ok::<_, ClickHouseError>(Period { from, totals })
        }
    };
    let (hour, day, week, month) =
        tokio::try_join!(period(Bucket::Hour), period(Bucket::Day), period(Bucket::Week), period(Bucket::Month),)?;

    Ok(Json(Overview { to, periods: Periods { hour, day, week, month } }))
}

// ---------------------------------------------------------------------------------------------
// GET /reports/usage

const USAGE_GROUPS: GroupColumns = &[
    ("provider", "provider"),
    ("model", "model_served"),
    ("template", "template"),
    ("key_id", "key_id"),
    ("end_user", "end_user"),
    ("status", "status"),
    ("mode", "mode"),
];

/// One bucket of one group, as selected and as written to CSV.
#[derive(Default, Serialize, Deserialize)]
struct UsageRow {
    bucket: DateTime<Utc>,
    group: String,
    #[serde(flatten)]
    totals: Totals,
    gateway_delay_p50_ms: f32,
    gateway_delay_p99_ms: f32,
}

#[derive(Serialize)]
struct UsageReport {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    bucket: &'static str,
    group_by: String,
    series: Vec<UsageSeries>,
}

#[derive(Serialize)]
struct UsageSeries {
    /// The value of the grouping column; `null` when the report is not grouped.
    group: Option<String>,
    points: Vec<UsagePoint>,
}

#[derive(Serialize)]
struct UsagePoint {
    bucket: DateTime<Utc>,
    #[serde(flatten)]
    totals: Totals,
    gateway_delay_p50_ms: f32,
    gateway_delay_p99_ms: f32,
}

pub async fn usage(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Response, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &USAGE_FILTERS)?.within(range);
    let bucket = Bucket::take(&mut params, Bucket::Hour)?;
    let group = Group::take(&mut params, "group_by", USAGE_GROUPS, true, Some(Group::None))?;
    let format = params.format()?;
    params.finish()?;

    let mut binds = Binds::new();
    let sql = format!(
        "WITH quantilesExact(0.5, 0.99)(gateway_delay_ms) AS delay \
         SELECT {bucket} AS bucket, {group} AS `group`, {TOTALS_SQL}, \
                delay[1] AS gateway_delay_p50_ms, delay[2] AS gateway_delay_p99_ms \
         FROM {EVENTS} {filter} \
         GROUP BY bucket, `group` \
         ORDER BY `group`, bucket",
        bucket = bucket.sql(),
        group = group.sql(&mut binds),
        filter = filters.where_sql(&mut binds),
    );
    let rows: Vec<UsageRow> = state.clickhouse().fetch(&sql, &binds).await?;

    if format == Format::Csv {
        return csv::download("usage", &UsageRow::default(), &rows);
    }

    let series = series_by_group(rows, !group.is_none(), |row| {
        let point = UsagePoint {
            bucket: row.bucket,
            totals: row.totals,
            gateway_delay_p50_ms: row.gateway_delay_p50_ms,
            gateway_delay_p99_ms: row.gateway_delay_p99_ms,
        };
        (row.group, point)
    })
    .into_iter()
    .map(|(group, points)| UsageSeries { group, points })
    .collect();

    Ok(Json(UsageReport { from: range.from, to: range.to, bucket: bucket.name(), group_by: group.label(), series })
        .into_response())
}

// ---------------------------------------------------------------------------------------------
// GET /reports/breakdown

const BREAKDOWN_GROUPS: GroupColumns = &[
    ("provider", "provider"),
    ("model", "model_served"),
    ("template", "template"),
    ("key_id", "key_id"),
    ("end_user", "end_user"),
    ("error_type", "error_type"),
];

#[derive(Default, Serialize, Deserialize)]
struct BreakdownRow {
    group: String,
    #[serde(flatten)]
    totals: Totals,
}

#[derive(Serialize)]
struct Breakdown {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    by: String,
    limit: u64,
    rows: Vec<BreakdownRow>,
}

pub async fn breakdown(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Response, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &USAGE_FILTERS)?.within(range);
    let by = Group::take(&mut params, "by", BREAKDOWN_GROUPS, true, None)?;
    let limit = params.limit(50, MAX_LIMIT)?;
    let format = params.format()?;
    params.finish()?;

    let mut binds = Binds::new();
    let sql = format!(
        "SELECT {group} AS `group`, {TOTALS_SQL} \
         FROM {EVENTS} {filter} \
         GROUP BY `group` \
         ORDER BY cost DESC, requests DESC, `group` ASC \
         LIMIT {limit}",
        group = by.sql(&mut binds),
        filter = filters.where_sql(&mut binds),
        limit = binds.uint64(limit),
    );
    let rows: Vec<BreakdownRow> = state.clickhouse().fetch(&sql, &binds).await?;

    if format == Format::Csv {
        return csv::download("breakdown", &BreakdownRow::default(), &rows);
    }
    Ok(Json(Breakdown { from: range.from, to: range.to, by: by.label(), limit, rows }).into_response())
}

// ---------------------------------------------------------------------------------------------
// GET /reports/accuracy

#[derive(Deserialize)]
struct AccuracyRow {
    requests: u64,
    total_cost: f64,
    non_provider_requests: u64,
    non_provider_cost: f64,
    price_missing_requests: u64,
}

#[derive(Serialize)]
pub struct Accuracy {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    requests: u64,
    total_cost: f64,
    /// Events whose token counts did not come from the provider.
    non_provider_requests: u64,
    non_provider_cost: f64,
    /// `non_provider_cost / total_cost`, zero when there is no cost.
    non_provider_share: f64,
    /// Events that could not be priced.
    price_missing_requests: u64,
}

pub async fn accuracy(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Json<Accuracy>, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &USAGE_FILTERS)?.within(range);
    params.finish()?;

    let mut binds = Binds::new();
    let sql = format!(
        "SELECT count() AS requests, \
                sum(ifNull(cost, 0)) AS total_cost, \
                countIf(usage_source != 'provider') AS non_provider_requests, \
                sumIf(ifNull(cost, 0), usage_source != 'provider') AS non_provider_cost, \
                countIf(price_missing) AS price_missing_requests \
         FROM {EVENTS} {filter}",
        filter = filters.where_sql(&mut binds),
    );
    let row: AccuracyRow = state.clickhouse().fetch_one(&sql, &binds).await?;

    let non_provider_share = if row.total_cost > 0.0 { row.non_provider_cost / row.total_cost } else { 0.0 };
    Ok(Json(Accuracy {
        from: range.from,
        to: range.to,
        requests: row.requests,
        total_cost: row.total_cost,
        non_provider_requests: row.non_provider_requests,
        non_provider_cost: row.non_provider_cost,
        non_provider_share,
        price_missing_requests: row.price_missing_requests,
    }))
}
