//! Usage-event retention (concept section 14.4): a daily run deletes the usage events, and the
//! rollups built from them, that are older than the retention of their workspace.
//!
//! The core hands out the work: the installation's retention, and every workspace whose own
//! (inherited) retention differs from it. The installation's retention also covers events of
//! workspaces that no longer exist. Cutoffs fall on midnight UTC, the granularity of the daily
//! rollups, so that a rollup never counts events that were deleted.

use std::collections::BTreeMap;

use chrono::{DateTime, Days, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::storage::rows::{DAILY_END_USERS_TABLE, HOURLY_TABLE, USAGE_TABLE};
use crate::storage::{Binds, ClickHouse};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Work {
    /// The installation's retention in days; `None` keeps events.
    global_days: Option<u32>,
    workspaces: Vec<WorkspaceRetention>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceRetention {
    workspace_id: String,
    /// `None` keeps the workspace's events, also when the installation sets a retention.
    days: Option<u32>,
}

#[derive(Deserialize)]
struct Count {
    n: u64,
}

/// The tables a retention deletes from, each with its time column. The rollups go by the same
/// midnight cutoff, so that a built hour or day never counts events that were deleted.
pub const RETAINED: &[(&str, &str)] =
    &[(USAGE_TABLE, "timestamp"), (HOURLY_TABLE, "bucket"), (DAILY_END_USERS_TABLE, "bucket")];

/// Midnight UTC `days` days before the day of `now`: what is older goes.
pub fn cutoff(now: DateTime<Utc>, days: u32) -> DateTime<Utc> {
    let midnight = now.date_naive().and_hms_opt(0, 0, 0).expect("midnight exists").and_utc();
    midnight.checked_sub_days(Days::new(u64::from(days))).unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Deletes what each retention says is expired. Returns, per retention, how many usage events
/// went.
pub async fn run(clickhouse: &ClickHouse, work: &Value, now: DateTime<Utc>) -> anyhow::Result<Value> {
    let work: Work = serde_json::from_value(work.clone())?;
    let mut by_days: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for w in &work.workspaces {
        if let Some(days) = w.days.filter(|d| *d > 0) {
            by_days.entry(days).or_default().push(w.workspace_id.clone());
        }
    }
    let mut report = Vec::new();
    // One deletion per distinct cutoff, not one per workspace.
    for (days, workspaces) in &by_days {
        let deleted = delete(clickhouse, Some(workspaces), &[], cutoff(now, *days)).await?;
        report.push(json!({ "days": days, "workspaces": workspaces.len(), "deleted": deleted }));
    }
    if let Some(days) = work.global_days.filter(|d| *d > 0) {
        let own: Vec<String> = work.workspaces.iter().map(|w| w.workspace_id.clone()).collect();
        let deleted = delete(clickhouse, None, &own, cutoff(now, days)).await?;
        report.push(json!({ "days": days, "scope": "installation", "deleted": deleted }));
    }
    Ok(json!({ "deletions": report }))
}

/// Deletes the rows before `before` of the workspaces in `only` (all when `None`), except those
/// in `except`.
async fn delete(
    clickhouse: &ClickHouse,
    only: Option<&[String]>,
    except: &[String],
    before: DateTime<Utc>,
) -> anyhow::Result<u64> {
    let mut deleted = 0;
    for (table, time) in RETAINED {
        let mut binds = Binds::new();
        let mut condition = format!("{time} < {}", binds.timestamp(before));
        if let Some(only) = only {
            condition.push_str(&format!(" AND has({}, workspace_id)", binds.strings(only)));
        }
        if !except.is_empty() {
            condition.push_str(&format!(" AND NOT has({}, workspace_id)", binds.strings(except)));
        }
        let count: Count =
            clickhouse.fetch_one(&format!("SELECT count() AS n FROM {table} WHERE {condition}"), &binds).await?;
        if count.n > 0 {
            clickhouse.command(&format!("DELETE FROM {table} WHERE {condition}"), &binds, &[]).await?;
        }
        if *table == USAGE_TABLE {
            deleted = count.n;
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoffs_fall_on_midnight_utc() {
        let now: DateTime<Utc> = "2026-10-10T17:45:00Z".parse().unwrap();
        assert_eq!(cutoff(now, 30), "2026-09-10T00:00:00Z".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(cutoff(now, 1), "2026-10-09T00:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }
}
