//! The rollup job (concept section 14.4): rebuilds the hourly and the daily end-user rollups
//! from the deduplicated events.
//!
//! Each run rebuilds the last two closed hours, every hour ingest marked late, and hours that
//! were never built: those of the last two days, and older ones a week at a time, walking back
//! from a frontier kept in `rollup_progress` until it passes the oldest event. Then the closed
//! days those hours belong to. A
//! rebuild writes the bucket's rows under a new generation, deletes the older generations,
//! and records the bucket as built. Late marks set before the run started are cleared; one
//! set while the run worked stays for the next run.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, DurationRound, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::storage::rows::{DAILY_END_USERS_TABLE, HOURLY_TABLE, USAGE_TABLE};
use crate::storage::{Binds, ClickHouse, ClickHouseError};

/// Hours never built that one run takes on, newest first.
const BACKFILL_HOURS: u64 = 48;
/// The recent past in which every run looks for hours never built.
const RECENT: Duration = Duration::days(2);
/// How much older history one step of the backfill looks at.
const BACKFILL_WINDOW: Duration = Duration::days(7);
/// Steps of the backfill in one run.
const BACKFILL_STEPS: usize = 10;
/// Late marks one run takes on.
const LATE_PER_RUN: u64 = 1000;

/// What the reports count, in the order of the rollup tables.
const SUMS: &str = "count() AS requests, countIf(status != 'ok') AS errors, \
     sum(input_tokens) AS input_tokens, sum(output_tokens) AS output_tokens, \
     sum(cache_read_tokens) AS cache_read_tokens, sum(cache_write_tokens) AS cache_write_tokens, \
     sum(reasoning_tokens) AS reasoning_tokens, sum(ifNull(cost_reporting, ifNull(cost, 0))) AS cost";

#[derive(Deserialize)]
struct Bucket {
    bucket: DateTime<Utc>,
}

pub fn hour_of(t: DateTime<Utc>) -> DateTime<Utc> {
    t.duration_trunc(Duration::hours(1)).unwrap_or(t)
}

pub fn day_of(t: DateTime<Utc>) -> DateTime<Utc> {
    t.duration_trunc(Duration::days(1)).unwrap_or(t)
}

async fn buckets(clickhouse: &ClickHouse, sql: &str, binds: &Binds) -> Result<Vec<DateTime<Utc>>, ClickHouseError> {
    Ok(clickhouse.fetch::<Bucket>(sql, binds).await?.into_iter().map(|b| b.bucket).collect())
}

/// One run at `now`. Returns how many hours and days were rebuilt.
pub async fn run(clickhouse: &ClickHouse, now: DateTime<Utc>) -> anyhow::Result<Value> {
    let current_hour = hour_of(now);
    let today = day_of(now);
    let mut hours: BTreeSet<DateTime<Utc>> =
        [current_hour - Duration::hours(1), current_hour - Duration::hours(2)].into();

    let mut binds = Binds::new();
    let late_sql = format!(
        "SELECT DISTINCT bucket FROM rollup_late WHERE marked_at <= {now} AND bucket < {hour} ORDER BY bucket LIMIT {n}",
        now = binds.timestamp(now),
        hour = binds.timestamp(current_hour),
        n = binds.uint64(LATE_PER_RUN),
    );
    let late = buckets(clickhouse, &late_sql, &binds).await?;
    hours.extend(late.iter().copied());

    let recent_from = current_hour - RECENT;
    hours.extend(unbuilt(clickhouse, recent_from, current_hour).await?);
    hours.extend(backfill(clickhouse, recent_from).await?);

    for hour in &hours {
        rebuild_hour(clickhouse, *hour).await?;
    }

    // The closed days of those hours.
    let days: BTreeSet<DateTime<Utc>> = hours.iter().map(|h| day_of(*h)).filter(|d| *d < today).collect();
    for day in &days {
        rebuild_day(clickhouse, *day).await?;
    }

    // The marks this run has seen are done; one set since stays.
    if !late.is_empty() {
        let mut binds = Binds::new();
        let late: Vec<String> = late.iter().map(|h| h.format("%Y-%m-%d %H:%M:%S").to_string()).collect();
        let sql = format!(
            "DELETE FROM rollup_late WHERE marked_at <= {now} AND has(arrayMap(x -> toDateTime(x, 'UTC'), {late}), bucket)",
            now = binds.timestamp(now),
            late = binds.strings(&late),
        );
        clickhouse.command(&sql, &binds, &[]).await?;
    }
    Ok(json!({ "hours": hours.len(), "days": days.len() }))
}

/// The hours of `[from, to)` that have events and were never built, newest first.
async fn unbuilt(
    clickhouse: &ClickHouse,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<DateTime<Utc>>, ClickHouseError> {
    let mut binds = Binds::new();
    let sql = format!(
        "SELECT DISTINCT toStartOfHour(timestamp) AS bucket FROM {USAGE_TABLE} \
         WHERE timestamp >= {from} AND timestamp < {to} \
           AND toStartOfHour(timestamp) NOT IN (SELECT bucket FROM rollup_built FINAL WHERE granularity = 'hour') \
         ORDER BY bucket DESC LIMIT {n}",
        from = binds.timestamp(from),
        to = binds.timestamp(to),
        n = binds.uint64(BACKFILL_HOURS),
    );
    buckets(clickhouse, &sql, &binds).await
}

#[derive(Deserialize)]
struct Newest {
    newest: Option<DateTime<Utc>>,
}

/// Older hours never built, at most a run's worth: walks back from the frontier, a week at a
/// time, skipping empty stretches, and moves the frontier past each week it has covered.
async fn backfill(clickhouse: &ClickHouse, recent_from: DateTime<Utc>) -> Result<Vec<DateTime<Utc>>, ClickHouseError> {
    let stored = buckets(
        clickhouse,
        "SELECT value AS bucket FROM rollup_progress FINAL WHERE name = 'backfill_before'",
        &Binds::new(),
    )
    .await?;
    let mut frontier = stored.first().copied().unwrap_or(recent_from).min(recent_from);
    let mut found = Vec::new();
    for _ in 0..BACKFILL_STEPS {
        // The newest event before the frontier; none means the walk is done.
        let mut binds = Binds::new();
        let sql = format!(
            "SELECT if(count() = 0, NULL, max(timestamp)) AS newest FROM {USAGE_TABLE} WHERE timestamp < {f}",
            f = binds.timestamp(frontier),
        );
        let Some(newest) = clickhouse.fetch_one::<Newest>(&sql, &binds).await?.newest else { break };
        let end = hour_of(newest) + Duration::hours(1);
        let start = end - BACKFILL_WINDOW;
        let hours = unbuilt(clickhouse, start, end).await?;
        let complete = (hours.len() as u64) < BACKFILL_HOURS;
        found.extend(hours);
        if !complete {
            break;
        }
        frontier = start;
        if found.len() as u64 >= BACKFILL_HOURS {
            break;
        }
    }
    if Some(frontier) != stored.first().copied() {
        let mut binds = Binds::new();
        let sql = format!(
            "INSERT INTO rollup_progress (name, value, updated) VALUES ('backfill_before', toDateTime({f}, 'UTC'), now64(3))",
            f = binds.int64(frontier.timestamp()),
        );
        clickhouse.command(&sql, &binds, &[]).await?;
    }
    Ok(found)
}

fn generation() -> u64 {
    Utc::now().timestamp_nanos_opt().unwrap_or_default().max(0) as u64
}

/// Writes one hour's rollup rows anew and records the hour as built.
pub async fn rebuild_hour(clickhouse: &ClickHouse, hour: DateTime<Utc>) -> Result<(), ClickHouseError> {
    let g = generation();
    let mut binds = Binds::new();
    let (from, to) = (binds.timestamp(hour), binds.timestamp(hour + Duration::hours(1)));
    let gen_ = binds.uint64(g);
    let insert = format!(
        "INSERT INTO {HOURLY_TABLE} (bucket, organisation_id, workspace_id, key_id, provider, model, template, status, \
             requests, errors, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, cost, generation) \
         SELECT toStartOfHour(timestamp) AS bucket, organisation_id, workspace_id, key_id, provider, \
                toString(model_served) AS model, template, status, {SUMS}, {gen_} AS generation \
         FROM {USAGE_TABLE} FINAL WHERE timestamp >= {from} AND timestamp < {to} \
         GROUP BY bucket, organisation_id, workspace_id, key_id, provider, model, template, status"
    );
    clickhouse.command(&insert, &binds, &[]).await?;
    replace_older(clickhouse, HOURLY_TABLE, "hour", hour, g).await
}

/// Writes one day's end-user rollup rows anew and records the day as built.
pub async fn rebuild_day(clickhouse: &ClickHouse, day: DateTime<Utc>) -> Result<(), ClickHouseError> {
    let g = generation();
    let mut binds = Binds::new();
    let (from, to) = (binds.timestamp(day), binds.timestamp(day + Duration::days(1)));
    let gen_ = binds.uint64(g);
    let insert = format!(
        "INSERT INTO {DAILY_END_USERS_TABLE} (bucket, organisation_id, workspace_id, end_user, requests, errors, \
             input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, cost, first_at, last_at, generation) \
         SELECT toStartOfDay(timestamp) AS bucket, organisation_id, workspace_id, end_user, {SUMS}, \
                min(timestamp) AS first_at, max(timestamp) AS last_at, {gen_} AS generation \
         FROM {USAGE_TABLE} FINAL WHERE timestamp >= {from} AND timestamp < {to} AND end_user != '' \
         GROUP BY bucket, organisation_id, workspace_id, end_user"
    );
    clickhouse.command(&insert, &binds, &[]).await?;
    replace_older(clickhouse, DAILY_END_USERS_TABLE, "day", day, g).await
}

/// Deletes the bucket's rows of older generations and records the bucket as built.
async fn replace_older(
    clickhouse: &ClickHouse,
    table: &str,
    granularity: &str,
    bucket: DateTime<Utc>,
    generation: u64,
) -> Result<(), ClickHouseError> {
    let mut binds = Binds::new();
    let sql = format!(
        "DELETE FROM {table} WHERE bucket = toDateTime({b}, 'UTC') AND generation < {g}",
        b = binds.int64(bucket.timestamp()),
        g = binds.uint64(generation),
    );
    clickhouse.command(&sql, &binds, &[]).await?;
    let mut binds = Binds::new();
    let sql = format!(
        "INSERT INTO rollup_built (granularity, bucket, generation) VALUES ({k}, toDateTime({b}, 'UTC'), {g})",
        k = binds.string(granularity),
        b = binds.int64(bucket.timestamp()),
        g = binds.uint64(generation),
    );
    clickhouse.command(&sql, &binds, &[]).await
}

/// Marks the closed hours of a stored batch as late, so that the job rebuilds them. `now`
/// decides which hours are closed.
pub async fn mark_late(
    clickhouse: &ClickHouse,
    hours: &BTreeSet<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(), ClickHouseError> {
    let closed: Vec<i64> = hours.iter().filter(|h| **h < hour_of(now)).map(|h| h.timestamp()).collect();
    if closed.is_empty() {
        return Ok(());
    }
    let mut binds = Binds::new();
    let marked_at = binds.timestamp(now);
    let rows: Vec<String> =
        closed.iter().map(|t| format!("(toDateTime({}, 'UTC'), {marked_at})", binds.int64(*t))).collect();
    let sql = format!("INSERT INTO rollup_late (bucket, marked_at) VALUES {}", rows.join(", "));
    clickhouse.command(&sql, &binds, &[]).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_whole_hours_and_days_in_utc() {
        let t: DateTime<Utc> = "2026-10-10T17:45:12Z".parse().unwrap();
        assert_eq!(hour_of(t), "2026-10-10T17:00:00Z".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(day_of(t), "2026-10-10T00:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }
}
