//! Benchmark reports (`pacod-bench-report/1`, crates/pacod-gw-bench-report): what
//! pacod-benchmark publishes, or a platform admin uploads from a file, for the console's
//! Benchmarks page (docs/testing.md, section 10).
//!
//! A report is checked in full before it is kept, whoever sent it: the gateway takes it as
//! untrusted input. It is kept byte for byte as sent, so that a later gateway reads fields this
//! one does not know, and the same report sent twice is kept once (by its SHA-256). A report is
//! not configuration: keeping one changes no configuration version.
//!
//! Platform admins only: a report describes the installation as a whole.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use pacod_gw_bench_report::{MAX_BYTES, Problem, Report, Source, parse};
use ring::digest;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::AdminState;
use super::auth::Principal;
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use crate::store::benchmarks::BenchmarkSummary;

/// The header the console sets on an upload; without it a report counts as published.
const SOURCE_HEADER: &str = "x-gw-bench-source";

#[derive(Debug, Serialize, JsonSchema)]
struct BenchmarkList {
    items: Vec<BenchmarkSummary>,
    /// The id to pass as `before` for the next page, if there is one.
    next_before: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct BenchmarkView {
    summary: BenchmarkSummary,
    /// The report as it was sent.
    document: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
struct BenchmarkStored {
    summary: BenchmarkSummary,
    /// False when the same report was kept before.
    created: bool,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    before: Option<String>,
    limit: Option<i64>,
}

fn platform_admin(p: &Principal) -> ApiResult<()> {
    if p.is_platform_admin() { Ok(()) } else { Err(ApiError::forbidden()) }
}

/// The list's columns, from the report.
fn summary_of(r: &Report, source: &str, p: &Principal, bytes: &[u8], digest: String) -> BenchmarkSummary {
    let fact = |name: &str| r.fact(name).unwrap_or_default();
    let observed_version =
        r.environment.get("gateway_version").filter(|f| f.source == Source::Observed).map(|_| fact("gateway_version"));
    BenchmarkSummary {
        id: crate::model::new_id("bmr"),
        name: r.name.clone(),
        schema: r.schema.clone(),
        schema_minor: r.minor as i32,
        report_id: r.id.clone(),
        created_at: r.created_at,
        imported_at: chrono::Utc::now(),
        source: source.to_owned(),
        published_by: p.id.clone(),
        published_by_name: p.display.clone(),
        run_by: r.run_by.as_ref().map(|b| b.name.clone()).unwrap_or_default(),
        target_label: r.target.label.clone(),
        target_kind: serde_json::to_value(r.target.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        product: r.target.product.clone(),
        gateway_version: observed_version.unwrap_or_else(|| fact("gateway_version")),
        layout: fact("layout"),
        measurement_class: serde_json::to_value(r.measurement.class)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        cells: r.cells.len() as i32,
        size_bytes: bytes.len() as i32,
        digest,
    }
}

fn refused(problems: Vec<Problem>) -> ApiError {
    let first = problems.first().map(|p| format!("{}: {}", p.path, p.message)).unwrap_or_default();
    ApiError::invalid(format!("the report is not valid: {first}")).with_details(json!({ "problems": problems }))
}

async fn list(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<BenchmarkList>> {
    platform_admin(&p)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let items = st.store.benchmark_reports(q.before.as_deref(), limit).await?;
    let next_before = (items.len() as i64 == limit).then(|| items.last().map(|s| s.id.clone())).flatten();
    Ok(Json(BenchmarkList { items, next_before }))
}

async fn one(State(st): State<AdminState>, p: Principal, Path(id): Path<String>) -> ApiResult<Json<BenchmarkView>> {
    platform_admin(&p)?;
    let (summary, bytes) =
        st.store.benchmark_report(&id).await?.ok_or_else(|| ApiError::not_found("benchmark report"))?;
    let document = serde_json::from_slice(&bytes).map_err(ApiError::internal)?;
    Ok(Json(BenchmarkView { summary, document }))
}

/// The report as it was sent, byte for byte: downloaded and uploaded again, it is the same
/// report (its digest).
async fn document(State(st): State<AdminState>, p: Principal, Path(id): Path<String>) -> ApiResult<Response> {
    platform_admin(&p)?;
    let (summary, bytes) =
        st.store.benchmark_report(&id).await?.ok_or_else(|| ApiError::not_found("benchmark report"))?;
    let name = format!("attachment; filename=\"{}.json\"", summary.id);
    Ok(([(header::CONTENT_TYPE, "application/json".to_owned()), (header::CONTENT_DISPOSITION, name)], bytes)
        .into_response())
}

async fn store(
    State(st): State<AdminState>,
    p: Principal,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<BenchmarkStored>)> {
    platform_admin(&p)?;
    if body.len() > MAX_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!("a benchmark report is at most {MAX_BYTES} bytes"),
        ));
    }
    let report = parse(&body).map_err(refused)?;
    let source = match headers.get(SOURCE_HEADER).and_then(|v| v.to_str().ok()) {
        Some("upload") => "upload",
        _ => "push",
    };
    let digest = hex::encode(digest::digest(&digest::SHA256, &body));
    let summary = summary_of(&report, source, &p, &body, digest);
    let (summary, created) = st.store.insert_benchmark_report(&summary, &body).await?;
    if created {
        let mut entry = p.audit(None, None);
        entry.action = "keep_benchmark_report".into();
        entry.kind = "benchmark_report".into();
        entry.object_id = summary.id.clone();
        entry.detail =
            json!({ "name": summary.name, "source": source, "target": summary.target_label, "cells": summary.cells });
        st.store.audit(&entry).await?;
    }
    let status = if created { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(BenchmarkStored { summary, created })))
}

async fn remove(State(st): State<AdminState>, p: Principal, Path(id): Path<String>) -> ApiResult<StatusCode> {
    platform_admin(&p)?;
    if !st.store.delete_benchmark_report(&id).await? {
        return Err(ApiError::not_found("benchmark report"));
    }
    let mut entry = p.audit(None, None);
    entry.action = "delete_benchmark_report".into();
    entry.kind = "benchmark_report".into();
    entry.object_id = id;
    st.store.audit(&entry).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let list_schema = api.schema::<BenchmarkList>();
    let view = api.schema::<BenchmarkView>();
    let stored = api.schema::<BenchmarkStored>();
    let report = api.schema::<Report>();
    api.add(
        api.op("get", "/benchmark-reports", "listBenchmarkReports", "benchmarks", "Benchmark reports, newest first")
            .query("before", "The id of the last report of the previous page.")
            .query_int("limit", "At most this many (1 to 200, default 50).")
            .ok(list_schema),
    );
    api.add(
        api.op(
            "post",
            "/benchmark-reports",
            "storeBenchmarkReport",
            "benchmarks",
            "Keep a benchmark report (pacod-bench-report/1), checked in full; the same report twice is kept once",
        )
        .body(report.clone())
        .created(stored.clone())
        .ok(stored),
    );
    api.add(
        api.op(
            "get",
            "/benchmark-reports/{id}",
            "getBenchmarkReport",
            "benchmarks",
            "One benchmark report, as it was sent",
        )
        .ok(view),
    );
    api.add(
        api.op(
            "get",
            "/benchmark-reports/{id}/document",
            "getBenchmarkDocument",
            "benchmarks",
            "A benchmark report's document, byte for byte as it was sent",
        )
        .ok(report),
    );
    api.add(
        api.op("delete", "/benchmark-reports/{id}", "deleteBenchmarkReport", "benchmarks", "Delete a benchmark report")
            .no_content(),
    );
    router
        .route("/benchmark-reports", get(list).post(store))
        .route("/benchmark-reports/{id}", get(one).delete(remove))
        .route("/benchmark-reports/{id}/document", get(document))
}
