//! The analytics facade (concept section 10.3).
//!
//! The UI asks the core for reports. The core checks the caller's role, replaces any scope
//! filter with one the caller is entitled to, and forwards the query to the analytics service,
//! which runs every ClickHouse query. The analytics service is never reachable from outside.

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use pacod_gw_events::ingest::INTERNAL_TOKEN_HEADER;

use super::AdminState;
use super::auth::Principal;
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use crate::model::RoleScope;
use crate::snapshot::Snapshot;

/// The scope filter a principal's query is restricted to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportScope {
    pub organisation_id: Option<String>,
    pub workspace_id: Option<String>,
}

/// Decides the scope filter for a report query.
///
/// The caller may ask for an organisation or a workspace; it is granted only if they can read
/// it. A caller who asks for nothing gets everything they can read: no filter for a
/// platform-level reader, their single organisation or workspace otherwise.
pub fn report_scope(
    p: &Principal,
    snap: &Snapshot,
    asked_org: Option<&str>,
    asked_ws: Option<&str>,
) -> ApiResult<ReportScope> {
    if let Some(ws) = asked_ws {
        let w = snap.workspaces.get(ws).ok_or_else(|| ApiError::not_found("workspace"))?;
        if !p.can_read_workspace(ws, &w.organisation_id) {
            return Err(ApiError::forbidden());
        }
        return Ok(ReportScope { organisation_id: Some(w.organisation_id.clone()), workspace_id: Some(ws.to_owned()) });
    }
    if let Some(org) = asked_org {
        if !snap.organisations.contains_key(org) {
            return Err(ApiError::not_found("organisation"));
        }
        if !p.can_read_org(org) {
            return Err(ApiError::forbidden());
        }
        return Ok(ReportScope { organisation_id: Some(org.to_owned()), workspace_id: None });
    }
    let platform_reader = p.is_platform_admin() || p.roles.iter().any(|(_, s)| *s == RoleScope::Platform);
    if platform_reader {
        return Ok(ReportScope::default());
    }
    // Exactly one readable scope: use it. Otherwise the caller has to choose.
    let mut scopes: Vec<&RoleScope> = p.roles.iter().map(|(_, s)| s).collect();
    scopes.dedup();
    match scopes.as_slice() {
        [RoleScope::Organisation(o)] => Ok(ReportScope { organisation_id: Some(o.clone()), workspace_id: None }),
        [RoleScope::Workspace(w)] => {
            let ws = snap.workspaces.get(w).ok_or_else(ApiError::forbidden)?;
            Ok(ReportScope { organisation_id: Some(ws.organisation_id.clone()), workspace_id: Some(w.clone()) })
        }
        [] => Err(ApiError::forbidden()),
        _ => Err(ApiError::bad_request("name the organisation_id or workspace_id to report on")),
    }
}

/// Rebuilds a query string: the caller's parameters without any scope filter, plus the
/// granted scope.
pub fn scoped_query(raw: Option<&str>, scope: &ReportScope) -> (String, Option<String>, Option<String>) {
    let mut asked_org = None;
    let mut asked_ws = None;
    let mut out = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "organisation_id" => asked_org = Some(v.into_owned()),
            "workspace_id" => asked_ws = Some(v.into_owned()),
            _ => {
                out.append_pair(&k, &v);
            }
        }
    }
    if let Some(o) = &scope.organisation_id {
        out.append_pair("organisation_id", o);
    }
    if let Some(w) = &scope.workspace_id {
        out.append_pair("workspace_id", w);
    }
    (out.finish(), asked_org, asked_ws)
}

fn asked(raw: Option<&str>) -> (Option<String>, Option<String>) {
    let (_, org, ws) = scoped_query(raw, &ReportScope::default());
    (org, ws)
}

/// Forwards a report query and streams the answer back.
pub async fn forward(st: &AdminState, path: &str, query: &str) -> ApiResult<Response> {
    let base = st.analytics_url()?;
    let url = if query.is_empty() { format!("{base}{path}") } else { format!("{base}{path}?{query}") };
    let resp = st
        .http
        .get(url)
        .header(INTERNAL_TOKEN_HEADER, &st.boot.internal_token)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "analytics service unreachable");
            ApiError::unavailable(
                "the analytics service is not reachable; usage data is delayed, traffic is not affected",
            )
        })?;
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = resp.headers().get(CONTENT_TYPE).cloned();
    let mut out = Response::new(Body::from_stream(resp.bytes_stream()));
    *out.status_mut() = status;
    out.headers_mut().insert(CONTENT_TYPE, content_type.unwrap_or(HeaderValue::from_static("application/json")));
    Ok(out)
}

/// A report whose rows carry a scope: the common case.
async fn report(st: &AdminState, p: &Principal, path: &str, raw: Option<&str>) -> ApiResult<Response> {
    let snap = st.config.snapshot();
    let (asked_org, asked_ws) = asked(raw);
    let scope = report_scope(p, &snap, asked_org.as_deref(), asked_ws.as_deref())?;
    let (query, _, _) = scoped_query(raw, &scope);
    forward(st, path, &query).await
}

async fn requests(State(st): State<AdminState>, p: Principal, RawQuery(q): RawQuery) -> ApiResult<Response> {
    report(&st, &p, "/reports/requests", q.as_deref()).await
}

async fn request_one(State(st): State<AdminState>, p: Principal, Path(id): Path<String>) -> ApiResult<Response> {
    // The event names its workspace: fetch it, then decide whether the caller may see it.
    let base = st.analytics_url()?;
    let id = event_id(&id)?;
    let resp = st
        .http
        .get(format!("{base}/reports/requests/{id}"))
        .header(INTERNAL_TOKEN_HEADER, &st.boot.internal_token)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| ApiError::unavailable("the analytics service is not reachable"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(ApiError::not_found("request"));
    }
    if !resp.status().is_success() {
        return Err(ApiError::unavailable("the analytics service could not answer"));
    }
    let event: serde_json::Value = resp.json().await.map_err(ApiError::internal)?;
    let ws = event["workspace_id"].as_str().unwrap_or_default();
    let org = event["organisation_id"].as_str().unwrap_or_default();
    if !p.can_read_workspace(ws, org) {
        // Do not reveal that the request exists.
        return Err(ApiError::not_found("request"));
    }
    Ok(axum::Json(event).into_response())
}

use axum::response::IntoResponse;

// --- reports -----------------------------------------------------------------------------------

async fn overview(State(st): State<AdminState>, p: Principal, RawQuery(q): RawQuery) -> ApiResult<Response> {
    report(&st, &p, "/reports/overview", q.as_deref()).await
}

async fn usage(State(st): State<AdminState>, p: Principal, RawQuery(q): RawQuery) -> ApiResult<Response> {
    report(&st, &p, "/reports/usage", q.as_deref()).await
}

async fn breakdown(State(st): State<AdminState>, p: Principal, RawQuery(q): RawQuery) -> ApiResult<Response> {
    report(&st, &p, "/reports/breakdown", q.as_deref()).await
}

async fn end_users(State(st): State<AdminState>, p: Principal, RawQuery(q): RawQuery) -> ApiResult<Response> {
    report(&st, &p, "/reports/end-users", q.as_deref()).await
}

async fn accuracy(State(st): State<AdminState>, p: Principal, RawQuery(q): RawQuery) -> ApiResult<Response> {
    report(&st, &p, "/reports/accuracy", q.as_deref()).await
}

async fn errors(State(st): State<AdminState>, p: Principal, RawQuery(q): RawQuery) -> ApiResult<Response> {
    report(&st, &p, "/reports/errors", q.as_deref()).await
}

async fn delay(State(st): State<AdminState>, p: Principal, RawQuery(q): RawQuery) -> ApiResult<Response> {
    report(&st, &p, "/reports/delay", q.as_deref()).await
}

fn segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// The id of a request as the request log stores it. A caller knows a request by the value of
/// `x-gw-request-id` (`gw_` and the id without hyphens); both forms name the same request.
/// Anything that is not an id names no request.
fn event_id(id: &str) -> ApiResult<String> {
    uuid::Uuid::parse_str(id.strip_prefix("gw_").unwrap_or(id))
        .map(|id| id.to_string())
        .map_err(|_| ApiError::not_found("request"))
}

async fn end_user(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
) -> ApiResult<Response> {
    report(&st, &p, &format!("/reports/end-users/{}", segment(&id)), q.as_deref()).await
}

async fn trace(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
) -> ApiResult<Response> {
    report(&st, &p, &format!("/reports/traces/{}", segment(&id)), q.as_deref()).await
}

// --- audit records (concept section 14.6) --------------------------------------------------------

/// Fetches a JSON document from the analytics service.
async fn fetch(st: &AdminState, path_and_query: &str, what: &'static str) -> ApiResult<serde_json::Value> {
    let base = st.analytics_url()?;
    let resp = st
        .http
        .get(format!("{base}{path_and_query}"))
        .header(INTERNAL_TOKEN_HEADER, &st.boot.internal_token)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| ApiError::unavailable("the analytics service is not reachable"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(ApiError::not_found(what));
    }
    if !resp.status().is_success() {
        return Err(ApiError::unavailable("the analytics service could not answer"));
    }
    resp.json().await.map_err(ApiError::internal)
}

/// The complete record of one audited call. Workspace admins and auditors only, and every
/// read is written to the audit log.
async fn request_audit(State(st): State<AdminState>, p: Principal, Path(id): Path<String>) -> ApiResult<Response> {
    let id = event_id(&id)?;
    let event = fetch(&st, &format!("/reports/requests/{id}"), "request").await?;
    let ws = event["workspace_id"].as_str().unwrap_or_default().to_owned();
    let org = event["organisation_id"].as_str().unwrap_or_default().to_owned();
    if !p.can_read_workspace(&ws, &org) {
        // Do not reveal that the request exists.
        return Err(ApiError::not_found("request"));
    }
    if !p.can_read_audit_records(&ws, &org) {
        return Err(ApiError::forbidden());
    }
    let audit_id = event["audit_id"].as_str().filter(|a| !a.is_empty()).ok_or_else(|| {
        ApiError::not_found("audit record").with_details(serde_json::json!({ "code": "not_audited" }))
    })?;
    let record =
        fetch(&st, &format!("/reports/audit/{}?workspace_id={}", segment(audit_id), segment(&ws)), "audit record")
            .await?;

    let mut entry = p.audit(Some(&org), Some(&ws));
    entry.action = "read_audit_record".into();
    entry.kind = "audit_record".into();
    entry.object_id = audit_id.to_owned();
    entry.detail = serde_json::json!({ "request_id": id });
    st.store.audit(&entry).await?;
    Ok(axum::Json(record).into_response())
}

/// The scope of an audit-record query: one the caller may read audit records in.
fn audit_scope(st: &AdminState, p: &Principal, raw: Option<&str>) -> ApiResult<(String, ReportScope)> {
    let snap = st.config.snapshot();
    let (asked_org, asked_ws) = asked(raw);
    let scope = report_scope(p, &snap, asked_org.as_deref(), asked_ws.as_deref())?;
    let allowed = match (&scope.workspace_id, &scope.organisation_id) {
        (Some(ws), Some(org)) => p.can_read_audit_records(ws, org),
        // A whole organisation, or everything: only who may read audit records at that level.
        (None, Some(org)) => p.can_admin_org(org) || p.has_role_at(crate::model::RoleName::Auditor, Some(org)),
        _ => p.is_platform_admin() || p.has_role_at(crate::model::RoleName::Auditor, None),
    };
    if !allowed {
        return Err(ApiError::forbidden());
    }
    let (query, _, _) = scoped_query(raw, &scope);
    Ok((query, scope))
}

/// Audit records without their bodies, newest first.
async fn audit_records(State(st): State<AdminState>, p: Principal, RawQuery(q): RawQuery) -> ApiResult<Response> {
    let (query, _) = audit_scope(&st, &p, q.as_deref())?;
    forward(&st, "/reports/audit", &query).await
}

/// Deletes the audit records of one end user or one session of a workspace.
async fn delete_audit_records(
    State(st): State<AdminState>,
    p: Principal,
    RawQuery(q): RawQuery,
) -> ApiResult<Response> {
    let (query, scope) = audit_scope(&st, &p, q.as_deref())?;
    let (Some(ws), Some(org)) = (&scope.workspace_id, &scope.organisation_id) else {
        return Err(ApiError::bad_request("name the workspace_id whose records are to be deleted"));
    };
    if !p.can_admin_workspace(ws, org) {
        return Err(ApiError::forbidden());
    }
    let base = st.analytics_url()?;
    let resp = st
        .http
        .delete(format!("{base}/audit?{query}"))
        .header(INTERNAL_TOKEN_HEADER, &st.boot.internal_token)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .map_err(|_| ApiError::unavailable("the analytics service is not reachable"))?;
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = resp.bytes().await.map_err(ApiError::internal)?;
    if status.is_success() {
        let mut entry = p.audit(Some(org), Some(ws));
        entry.action = "delete_audit_records".into();
        entry.kind = "audit_record".into();
        entry.detail = serde_json::json!({ "filter": query });
        st.store.audit(&entry).await?;
    }
    let mut out = Response::new(Body::from(body));
    *out.status_mut() = status;
    out.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(out)
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let free = serde_json::json!({ "type": "object", "additionalProperties": true });
    // The parameters are those of the analytics service, which checks them strictly: one it
    // does not know is refused with 400, never ignored. Its API document describes each report.
    const RANGE: &[(&str, &str)] = &[
        ("from", "Start of the range, RFC 3339. Default 24 hours before `to`."),
        ("to", "End of the range, RFC 3339. Default now."),
    ];
    const SCOPE: &[(&str, &str)] =
        &[("organisation_id", "Restrict to one organisation."), ("workspace_id", "Restrict to one workspace.")];
    const FILTERS: &[(&str, &str)] = &[
        ("provider", "Provider name."),
        ("model", "Model served."),
        ("template", "Template name."),
        ("key_id", "API key id."),
        ("end_user", "End user id."),
        ("status", "ok, error, timeout, cancelled or rejected."),
        ("mode", "proxy or managed."),
        ("tag.{key}", "Calls whose tag {key} has this value, for example `tag.purpose=planner`."),
    ];
    const BUCKET: (&str, &str) = ("bucket", "The width of a point in time: hour (default), day, week or month.");
    const CSV: (&str, &str) = ("format", "json (default) or csv.");
    let mut report_op = |path: &str, id: &str, tag: &str, summary: &str, groups: &[&[(&str, &str)]]| {
        let mut op = api.op("get", path, id, tag, summary);
        for (name, description) in groups.iter().flat_map(|g| g.iter()) {
            op = if *name == "limit" || *name == "offset" {
                op.query_int(name, description)
            } else {
                op.query(name, description)
            };
        }
        api.add(op.ok(free.clone()));
    };
    report_op(
        "/analytics/overview",
        "analyticsOverview",
        "analytics",
        "Spend, tokens and requests for the current hour, day, week and month",
        &[&[("to", "The instant the periods end at, RFC 3339. Default now.")], SCOPE, FILTERS],
    );
    report_op(
        "/analytics/usage",
        "analyticsUsage",
        "analytics",
        "Usage over time, filtered and grouped",
        &[
            RANGE,
            SCOPE,
            FILTERS,
            &[
                BUCKET,
                (
                    "group_by",
                    "none (default), provider, model, template, key_id, end_user, status, mode, or `tag:{key}`.",
                ),
                CSV,
            ],
        ],
    );
    report_op(
        "/analytics/breakdown",
        "analyticsBreakdown",
        "analytics",
        "Totals per model, template, key, tag or end user",
        &[
            RANGE,
            SCOPE,
            FILTERS,
            &[
                ("by", "Required: provider, model, template, key_id, end_user, error_type, or `tag:{key}`."),
                ("limit", "The number of groups, 1 to 1000. Default 50."),
                CSV,
            ],
        ],
    );
    report_op(
        "/analytics/traces/{trace_id}",
        "analyticsTrace",
        "analytics",
        "Every call of one trace with its totals",
        &[SCOPE, &[("from", "Optional. Makes the lookup cheaper."), ("to", "Optional.")]],
    );
    report_op(
        "/analytics/end-users",
        "analyticsEndUsers",
        "analytics",
        "End users with their consumption",
        &[RANGE, SCOPE, FILTERS, &[("limit", "1 to 1000. Default 100."), ("offset", "Rows to skip.")]],
    );
    report_op(
        "/analytics/end-users/{id}",
        "analyticsEndUser",
        "analytics",
        "One end user in detail",
        &[RANGE, SCOPE, FILTERS, &[("limit", "The number of recent calls, 1 to 1000. Default 20.")]],
    );
    report_op(
        "/analytics/accuracy",
        "analyticsAccuracy",
        "analytics",
        "How much of the spend rests on provider-reported usage and known prices",
        &[RANGE, SCOPE, FILTERS],
    );
    report_op(
        "/errors",
        "listErrors",
        "requests",
        "Failures by provider, model, type and time",
        &[RANGE, SCOPE, FILTERS, &[BUCKET, ("limit", "The number of recent failures, 1 to 1000. Default 20.")]],
    );
    report_op(
        "/monitoring/delay",
        "monitoringDelay",
        "monitoring",
        "Gateway delay percentiles, its stages and the slowest calls",
        &[
            RANGE,
            SCOPE,
            FILTERS,
            &[
                BUCKET,
                ("group_by", "none (default), mode, provider, model, template, workspace_id or instance_id."),
                ("limit", "The number of slowest calls, 1 to 1000. Default 20."),
            ],
        ],
    );
    report_op(
        "/audit-records",
        "listAuditRecords",
        "requests",
        "Audit records without their bodies, newest first",
        &[
            RANGE,
            SCOPE,
            &[
                ("model", "The record's model."),
                ("template", "Template name."),
                ("key_id", "API key id."),
                ("end_user", "End user id."),
                ("mode", "proxy or managed."),
                ("session_id", "Records of one session."),
                ("trace_id", "Records of one trace."),
                ("limit", "Page size, 1 to 1000. Default 50."),
                ("cursor", "`next_cursor` of the previous page."),
            ],
        ],
    );
    report_op(
        "/requests",
        "listRequests",
        "requests",
        "The request log, newest first, with the timing of each call",
        &[
            RANGE,
            SCOPE,
            FILTERS,
            &[
                ("limit", "Page size, 1 to 1000. Default 50."),
                ("cursor", "`next_cursor` of the previous page."),
                ("trace_id", "Calls of one trace."),
                ("session_id", "Calls of one session."),
                ("audited", "true: only calls with an audit record. false: only calls without."),
                CSV,
            ],
        ],
    );
    api.add(
        api.op(
            "delete",
            "/audit-records",
            "deleteAuditRecords",
            "requests",
            "Delete the audit records of one end user or one session",
        )
        .query("workspace_id", "The workspace. Required.")
        .query("end_user", "Delete this end user's records.")
        .query("session_id", "Delete this session's records.")
        .ok(free.clone()),
    );
    api.add(
        api.op("get", "/requests/{id}/audit", "getRequestAudit", "requests", "The complete record of an audited call")
            .ok(free.clone()),
    );

    api.add(api.op("get", "/requests/{id}", "getRequest", "requests", "One call with all its timing stages").ok(free));
    router
        .route("/requests", get(requests))
        .route("/requests/{id}", get(request_one))
        .route("/requests/{id}/audit", get(request_audit))
        .route("/audit-records", get(audit_records).delete(delete_audit_records))
        .route("/errors", get(errors))
        .route("/analytics/overview", get(overview))
        .route("/analytics/usage", get(usage))
        .route("/analytics/breakdown", get(breakdown))
        .route("/analytics/traces/{trace_id}", get(trace))
        .route("/analytics/end-users", get(end_users))
        .route("/analytics/end-users/{id}", get(end_user))
        .route("/analytics/accuracy", get(accuracy))
        .route("/monitoring/delay", get(delay))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_named_by_its_id_or_by_the_header_value() {
        let id = "0198a6c2-6f0e-7d31-9c55-0d2b1f3a4e77";
        assert_eq!(event_id(id).unwrap(), id);
        assert_eq!(event_id("gw_0198a6c26f0e7d319c550d2b1f3a4e77").unwrap(), id);
        assert!(event_id("gw_").is_err());
        assert!(event_id("../audit").is_err());
    }
}
