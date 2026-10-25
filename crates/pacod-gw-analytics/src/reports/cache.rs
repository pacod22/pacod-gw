//! What the response cache did (`docs/plugin-concept.md`, section 15): calls by result, the
//! share it answered, and what its answers saved, in total, over time, by model and by
//! workspace.
//!
//! Only events of workspaces with a response cache bound count (`cache` is not empty). The
//! rollups keep neither the result nor the saved cost, so the report reads the events.

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::EVENTS;
use super::params::{Bucket, Filters, Params, USAGE_FILTERS};
use crate::AppState;
use crate::error::ApiError;
use crate::storage::Binds;

const CACHED: &str = "cache != ''";

/// Largest `limit` of the lists by model and by workspace.
const MAX_TOP: u64 = 100;

/// What a hit saved, in the reporting currency where it was converted: as `TOTALS_SQL` adds up
/// costs. Not named after its column, which the expressions read (see `reports/mod.rs`).
const SAVED: &str = "ifNull(cache_saved_cost_reporting, ifNull(cache_saved_cost, 0))";

/// The counters of every row. `lookups` are the calls the cache looked up an answer for, as
/// the gateway's hit-rate metric counts them.
fn counts_sql() -> String {
    format!(
        "countIf(cache = 'hit') AS hits, \
         countIf(cache = 'miss') AS misses, \
         countIf(cache = 'would_hit') AS would_hits, \
         countIf(cache = 'refresh') AS refreshes, \
         countIf(cache = 'bypass') AS bypasses, \
         countIf(cache IN ('hit', 'miss', 'would_hit')) AS lookups, \
         sumIf({SAVED}, cache = 'hit') AS saved_cost"
    )
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Counts {
    hits: u64,
    misses: u64,
    would_hits: u64,
    refreshes: u64,
    bypasses: u64,
    lookups: u64,
    saved_cost: f64,
}

#[derive(Serialize)]
struct CacheTotals {
    #[serde(flatten)]
    counts: Counts,
    /// `hits / lookups`; `null` without lookups.
    hit_rate: Option<f64>,
    /// The saved cost per currency: one entry, the reporting currency, unless some hits had no
    /// exchange rate.
    currencies: Vec<CurrencyRow>,
}

#[derive(Serialize, Deserialize)]
struct CurrencyRow {
    /// Selected as `code`: an alias named `currency` would be read for the column.
    #[serde(rename(deserialize = "code"))]
    currency: String,
    saved_cost: f64,
}

#[derive(Serialize, Deserialize)]
struct TimeRow {
    bucket: DateTime<Utc>,
    #[serde(flatten)]
    counts: Counts,
}

#[derive(Serialize, Deserialize)]
struct ModelRow {
    provider: String,
    model: String,
    #[serde(flatten)]
    counts: Counts,
}

#[derive(Serialize, Deserialize)]
struct WorkspaceRow {
    organisation_id: String,
    workspace_id: String,
    #[serde(flatten)]
    counts: Counts,
}

#[derive(Serialize)]
pub struct CacheReport {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    bucket: &'static str,
    totals: CacheTotals,
    /// Every bucket that has events, in time order.
    series: Vec<TimeRow>,
    /// Most hits first, at most `limit`.
    top_models: Vec<ModelRow>,
    /// Most hits first, at most `limit`.
    top_workspaces: Vec<WorkspaceRow>,
}

fn hit_rate(c: &Counts) -> Option<f64> {
    (c.lookups > 0).then(|| c.hits as f64 / c.lookups as f64)
}

pub async fn report(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Response, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &USAGE_FILTERS)?.within(range).and(CACHED);
    let bucket = Bucket::take(&mut params, Bucket::Hour)?;
    let limit = params.limit(10, MAX_TOP)?;
    params.finish()?;

    let clickhouse = state.clickhouse();
    let counts = counts_sql();

    let mut total_binds = Binds::new();
    let total_sql = format!("SELECT {counts} FROM {EVENTS} {}", filters.where_sql(&mut total_binds));

    let mut currency_binds = Binds::new();
    let currency_sql = format!(
        "SELECT if(isNull(cache_saved_cost_reporting), toString(currency), toString(reporting_currency)) AS code, \
                sum({SAVED}) AS saved_cost \
         FROM {EVENTS} {filter} \
         GROUP BY code \
         ORDER BY saved_cost DESC, code ASC",
        filter = filters.clone().and("cache = 'hit'").where_sql(&mut currency_binds),
    );

    let mut time_binds = Binds::new();
    let time_sql = format!(
        "SELECT {bucket} AS bucket, {counts} FROM {EVENTS} {filter} GROUP BY bucket ORDER BY bucket",
        bucket = bucket.sql(),
        filter = filters.where_sql(&mut time_binds),
    );

    let mut model_binds = Binds::new();
    let model_sql = format!(
        "SELECT provider, toString(model_served) AS model, {counts} FROM {EVENTS} {filter} \
         GROUP BY provider, model \
         ORDER BY hits DESC, saved_cost DESC, provider ASC, model ASC \
         LIMIT {limit}",
        filter = filters.where_sql(&mut model_binds),
        limit = model_binds.uint64(limit),
    );

    let mut ws_binds = Binds::new();
    let ws_sql = format!(
        "SELECT organisation_id, workspace_id, {counts} FROM {EVENTS} {filter} \
         GROUP BY organisation_id, workspace_id \
         ORDER BY hits DESC, saved_cost DESC, workspace_id ASC \
         LIMIT {limit}",
        filter = filters.where_sql(&mut ws_binds),
        limit = ws_binds.uint64(limit),
    );

    let (totals, currencies, series, top_models, top_workspaces) = tokio::try_join!(
        clickhouse.fetch_one::<Counts>(&total_sql, &total_binds),
        clickhouse.fetch::<CurrencyRow>(&currency_sql, &currency_binds),
        clickhouse.fetch::<TimeRow>(&time_sql, &time_binds),
        clickhouse.fetch::<ModelRow>(&model_sql, &model_binds),
        clickhouse.fetch::<WorkspaceRow>(&ws_sql, &ws_binds),
    )?;

    Ok(Json(CacheReport {
        from: range.from,
        to: range.to,
        bucket: bucket.name(),
        totals: CacheTotals { hit_rate: hit_rate(&totals), counts: totals, currencies },
        series,
        top_models,
        top_workspaces,
    })
    .into_response())
}
