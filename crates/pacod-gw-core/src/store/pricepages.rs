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
    /// The page is to be read at its next check even when it did not change.
    #[serde(skip)]
    pub force_read: bool,
    /// What the last reading found.
    #[serde(skip)]
    pub extraction: serde_json::Value,
    #[serde(skip)]
    pub previous_extraction: serde_json::Value,
}

const COLUMNS: &str = "page_id, provider_id, last_checked_at, last_changed_at, last_read_at, content_hash, \
                       http_status, final_url, outcome, message, consecutive_failures, flag, moved_to, \
                       check_requested, force_read, extraction, previous_extraction";

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
        force_read: r.try_get("force_read")?,
        extraction: r.try_get("extraction")?,
        previous_extraction: r.try_get("previous_extraction")?,
    })
}

/// What one check of a page found, as it is recorded.
#[derive(Debug, Clone, Default)]
pub struct Recording {
    /// `read`, `unchanged`, `not_read` or `failed`.
    pub outcome: String,
    pub message: String,
    pub http_status: Option<i32>,
    pub final_url: String,
    /// Stored only after a successful reading, so that a page that could not be read is read
    /// again at its next check.
    pub content_hash: Option<String>,
    pub text_hash: Option<String>,
    /// What the reading found; the one before is kept as the previous reading.
    pub extraction: Option<serde_json::Value>,
}

impl Store {
    /// Records one check of a page. A reading or an unchanged page answers the requests for a
    /// check; a page that failed or was not read keeps being asked to be read.
    pub async fn price_page_record(&self, page_id: &str, provider_id: &str, r: &Recording) -> Result<(), StoreError> {
        let failed = r.outcome == "failed";
        let done = matches!(r.outcome.as_str(), "read" | "unchanged");
        sqlx::query(
            "INSERT INTO price_page_state (page_id, provider_id, last_checked_at, outcome, message, http_status,
                 final_url, consecutive_failures, content_hash, text_hash, last_changed_at, last_read_at, extraction)
             VALUES ($1, $2, now(), $3, $4, $5, $6, CASE WHEN $7 THEN 1 ELSE 0 END, coalesce($8, ''), coalesce($9, ''),
                 CASE WHEN $8 IS NOT NULL THEN now() END, CASE WHEN $10::jsonb IS NOT NULL THEN now() END,
                 coalesce($10, '{}'::jsonb))
             ON CONFLICT (page_id) DO UPDATE SET
               provider_id = $2, last_checked_at = now(), outcome = $3, message = $4, http_status = $5, final_url = $6,
               consecutive_failures = CASE WHEN $7 THEN price_page_state.consecutive_failures + 1 ELSE 0 END,
               last_changed_at = CASE WHEN $8 IS NOT NULL AND $8 <> price_page_state.content_hash THEN now()
                                      ELSE price_page_state.last_changed_at END,
               content_hash = coalesce($8, price_page_state.content_hash),
               text_hash = coalesce($9, price_page_state.text_hash),
               last_read_at = CASE WHEN $10::jsonb IS NOT NULL THEN now() ELSE price_page_state.last_read_at END,
               previous_extraction = CASE WHEN $10::jsonb IS NOT NULL THEN price_page_state.extraction
                                          ELSE price_page_state.previous_extraction END,
               extraction = coalesce($10, price_page_state.extraction),
               check_requested = CASE WHEN $11 THEN false ELSE price_page_state.check_requested END,
               force_read = CASE WHEN $11 THEN false ELSE price_page_state.force_read END,
               updated_at = now()",
        )
        .bind(page_id)
        .bind(provider_id)
        .bind(&r.outcome)
        .bind(&r.message)
        .bind(r.http_status)
        .bind(&r.final_url)
        .bind(failed)
        .bind(&r.content_hash)
        .bind(&r.text_hash)
        .bind(&r.extraction)
        .bind(done)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Records what the scan found of a page: `gone`, `moved` (to `moved_to`), or nothing wrong.
    pub async fn price_page_flag(
        &self,
        page_id: &str,
        provider_id: &str,
        flag: Option<&str>,
        moved_to: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO price_page_state (page_id, provider_id, flag, moved_to) VALUES ($1, $2, $3, $4)
             ON CONFLICT (page_id) DO UPDATE SET flag = $3, moved_to = $4, updated_at = now()",
        )
        .bind(page_id)
        .bind(provider_id)
        .bind(flag)
        .bind(moved_to)
        .execute(self.pool())
        .await?;
        Ok(())
    }

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
