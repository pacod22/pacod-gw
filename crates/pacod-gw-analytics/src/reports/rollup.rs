//! Reading the rollups (concept section 14.4).
//!
//! A report that the rollups can answer reads, instead of the events, a source that is the
//! union of two parts: the rollup rows of the buckets that were built and are not marked
//! late, and the events of everything else in its range (partial hours or days at the edges,
//! the current hour or day, buckets not built yet or marked late). Both parts have the same
//! columns, and the report adds them up. The answer is the one the events alone give.
//!
//! Only what the rollups keep can be read from them: the filters and groupings of their
//! columns. A report with any other filter reads the events, as it does when its range holds
//! no built bucket.

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use super::params::Filters;
use crate::jobs::rollups::{day_of, hour_of};
use crate::storage::rows::{DAILY_END_USERS_TABLE, HOURLY_TABLE, USAGE_TABLE};
use crate::storage::{Binds, ClickHouse, ClickHouseError};

/// The filters the hourly rollup can answer: event column, rollup column.
pub const HOURLY_COLUMNS: &[(&str, &str)] = &[
    ("organisation_id", "organisation_id"),
    ("workspace_id", "workspace_id"),
    ("provider", "provider"),
    ("model_served", "model"),
    ("template", "template"),
    ("key_id", "key_id"),
    ("status", "status"),
];

/// The filters the daily end-user rollup can answer.
pub const END_USER_COLUMNS: &[(&str, &str)] =
    &[("organisation_id", "organisation_id"), ("workspace_id", "workspace_id")];

/// The fixed condition of the end-user reports, which the end-user rollup meets by itself.
pub const HAS_END_USER: &str = "end_user != ''";

/// What a source adds up, under the names of `TOTALS_SQL`.
pub const SOURCE_TOTALS: &str = "sum(requests) AS requests, sum(errors) AS errors, \
     sum(input_tokens) AS input_tokens, sum(output_tokens) AS output_tokens, \
     sum(cache_read_tokens) AS cache_read_tokens, sum(cache_write_tokens) AS cache_write_tokens, \
     sum(reasoning_tokens) AS reasoning_tokens, sum(cost) AS cost";

/// The columns every event contributes, in the order of the rollup tables.
const EVENT_SUMS: &str = "toUInt64(1), toUInt64(status != 'ok'), input_tokens, output_tokens, \
     cache_read_tokens, cache_write_tokens, reasoning_tokens, ifNull(cost_reporting, ifNull(cost, 0))";

const ROLLUP_SUMS: &str = "requests, errors, input_tokens, output_tokens, cache_read_tokens, \
     cache_write_tokens, reasoning_tokens, cost";

#[derive(Deserialize)]
struct Count {
    n: u64,
}

/// The whole buckets of `[from, to)` that are closed at `now`, as `[first, end)`, with `size`
/// and `start_of` giving the bucket. `None` when there is none.
fn span(
    filters: &Filters,
    now: DateTime<Utc>,
    size: Duration,
    start_of: fn(DateTime<Utc>) -> DateTime<Utc>,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let (Some(from), Some(to)) = filters.bounds() else { return None };
    let first = if start_of(from) == from { from } else { start_of(from) + size };
    let end = start_of(to).min(start_of(now));
    (first < end).then_some((first, end))
}

/// A source for a report on `filters` that reads the hourly rollup where it can. Its columns:
/// `t`, `organisation_id`, `workspace_id`, `key_id`, `provider`, `model`, `template`, `status`
/// and the counters of [`SOURCE_TOTALS`]. `None` when the report must read the events: a
/// filter the rollup does not keep, or no built hour in the range.
pub async fn hourly(
    clickhouse: &ClickHouse,
    filters: &Filters,
    binds: &mut Binds,
    now: DateTime<Utc>,
) -> Result<Option<String>, ClickHouseError> {
    let Some((first, end)) = span(filters, now, Duration::hours(1), hour_of) else { return Ok(None) };
    let mut probe = Binds::new();
    let Some(conditions) = filters.rollup_conditions(HOURLY_COLUMNS, &[], &mut probe) else { return Ok(None) };
    let built = |binds: &mut Binds| {
        format!(
            "(SELECT bucket FROM rollup_built FINAL WHERE granularity = 'hour' AND bucket >= {f} AND bucket < {e} \
               AND bucket NOT IN (SELECT bucket FROM rollup_late))",
            f = binds.timestamp(first),
            e = binds.timestamp(end),
        )
    };
    let count_sql = format!("SELECT count() AS n FROM {}", built(&mut probe));
    if clickhouse.fetch_one::<Count>(&count_sql, &probe).await?.n == 0 {
        return Ok(None);
    }
    let rollup_conditions = filters.rollup_conditions(HOURLY_COLUMNS, &[], binds).unwrap_or_default();
    let rolled = format!(
        "SELECT bucket AS t, organisation_id, workspace_id, key_id, provider, model, template, status, {ROLLUP_SUMS} \
         FROM {HOURLY_TABLE} FINAL WHERE bucket IN {built}{and}{rollup}",
        built = built(binds),
        and = if conditions.is_empty() { "" } else { " AND " },
        rollup = rollup_conditions.join(" AND "),
    );
    let events = format!(
        "SELECT timestamp, organisation_id, workspace_id, key_id, provider, toString(model_served), template, status, \
                {EVENT_SUMS} \
         FROM {USAGE_TABLE} FINAL WHERE {raw} AND toStartOfHour(timestamp) NOT IN {built}",
        raw = filters.condition_sql(binds),
        built = built(binds),
    );
    Ok(Some(format!("{rolled} UNION ALL {events}")))
}

/// A source for the end-user reports that reads the daily end-user rollup where it can. Its
/// columns: `t`, `organisation_id`, `workspace_id`, `end_user`, the counters of
/// [`SOURCE_TOTALS`], and `first_at` and `last_at`. `None` as for [`hourly`].
pub async fn daily_end_users(
    clickhouse: &ClickHouse,
    filters: &Filters,
    binds: &mut Binds,
    now: DateTime<Utc>,
) -> Result<Option<String>, ClickHouseError> {
    let Some((first, end)) = span(filters, now, Duration::days(1), day_of) else { return Ok(None) };
    let mut probe = Binds::new();
    let Some(conditions) = filters.rollup_conditions(END_USER_COLUMNS, &[HAS_END_USER], &mut probe) else {
        return Ok(None);
    };
    // A day is late when any of its hours is.
    let built = |binds: &mut Binds| {
        format!(
            "(SELECT bucket FROM rollup_built FINAL WHERE granularity = 'day' AND bucket >= {f} AND bucket < {e} \
               AND bucket NOT IN (SELECT toStartOfDay(bucket) FROM rollup_late))",
            f = binds.timestamp(first),
            e = binds.timestamp(end),
        )
    };
    let count_sql = format!("SELECT count() AS n FROM {}", built(&mut probe));
    if clickhouse.fetch_one::<Count>(&count_sql, &probe).await?.n == 0 {
        return Ok(None);
    }
    let rollup_conditions = filters.rollup_conditions(END_USER_COLUMNS, &[HAS_END_USER], binds).unwrap_or_default();
    let rolled = format!(
        "SELECT bucket AS t, organisation_id, workspace_id, end_user, {ROLLUP_SUMS}, first_at, last_at \
         FROM {DAILY_END_USERS_TABLE} FINAL WHERE bucket IN {built}{and}{rollup}",
        built = built(binds),
        and = if conditions.is_empty() { "" } else { " AND " },
        rollup = rollup_conditions.join(" AND "),
    );
    let events = format!(
        "SELECT timestamp, organisation_id, workspace_id, end_user, {EVENT_SUMS}, timestamp, timestamp \
         FROM {USAGE_TABLE} FINAL WHERE {raw} AND toStartOfDay(timestamp) NOT IN {built}",
        raw = filters.condition_sql(binds),
        built = built(binds),
    );
    Ok(Some(format!("{rolled} UNION ALL {events}")))
}
