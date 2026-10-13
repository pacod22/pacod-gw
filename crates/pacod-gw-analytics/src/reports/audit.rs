//! Audit records (concept section 14.6): the full record of one call, the list without bodies,
//! and erasure for one end user or one session.
//!
//! A record is served until `expires_at`. ClickHouse removes expired rows in the background;
//! the condition below hides them in the meantime.

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use pacod_gw_events::AuditRecord;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::params::{AUDIT_FILTERS, AUDIT_SCOPE, Cursor, Filters, Params};
use super::{AUDITS, MAX_LIMIT, Segment, segment};
use crate::AppState;
use crate::error::ApiError;
use crate::storage::Binds;
use crate::storage::rows::{ATTACHMENT_TABLE, AUDIT_TABLE, AuditRow};
use crate::storage::{ClickHouse, ClickHouseError};

const NOT_EXPIRED: &str = "expires_at > now64(9, 'UTC')";

// ---------------------------------------------------------------------------------------------
// GET /reports/audit/{audit_id}

/// The complete record. Restricted by scope only.
pub async fn one(
    State(state): State<AppState>,
    audit_id: Segment,
    RawQuery(query): RawQuery,
) -> Result<Json<AuditRecord>, ApiError> {
    let audit_id = segment(audit_id)?;
    let mut params = Params::parse(query.as_deref());
    let filters = Filters::take(&mut params, &AUDIT_SCOPE)?.equal("audit_id", audit_id).and(NOT_EXPIRED);
    params.finish()?;

    let mut binds = Binds::new();
    let sql = format!("SELECT * FROM {AUDITS} {filter} LIMIT 1", filter = filters.where_sql(&mut binds),);
    let mut found: Vec<AuditRow> = state.clickhouse().fetch(&sql, &binds).await?;
    let mut row = found.pop().ok_or_else(|| ApiError::not_found("audit record"))?;
    if !row.attachments.is_empty() {
        let contents = attachments_of(state.clickhouse(), &row.workspace_id, &row.attachments).await?;
        row.restore(&contents);
    }
    let record = row.into_record().map_err(|error| {
        // The error can quote stored content; only its position is logged.
        tracing::error!(line = error.line(), column = error.column(), "a stored audit record is not valid JSON");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", "the stored audit record could not be read")
    })?;
    Ok(Json(record))
}

#[derive(Deserialize)]
struct AttachmentContent {
    hash: String,
    content: String,
}

/// The contents of a workspace's attachments, by hash.
async fn attachments_of(
    clickhouse: &ClickHouse,
    workspace_id: &str,
    hashes: &[String],
) -> Result<std::collections::HashMap<String, String>, ClickHouseError> {
    let mut binds = Binds::new();
    let sql = format!(
        "SELECT hash, content FROM {ATTACHMENT_TABLE} FINAL WHERE workspace_id = {ws} AND has({hashes}, hash)",
        ws = binds.string(workspace_id),
        hashes = binds.strings(hashes),
    );
    let rows: Vec<AttachmentContent> = clickhouse.fetch(&sql, &binds).await?;
    Ok(rows.into_iter().map(|r| (r.hash, r.content)).collect())
}

#[derive(Deserialize)]
struct Referenced {
    workspace_id: String,
    hash: String,
}

/// The attachments the records matching `condition` refer to, by workspace.
async fn referenced(
    clickhouse: &ClickHouse,
    condition: &str,
    binds: &Binds,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, ClickHouseError> {
    let sql = format!(
        "SELECT DISTINCT toString(workspace_id) AS workspace_id, arrayJoin(attachments) AS hash \
         FROM {AUDITS} WHERE {condition}"
    );
    let rows: Vec<Referenced> = clickhouse.fetch(&sql, binds).await?;
    let mut out: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for r in rows {
        out.entry(r.workspace_id).or_default().push(r.hash);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// GET /reports/audit

/// An audit record without its bodies.
#[derive(Serialize, Deserialize)]
struct AuditMeta {
    audit_id: String,
    event_id: Uuid,
    timestamp: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    organisation_id: String,
    workspace_id: String,
    key_id: String,
    end_user: String,
    session_id: String,
    trace_id: String,
    agent_id: String,
    user_id: String,
    task: String,
    complexity: String,
    template: String,
    model: String,
    mode: String,
    truncated: bool,
}

const META_COLUMNS: &str = "audit_id, event_id, timestamp, expires_at, organisation_id, \
     workspace_id, key_id, end_user, session_id, trace_id, agent_id, user_id, task, complexity, \
     template, model, mode, truncated";

#[derive(Serialize)]
pub struct AuditPage {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    limit: u64,
    records: Vec<AuditMeta>,
    /// Pass as `cursor` to get the next page; `null` on the last page.
    next_cursor: Option<String>,
}

pub async fn list(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Json<AuditPage>, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let filters = Filters::take(&mut params, &AUDIT_FILTERS)?.within(range).and(NOT_EXPIRED);
    let limit = params.limit(50, MAX_LIMIT)?;
    let cursor = Cursor::take(&mut params)?;
    params.finish()?;

    let mut binds = Binds::new();
    let after_cursor = cursor.map(|cursor| cursor.before_string("audit_id", &mut binds));
    // One row more than asked for tells whether another page follows.
    let sql = format!(
        "SELECT {META_COLUMNS} FROM {AUDITS} {filter} \
         ORDER BY timestamp DESC, audit_id DESC LIMIT {limit}",
        filter = filters.where_with(&mut binds, after_cursor),
        limit = binds.uint64(limit + 1),
    );
    let mut records: Vec<AuditMeta> = state.clickhouse().fetch(&sql, &binds).await?;

    let has_more = records.len() as u64 > limit;
    records.truncate(limit as usize);
    let next_cursor = match (has_more, records.last()) {
        (true, Some(last)) => Some(Cursor::encode(last.timestamp, &last.audit_id)),
        _ => None,
    };

    Ok(Json(AuditPage { from: range.from, to: range.to, limit, records, next_cursor }))
}

// ---------------------------------------------------------------------------------------------
// DELETE /audit

#[derive(Deserialize)]
struct Count {
    records: u64,
}

#[derive(Serialize)]
pub struct Erased {
    /// How many records matched and were erased.
    deleted: u64,
}

/// Erases the audit records of one end user or one session, within the scope when one is given.
/// At least one of `end_user` and `session_id` is required. Returns after ClickHouse has
/// rewritten the data, so the records are gone from disk and not only hidden.
pub async fn erase(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Json<Erased>, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let mut filters = Filters::take(&mut params, &AUDIT_SCOPE)?;
    let end_user = params.one("end_user")?;
    let session_id = params.one("session_id")?;
    params.finish()?;

    if end_user.is_none() && session_id.is_none() {
        return Err(ApiError::bad_request(
            "missing_parameter",
            "give end_user or session_id: an erasure must name whose records to erase",
        ));
    }
    for (name, column, value) in [("end_user", "end_user", end_user), ("session_id", "session_id", session_id)] {
        match value {
            Some(value) if value.is_empty() => {
                return Err(ApiError::invalid_parameter(name, "must not be empty"));
            }
            Some(value) => filters = filters.equal(column, value),
            None => {}
        }
    }

    let deleted = erase_matching(state.clickhouse(), &filters).await?;
    tracing::info!(deleted, "audit records erased");

    Ok(Json(Erased { deleted }))
}

/// Erases the audit records `filters` match, and then the attachments of their workspaces that
/// no remaining record refers to. Returns how many records matched, after ClickHouse rewrote
/// the data.
pub(super) async fn erase_matching(clickhouse: &ClickHouse, filters: &Filters) -> Result<u64, ApiError> {
    let mut count_binds = Binds::new();
    let count_sql =
        format!("SELECT count() AS records FROM {AUDITS} {filter}", filter = filters.where_sql(&mut count_binds),);
    let count: Count = clickhouse.fetch_one(&count_sql, &count_binds).await?;

    if count.records > 0 {
        let mut binds = Binds::new();
        let condition = filters.condition_sql(&mut binds);
        // The attachments of the doomed records, before they go.
        let doomed = referenced(clickhouse, &condition, &binds).await?;
        let sql = format!("ALTER TABLE {AUDIT_TABLE} DELETE WHERE {condition}");
        // Wait until the mutation has finished on every replica.
        clickhouse.command(&sql, &binds, &[("mutations_sync", "2")]).await?;
        // Then those no remaining record of the workspace refers to.
        for (workspace_id, hashes) in doomed {
            let mut binds = Binds::new();
            let condition = format!(
                "workspace_id = {ws} AND hasAny(attachments, {hashes})",
                ws = binds.string(&workspace_id),
                hashes = binds.strings(&hashes)
            );
            let still = referenced(clickhouse, &condition, &binds).await?.remove(&workspace_id).unwrap_or_default();
            let gone: Vec<String> = hashes.into_iter().filter(|h| !still.contains(h)).collect();
            if !gone.is_empty() {
                let mut binds = Binds::new();
                let sql = format!(
                    "DELETE FROM {ATTACHMENT_TABLE} WHERE workspace_id = {ws} AND has({gone}, hash)",
                    ws = binds.string(&workspace_id),
                    gone = binds.strings(&gone)
                );
                clickhouse.command(&sql, &binds, &[]).await?;
            }
        }
    }
    Ok(count.records)
}
