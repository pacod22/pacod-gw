//! `DELETE /erasure`: everything the service holds about one of a workspace's users (identity
//! concept 6.9), by the `user_id` or the `end_user` the caller sent with its calls: their usage
//! events, their audit records (with the attachments no other record needs) and their end-user
//! rollups. The product that knows the person asks, through the core.

use axum::Json;
use axum::extract::{RawQuery, State};
use serde::{Deserialize, Serialize};

use super::params::{Filters, Params};
use crate::AppState;
use crate::error::ApiError;
use crate::storage::rows::{DAILY_END_USERS_TABLE, USAGE_TABLE};
use crate::storage::{Binds, ClickHouse};

#[derive(Serialize)]
pub struct Erasure {
    /// How many usage events matched and were erased.
    usage_events: u64,
    /// How many audit records matched and were erased.
    audit_records: u64,
    /// How many daily end-user rollup rows were erased (by `end_user` only: rollups carry no
    /// `user_id`).
    end_user_rollups: u64,
}

#[derive(Deserialize)]
struct Count {
    rows: u64,
}

/// Erases one user's data in one workspace. `organisation_id`, `workspace_id` and exactly one
/// of `user_id` and `end_user` are required, none of them empty. Returns after ClickHouse has
/// rewritten the data.
pub async fn erase(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Json<Erasure>, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let organisation_id = params.one("organisation_id")?;
    let workspace_id = params.one("workspace_id")?;
    let user_id = params.one("user_id")?;
    let end_user = params.one("end_user")?;
    params.finish()?;

    let (Some(organisation_id), Some(workspace_id)) = (organisation_id, workspace_id) else {
        return Err(ApiError::bad_request(
            "missing_parameter",
            "give organisation_id and workspace_id: an erasure is for one workspace",
        ));
    };
    for (name, value) in [("organisation_id", &organisation_id), ("workspace_id", &workspace_id)] {
        if value.is_empty() {
            return Err(ApiError::invalid_parameter(name, "must not be empty"));
        }
    }
    let (column, value) = match (user_id, end_user) {
        (Some(v), None) => ("user_id", v),
        (None, Some(v)) => ("end_user", v),
        _ => {
            return Err(ApiError::bad_request(
                "missing_parameter",
                "give user_id or end_user, one of them: an erasure must name whose data to erase",
            ));
        }
    };
    if value.is_empty() {
        return Err(ApiError::invalid_parameter(column, "must not be empty"));
    }

    let clickhouse = state.clickhouse();
    let filters = Filters::default()
        .equal("organisation_id", organisation_id)
        .equal("workspace_id", workspace_id)
        .equal(column, value);
    let usage_events = delete_matching(clickhouse, USAGE_TABLE, &filters).await?;
    let audit_records = super::audit::erase_matching(clickhouse, &filters).await?;
    let end_user_rollups =
        if column == "end_user" { delete_matching(clickhouse, DAILY_END_USERS_TABLE, &filters).await? } else { 0 };
    tracing::info!(by = column, usage_events, audit_records, end_user_rollups, "a user's data erased");
    Ok(Json(Erasure { usage_events, audit_records, end_user_rollups }))
}

/// Deletes the rows of `table` that `filters` match and returns how many there were, after the
/// mutation has finished on every replica.
async fn delete_matching(clickhouse: &ClickHouse, table: &str, filters: &Filters) -> Result<u64, ApiError> {
    let mut binds = Binds::new();
    let sql = format!("SELECT count() AS rows FROM {table} FINAL {}", filters.where_sql(&mut binds));
    let count: Count = clickhouse.fetch_one(&sql, &binds).await?;
    if count.rows > 0 {
        let mut binds = Binds::new();
        let sql = format!("ALTER TABLE {table} DELETE WHERE {}", filters.condition_sql(&mut binds));
        clickhouse.command(&sql, &binds, &[("mutations_sync", "2")]).await?;
    }
    Ok(count.rows)
}
