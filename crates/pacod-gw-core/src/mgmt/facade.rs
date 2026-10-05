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
    let id = url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>();
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

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let free = serde_json::json!({ "type": "object", "additionalProperties": true });
    api.add(
        api.op(
            "get",
            "/requests",
            "listRequests",
            "requests",
            "The request log, newest first, with the timing of each call",
        )
        .query("from", "Start of the range, RFC 3339. Default 24 hours ago.")
        .query("to", "End of the range, RFC 3339. Default now.")
        .query("organisation_id", "Restrict to one organisation.")
        .query("workspace_id", "Restrict to one workspace.")
        .query_int("limit", "Page size.")
        .query("cursor", "Continue a listing.")
        .query("trace_id", "Calls of one trace.")
        .query("session_id", "Calls of one session.")
        .query("status", "ok, error, timeout, cancelled or rejected.")
        .query("provider", "Provider name.")
        .query("model", "Model served.")
        .query("key_id", "API key id.")
        .query("end_user", "End user id.")
        .ok(free.clone()),
    );
    api.add(api.op("get", "/requests/{id}", "getRequest", "requests", "One call with all its timing stages").ok(free));
    router.route("/requests", get(requests)).route("/requests/{id}", get(request_one))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mgmt::auth::PrincipalKind;
    use crate::model::{Organisation, RoleName, Status, Workspace};
    use crate::snapshot::{OrgRt, WorkspaceRt};
    use std::sync::Arc;

    fn snap() -> Snapshot {
        let mut s = Snapshot::default();
        for o in ["org_a", "org_b"] {
            s.organisations.insert(
                o.into(),
                Arc::new(OrgRt {
                    id: o.into(),
                    doc: Organisation {
                        name: o.into(),
                        display_name: o.into(),
                        status: Status::Active,
                        settings: Default::default(),
                    },
                    grants: Default::default(),
                    limits: Vec::new(),
                }),
            );
        }
        for (w, o) in [("ws_a1", "org_a"), ("ws_a2", "org_a"), ("ws_b1", "org_b")] {
            let doc: Workspace = serde_json::from_value(serde_json::json!({ "name": w })).unwrap();
            let settings = crate::model::settings::ResolvedSettings::resolve(
                &Default::default(),
                &Default::default(),
                &Default::default(),
            );
            s.workspaces.insert(
                w.into(),
                Arc::new(WorkspaceRt {
                    id: w.into(),
                    organisation_id: o.into(),
                    doc,
                    settings,
                    enabled: true,
                    grants: None,
                    templates: Default::default(),
                    limits: Vec::new(),
                    overrides: Default::default(),
                }),
            );
        }
        s
    }

    fn p(roles: Vec<(RoleName, RoleScope)>) -> Principal {
        Principal { id: "u".into(), kind: PrincipalKind::User, display: "u".into(), roles, session_hash: None }
    }

    #[test]
    fn platform_readers_get_no_filter_unless_they_ask() {
        let admin = p(vec![(RoleName::PlatformAdmin, RoleScope::Platform)]);
        assert_eq!(report_scope(&admin, &snap(), None, None).unwrap(), ReportScope::default());
        let s = report_scope(&admin, &snap(), None, Some("ws_b1")).unwrap();
        assert_eq!((s.organisation_id.as_deref(), s.workspace_id.as_deref()), (Some("org_b"), Some("ws_b1")));
    }

    #[test]
    fn a_workspace_reader_is_confined_to_the_workspace() {
        let v = p(vec![(RoleName::Viewer, RoleScope::Workspace("ws_a1".into()))]);
        let s = report_scope(&v, &snap(), None, None).unwrap();
        assert_eq!((s.organisation_id.as_deref(), s.workspace_id.as_deref()), (Some("org_a"), Some("ws_a1")));
        assert!(report_scope(&v, &snap(), None, Some("ws_a2")).is_err(), "another workspace of the same organisation");
        assert!(report_scope(&v, &snap(), Some("org_a"), None).is_err(), "the whole organisation");
        assert!(report_scope(&v, &snap(), None, Some("ws_b1")).is_err());
    }

    #[test]
    fn an_organisation_reader_sees_the_organisation_and_its_workspaces() {
        let a = p(vec![(RoleName::OrgAdmin, RoleScope::Organisation("org_a".into()))]);
        assert_eq!(report_scope(&a, &snap(), None, None).unwrap().organisation_id.as_deref(), Some("org_a"));
        assert!(report_scope(&a, &snap(), None, Some("ws_a2")).is_ok());
        assert!(report_scope(&a, &snap(), Some("org_b"), None).is_err());
        assert!(report_scope(&a, &snap(), None, Some("ws_b1")).is_err());
    }

    #[test]
    fn several_scopes_require_a_choice_and_none_is_forbidden() {
        let two = p(vec![
            (RoleName::Viewer, RoleScope::Workspace("ws_a1".into())),
            (RoleName::Viewer, RoleScope::Workspace("ws_b1".into())),
        ]);
        assert_eq!(report_scope(&two, &snap(), None, None).unwrap_err().status, StatusCode::BAD_REQUEST);
        assert_eq!(report_scope(&p(vec![]), &snap(), None, None).unwrap_err().status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn the_callers_scope_parameters_are_replaced_not_trusted() {
        let granted = ReportScope { organisation_id: Some("org_a".into()), workspace_id: Some("ws_a1".into()) };
        let (q, asked_org, asked_ws) = scoped_query(
            Some("workspace_id=ws_b1&organisation_id=org_b&limit=5&workspace_id=ws_b2&model=a%2Fb"),
            &granted,
        );
        assert_eq!(asked_org.as_deref(), Some("org_b"));
        assert_eq!(asked_ws.as_deref(), Some("ws_b2"));
        assert_eq!(q, "limit=5&model=a%2Fb&organisation_id=org_a&workspace_id=ws_a1");
        assert!(!q.contains("org_b") && !q.contains("ws_b"));
    }
}
