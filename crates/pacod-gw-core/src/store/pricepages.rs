//! What the price tracker knows about each price page (`price_page_state`), outside the
//! configuration: a daily check changes nothing an administrator decided.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::Row;

use super::{Store, StoreError};

/// The state of one price page.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct PageState {
    #[serde(skip)]
    pub page_id: String,
    #[serde(skip)]
    pub provider_id: String,
    pub last_checked_at: Option<DateTime<Utc>>,
    /// When the page last read differently from the time before.
    pub last_changed_at: Option<DateTime<Utc>>,
    /// When the extraction model last read it.
    pub last_read_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub content_hash: String,
    pub http_status: Option<i32>,
    /// Where the page ended up after redirects, when that is elsewhere.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub final_url: String,
    /// `read`, `unchanged`, `not_read` or `failed`; empty before the first check.
    pub outcome: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
    pub consecutive_failures: i32,
    /// `gone` or `moved`, when the scan found so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<String>,
    /// A check was asked for and has not happened yet.
    pub check_requested: bool,
    /// What the last reading found.
    #[serde(skip)]
    pub extraction: serde_json::Value,
    #[serde(skip)]
    pub previous_extraction: serde_json::Value,
}

const COLUMNS: &str = "page_id, provider_id, last_checked_at, last_changed_at, last_read_at, content_hash, \
                       http_status, final_url, outcome, message, consecutive_failures, flag, moved_to, \
                       check_requested, extraction, previous_extraction";

fn from_row(r: &sqlx::postgres::PgRow) -> Result<PageState, sqlx::Error> {
    Ok(PageState {
        page_id: r.try_get("page_id")?,
        provider_id: r.try_get("provider_id")?,
        last_checked_at: r.try_get("last_checked_at")?,
        last_changed_at: r.try_get("last_changed_at")?,
        last_read_at: r.try_get("last_read_at")?,
        content_hash: r.try_get("content_hash")?,
        http_status: r.try_get("http_status")?,
        final_url: r.try_get("final_url")?,
        outcome: r.try_get("outcome")?,
        message: r.try_get("message")?,
        consecutive_failures: r.try_get("consecutive_failures")?,
        flag: r.try_get("flag")?,
        moved_to: r.try_get("moved_to")?,
        check_requested: r.try_get("check_requested")?,
        extraction: r.try_get("extraction")?,
        previous_extraction: r.try_get("previous_extraction")?,
    })
}

impl Store {
    /// The state of every page that has one, by page id.
    pub async fn price_page_states(&self) -> Result<HashMap<String, PageState>, StoreError> {
        let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM price_page_state")).fetch_all(self.pool()).await?;
        let states = rows.iter().map(from_row).collect::<Result<Vec<_>, _>>()?;
        Ok(states.into_iter().map(|s| (s.page_id.clone(), s)).collect())
    }

    /// Asks for pages to be checked at the next run. `force` has them read by the extraction
    /// model even when they did not change.
    pub async fn price_pages_request_check(&self, pages: &[(String, String)], force: bool) -> Result<(), StoreError> {
        for (page_id, provider_id) in pages {
            sqlx::query(
                "INSERT INTO price_page_state (page_id, provider_id, check_requested, force_read)
                 VALUES ($1, $2, true, $3)
                 ON CONFLICT (page_id) DO UPDATE SET check_requested = true,
                   force_read = price_page_state.force_read OR $3, updated_at = now()",
            )
            .bind(page_id)
            .bind(provider_id)
            .bind(force)
            .execute(self.pool())
            .await?;
        }
        Ok(())
    }
}
