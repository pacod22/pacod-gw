//! Failures by type, by provider and model, over time, and the most recent ones.
//!
//! A failure is an event whose status is not `ok` (concept section 14.4).

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use pacod_gw_events::UsageEvent;
use serde::{Deserialize, Serialize};

use super::params::{Bucket, Filters, Format, Params, USAGE_FILTERS};
use super::{EVENTS, MAX_LIMIT, NEWEST_FIRST, csv, events};
use crate::AppState;
use crate::error::ApiError;
use crate::storage::Binds;

const IS_FAILURE: &str = "status != 'ok'";

#[derive(Deserialize)]
struct Counts {
    requests: u64,
    errors: u64,
}

#[derive(Serialize)]
struct ErrorTotals {
    requests: u64,
    errors: u64,
    /// `errors / requests`, zero when there are no requests.
    error_rate: f64,
}

#[derive(Serialize, Deserialize)]
struct TypeRow {
    /// Empty when the event carries no error type.
    error_type: String,
    errors: u64,
    last_seen_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct ProviderModelCounts {
    provider: String,
    model: String,
    requests: u64,
    errors: u64,
}

#[derive(Serialize, Default)]
struct ProviderModelRow {
    provider: String,
    model: String,
    requests: u64,
    errors: u64,
    error_rate: f64,
}

#[derive(Serialize, Deserialize)]
struct TimeRow {
    bucket: DateTime<Utc>,
    requests: u64,
    errors: u64,
}

#[derive(Serialize)]
pub struct Errors {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    bucket: &'static str,
    totals: ErrorTotals,
    /// Most frequent first.
    by_type: Vec<TypeRow>,
    /// Provider and model pairs with at least one failure, most failures first.
    by_provider_model: Vec<ProviderModelRow>,
    /// Every bucket that has events, in time order.
    over_time: Vec<TimeRow>,
    /// Newest first, at most `limit`.
    recent_requests: Vec<UsageEvent>,
}

fn rate(errors: u64, requests: u64) -> f64 {
    if requests == 0 { 0.0 } else { errors as f64 / requests as f64 }
}

pub async fn report(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Response, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &USAGE_FILTERS)?.within(range);
    let bucket = Bucket::take(&mut params, Bucket::Hour)?;
    let limit = params.limit(20, MAX_LIMIT)?;
    let format = params.format()?;
    params.finish()?;

    let failures = filters.clone().and(IS_FAILURE);
    let clickhouse = state.clickhouse();

    let mut counts_binds = Binds::new();
    let counts_sql = format!(
        "SELECT count() AS requests, countIf({IS_FAILURE}) AS errors FROM {EVENTS} {filter}",
        filter = filters.where_sql(&mut counts_binds),
    );

    let mut type_binds = Binds::new();
    let type_sql = format!(
        "SELECT error_type, count() AS errors, max(timestamp) AS last_seen_at \
         FROM {EVENTS} {filter} \
         GROUP BY error_type \
         ORDER BY errors DESC, error_type ASC",
        filter = failures.where_sql(&mut type_binds),
    );

    let mut pair_binds = Binds::new();
    let pair_sql = format!(
        "SELECT provider, toString(model_served) AS model, \
                count() AS requests, countIf({IS_FAILURE}) AS errors \
         FROM {EVENTS} {filter} \
         GROUP BY provider, model \
         HAVING errors > 0 \
         ORDER BY errors DESC, provider ASC, model ASC",
        filter = filters.where_sql(&mut pair_binds),
    );

    let mut time_binds = Binds::new();
    let time_sql = format!(
        "SELECT {bucket} AS bucket, count() AS requests, countIf({IS_FAILURE}) AS errors \
         FROM {EVENTS} {filter} \
         GROUP BY bucket \
         ORDER BY bucket",
        bucket = bucket.sql(),
        filter = filters.where_sql(&mut time_binds),
    );

    let (counts, by_type, pairs, over_time, recent_requests) = tokio::try_join!(
        clickhouse.fetch_one::<Counts>(&counts_sql, &counts_binds),
        clickhouse.fetch::<TypeRow>(&type_sql, &type_binds),
        clickhouse.fetch::<ProviderModelCounts>(&pair_sql, &pair_binds),
        clickhouse.fetch::<TimeRow>(&time_sql, &time_binds),
        events(clickhouse, &failures, NEWEST_FIRST, limit),
    )?;

    let by_provider_model: Vec<ProviderModelRow> = pairs
        .into_iter()
        .map(|row| ProviderModelRow {
            error_rate: rate(row.errors, row.requests),
            provider: row.provider,
            model: row.model,
            requests: row.requests,
            errors: row.errors,
        })
        .collect();
    if format == Format::Csv {
        // The table of provider and model pairs; the other parts are in the JSON answer.
        return csv::download("errors", &ProviderModelRow::default(), &by_provider_model);
    }
    Ok(Json(Errors {
        from: range.from,
        to: range.to,
        bucket: bucket.name(),
        totals: ErrorTotals {
            requests: counts.requests,
            errors: counts.errors,
            error_rate: rate(counts.errors, counts.requests),
        },
        by_type,
        by_provider_model,
        over_time,
        recent_requests,
    })
    .into_response())
}
