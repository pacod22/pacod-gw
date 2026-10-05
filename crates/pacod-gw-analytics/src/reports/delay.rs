//! Gateway delay (concept section 14.2): percentiles over time, the average of each timing
//! stage, and the slowest calls.
//!
//! Percentiles are exact (`quantilesExact`): the value at position `floor(level * n)` of the
//! sorted delays, without interpolation.

use axum::Json;
use axum::extract::{RawQuery, State};
use chrono::{DateTime, Utc};
use pacod_gw_events::UsageEvent;
use serde::{Deserialize, Serialize};

use super::params::{Bucket, Filters, Group, GroupColumns, Params, USAGE_FILTERS};
use super::{EVENTS, MAX_LIMIT, events, series_by_group};
use crate::AppState;
use crate::error::ApiError;
use crate::storage::Binds;

const DELAY_GROUPS: GroupColumns = &[
    ("mode", "mode"),
    ("provider", "provider"),
    ("model", "model_served"),
    ("template", "template"),
    ("workspace_id", "workspace_id"),
    ("instance_id", "instance_id"),
];

/// Median, 95th and 99th percentile and maximum of `gateway_delay_ms`.
const PERCENTILES_SQL: &str = "delay[1] AS gateway_delay_p50_ms, \
     delay[2] AS gateway_delay_p95_ms, \
     delay[3] AS gateway_delay_p99_ms, \
     max(gateway_delay_ms) AS gateway_delay_max_ms";

const WITH_DELAY: &str = "WITH quantilesExact(0.5, 0.95, 0.99)(gateway_delay_ms) AS delay";

/// The overall figures and the stage averages come from one query. The averages are selected
/// under an `avg_` prefix because the percentiles read `gateway_delay_ms` in the same `SELECT`.
#[derive(Deserialize)]
struct SummaryRow {
    requests: u64,
    gateway_delay_p50_ms: Option<f32>,
    gateway_delay_p95_ms: Option<f32>,
    gateway_delay_p99_ms: Option<f32>,
    gateway_delay_max_ms: Option<f32>,
    avg_gateway_delay_ms: Option<f64>,
    avg_receive_ms: Option<f64>,
    avg_pre_ms: Option<f64>,
    avg_connect_ms: Option<f64>,
    avg_retry_ms: Option<f64>,
    avg_upstream_first_byte_ms: Option<f64>,
    avg_upstream_stream_ms: Option<f64>,
    avg_post_ms: Option<f64>,
    avg_relay_max_ms: Option<f64>,
    avg_settle_ms: Option<f64>,
    avg_total_ms: Option<f64>,
}

/// All figures are `null` when no event matches.
#[derive(Serialize)]
struct Summary {
    requests: u64,
    gateway_delay_p50_ms: Option<f32>,
    gateway_delay_p95_ms: Option<f32>,
    gateway_delay_p99_ms: Option<f32>,
    gateway_delay_max_ms: Option<f32>,
}

/// The average of each timing field of the events, in milliseconds.
#[derive(Serialize)]
struct StageAverages {
    gateway_delay_ms: Option<f64>,
    receive_ms: Option<f64>,
    pre_ms: Option<f64>,
    connect_ms: Option<f64>,
    retry_ms: Option<f64>,
    upstream_first_byte_ms: Option<f64>,
    upstream_stream_ms: Option<f64>,
    post_ms: Option<f64>,
    relay_max_ms: Option<f64>,
    settle_ms: Option<f64>,
    total_ms: Option<f64>,
}

#[derive(Deserialize)]
struct PointRow {
    bucket: DateTime<Utc>,
    group: String,
    requests: u64,
    gateway_delay_p50_ms: f32,
    gateway_delay_p95_ms: f32,
    gateway_delay_p99_ms: f32,
    gateway_delay_max_ms: f32,
}

#[derive(Serialize)]
struct Point {
    bucket: DateTime<Utc>,
    requests: u64,
    gateway_delay_p50_ms: f32,
    gateway_delay_p95_ms: f32,
    gateway_delay_p99_ms: f32,
    gateway_delay_max_ms: f32,
}

#[derive(Serialize)]
struct Series {
    /// The value of the grouping column; `null` when the report is not grouped.
    group: Option<String>,
    points: Vec<Point>,
}

#[derive(Serialize)]
pub struct Delay {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    bucket: &'static str,
    group_by: String,
    summary: Summary,
    stage_averages: StageAverages,
    series: Vec<Series>,
    /// Largest gateway delay first, at most `limit`.
    slowest_requests: Vec<UsageEvent>,
}

pub async fn report(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Json<Delay>, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &USAGE_FILTERS)?.within(range);
    let bucket = Bucket::take(&mut params, Bucket::Hour)?;
    let group = Group::take(&mut params, "group_by", DELAY_GROUPS, false, Some(Group::None))?;
    let limit = params.limit(20, MAX_LIMIT)?;
    params.finish()?;

    let clickhouse = state.clickhouse();

    let mut summary_binds = Binds::new();
    let summary_sql = format!(
        "{WITH_DELAY} \
         SELECT count() AS requests, {PERCENTILES_SQL}, \
                avg(gateway_delay_ms) AS avg_gateway_delay_ms, \
                avg(receive_ms) AS avg_receive_ms, \
                avg(pre_ms) AS avg_pre_ms, \
                avg(connect_ms) AS avg_connect_ms, \
                avg(retry_ms) AS avg_retry_ms, \
                avg(upstream_first_byte_ms) AS avg_upstream_first_byte_ms, \
                avg(upstream_stream_ms) AS avg_upstream_stream_ms, \
                avg(post_ms) AS avg_post_ms, \
                avg(relay_max_ms) AS avg_relay_max_ms, \
                avg(settle_ms) AS avg_settle_ms, \
                avg(total_ms) AS avg_total_ms \
         FROM {EVENTS} {filter}",
        filter = filters.where_sql(&mut summary_binds),
    );

    let mut series_binds = Binds::new();
    let series_sql = format!(
        "{WITH_DELAY} \
         SELECT {bucket} AS bucket, {group} AS `group`, count() AS requests, {PERCENTILES_SQL} \
         FROM {EVENTS} {filter} \
         GROUP BY bucket, `group` \
         ORDER BY `group`, bucket",
        bucket = bucket.sql(),
        group = group.sql(&mut series_binds),
        filter = filters.where_sql(&mut series_binds),
    );

    let (summary, rows, slowest_requests) = tokio::try_join!(
        clickhouse.fetch_one::<SummaryRow>(&summary_sql, &summary_binds),
        clickhouse.fetch::<PointRow>(&series_sql, &series_binds),
        events(clickhouse, &filters, "gateway_delay_ms DESC, timestamp DESC, id DESC", limit),
    )?;

    let series = series_by_group(rows, !group.is_none(), |row| {
        let point = Point {
            bucket: row.bucket,
            requests: row.requests,
            gateway_delay_p50_ms: row.gateway_delay_p50_ms,
            gateway_delay_p95_ms: row.gateway_delay_p95_ms,
            gateway_delay_p99_ms: row.gateway_delay_p99_ms,
            gateway_delay_max_ms: row.gateway_delay_max_ms,
        };
        (row.group, point)
    })
    .into_iter()
    .map(|(group, points)| Series { group, points })
    .collect();

    // Without events ClickHouse reports the percentiles as zero; say "no value" instead.
    let measured = |value: Option<f32>| value.filter(|_| summary.requests > 0);
    Ok(Json(Delay {
        from: range.from,
        to: range.to,
        bucket: bucket.name(),
        group_by: group.label(),
        summary: Summary {
            requests: summary.requests,
            gateway_delay_p50_ms: measured(summary.gateway_delay_p50_ms),
            gateway_delay_p95_ms: measured(summary.gateway_delay_p95_ms),
            gateway_delay_p99_ms: measured(summary.gateway_delay_p99_ms),
            gateway_delay_max_ms: measured(summary.gateway_delay_max_ms),
        },
        stage_averages: StageAverages {
            gateway_delay_ms: summary.avg_gateway_delay_ms,
            receive_ms: summary.avg_receive_ms,
            pre_ms: summary.avg_pre_ms,
            connect_ms: summary.avg_connect_ms,
            retry_ms: summary.avg_retry_ms,
            upstream_first_byte_ms: summary.avg_upstream_first_byte_ms,
            upstream_stream_ms: summary.avg_upstream_stream_ms,
            post_ms: summary.avg_post_ms,
            relay_max_ms: summary.avg_relay_max_ms,
            settle_ms: summary.avg_settle_ms,
            total_ms: summary.avg_total_ms,
        },
        series,
        slowest_requests,
    }))
}
