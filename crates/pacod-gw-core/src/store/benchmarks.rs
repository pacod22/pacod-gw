//! Benchmark reports: what a benchmark published or a platform admin uploaded. Each is kept as
//! sent; the columns are what the console's list shows.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;

use super::{Store, StoreError};

/// A report in the list: everything but the document.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct BenchmarkSummary {
    pub id: String,
    pub name: String,
    pub schema: String,
    pub schema_minor: i32,
    pub report_id: String,
    pub created_at: DateTime<Utc>,
    pub imported_at: DateTime<Utc>,
    /// `push` or `upload`.
    pub source: String,
    /// The caller that gave it to the gateway: its id and name.
    pub published_by: String,
    pub published_by_name: String,
    /// Who ran it, as the report says.
    pub run_by: String,
    pub target_label: String,
    pub target_kind: String,
    pub product: String,
    pub gateway_version: String,
    pub layout: String,
    /// `behaviour` or `capacity`.
    pub measurement_class: String,
    pub cells: i32,
    pub size_bytes: i32,
    pub digest: String,
}

const SUMMARY: &str = "id, name, schema, schema_minor, report_id, created_at, imported_at, source, published_by,
    published_by_name, run_by, target_label, target_kind, product, gateway_version, layout, measurement_class, cells,
    size_bytes, digest";

fn summary(r: &sqlx::postgres::PgRow) -> Result<BenchmarkSummary, sqlx::Error> {
    Ok(BenchmarkSummary {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        schema: r.try_get("schema")?,
        schema_minor: r.try_get("schema_minor")?,
        report_id: r.try_get("report_id")?,
        created_at: r.try_get("created_at")?,
        imported_at: r.try_get("imported_at")?,
        source: r.try_get("source")?,
        published_by: r.try_get("published_by")?,
        published_by_name: r.try_get("published_by_name")?,
        run_by: r.try_get("run_by")?,
        target_label: r.try_get("target_label")?,
        target_kind: r.try_get("target_kind")?,
        product: r.try_get("product")?,
        gateway_version: r.try_get("gateway_version")?,
        layout: r.try_get("layout")?,
        measurement_class: r.try_get("measurement_class")?,
        cells: r.try_get("cells")?,
        size_bytes: r.try_get("size_bytes")?,
        digest: r.try_get("digest")?,
    })
}

impl Store {
    /// Keeps a report; a report with the same digest is kept once: its summary, and false.
    pub async fn insert_benchmark_report(
        &self,
        s: &BenchmarkSummary,
        document: &[u8],
    ) -> Result<(BenchmarkSummary, bool), StoreError> {
        let inserted = sqlx::query(&format!(
            "INSERT INTO benchmark_reports ({SUMMARY}, document)
             VALUES ($1, $2, $3, $4, $5, $6, now(), $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
             ON CONFLICT (digest) DO NOTHING
             RETURNING {SUMMARY}"
        ))
        .bind(&s.id)
        .bind(&s.name)
        .bind(&s.schema)
        .bind(s.schema_minor)
        .bind(&s.report_id)
        .bind(s.created_at)
        .bind(&s.source)
        .bind(&s.published_by)
        .bind(&s.published_by_name)
        .bind(&s.run_by)
        .bind(&s.target_label)
        .bind(&s.target_kind)
        .bind(&s.product)
        .bind(&s.gateway_version)
        .bind(&s.layout)
        .bind(&s.measurement_class)
        .bind(s.cells)
        .bind(s.size_bytes)
        .bind(&s.digest)
        .bind(document)
        .fetch_optional(self.pool())
        .await?;
        if let Some(row) = inserted {
            return Ok((summary(&row)?, true));
        }
        let row = sqlx::query(&format!("SELECT {SUMMARY} FROM benchmark_reports WHERE digest = $1"))
            .bind(&s.digest)
            .fetch_one(self.pool())
            .await?;
        Ok((summary(&row)?, false))
    }

    /// Newest first, `limit` at most, older than the report `before` when given.
    pub async fn benchmark_reports(
        &self,
        before: Option<&str>,
        limit: i64,
    ) -> Result<Vec<BenchmarkSummary>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {SUMMARY} FROM benchmark_reports WHERE ($1::text IS NULL OR id < $1) ORDER BY id DESC LIMIT $2"
        ))
        .bind(before)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(summary).collect::<Result<_, _>>()?)
    }

    /// One report: its summary and the document as it was sent.
    pub async fn benchmark_report(&self, id: &str) -> Result<Option<(BenchmarkSummary, Vec<u8>)>, StoreError> {
        let row = sqlx::query(&format!("SELECT {SUMMARY}, document FROM benchmark_reports WHERE id = $1"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        match row {
            Some(r) => Ok(Some((summary(&r)?, r.try_get("document")?))),
            None => Ok(None),
        }
    }

    pub async fn delete_benchmark_report(&self, id: &str) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM benchmark_reports WHERE id = $1").bind(id).execute(self.pool()).await?;
        Ok(done.rows_affected() > 0)
    }
}
