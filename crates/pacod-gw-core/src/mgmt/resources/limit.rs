//! Limits, their live state, end-user overrides and resets (concept section 12).

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::limits::LimitState;
use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{
    BuildCtx, Ops, Params, Resource, ScopeIds, ScopeKind, collection_path, load, register, resolve_scope,
};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{
    Doc, EndUserOverride, LimitSpec, LimitSubject, Metric, Obj, OrgLimit, Window, WorkspaceLimit, is_slug,
};
use crate::snapshot::Snapshot;

fn check_spec(spec: &LimitSpec, org_level: bool) -> ApiResult<()> {
    if !is_slug(&spec.name) {
        return Err(ApiError::invalid("name must be lower-case letters, digits and hyphens"));
    }
    if !spec.amount.is_finite() || spec.amount < 0.0 {
        return Err(ApiError::invalid("amount must be zero or more"));
    }
    match (org_level, spec.subject) {
        (true, LimitSubject::Organisation) => {}
        (true, _) => {
            return Err(ApiError::invalid(
                "an organisation's limit counts the whole organisation: subject must be organisation",
            ));
        }
        (false, LimitSubject::Organisation) => {
            return Err(ApiError::invalid("a workspace's limit counts the workspace, each key or each end user"));
        }
        (false, _) => {}
    }
    if spec.window == Window::Minute && spec.metric == Metric::Spend {
        return Err(ApiError::invalid("a per-minute limit counts requests or tokens, not spend"));
    }
    if spec.alert_thresholds.iter().any(|t| *t == 0 || *t > 200) {
        return Err(ApiError::invalid("alert thresholds are percentages between 1 and 200"));
    }
    Ok(())
}

impl Resource for OrgLimit {
    const PLURAL: &'static str = "limits";
    const LABEL: &'static str = "limit";
    const SCOPE: ScopeKind = ScopeKind::Organisation;
    type Input = LimitSpec;
    type View = LimitSpec;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.0.name.clone())
    }

    fn build(input: Self::Input, _existing: Option<&Obj<Self>>, _ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        check_spec(&input, true)?;
        Ok(OrgLimit(input))
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.0.clone()
    }

    /// A cap on an organisation is set by the platform, not by the organisation itself.
    fn can_write(p: &Principal, _scope: &ScopeIds, _snap: &Snapshot) -> bool {
        p.is_platform_admin()
    }
}

impl Resource for WorkspaceLimit {
    const PLURAL: &'static str = "limits";
    const LABEL: &'static str = "limit";
    const SCOPE: ScopeKind = ScopeKind::Workspace;
    type Input = LimitSpec;
    type View = LimitSpec;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.0.name.clone())
    }

    fn build(input: Self::Input, _existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        check_spec(&input, false)?;
        let ws = ctx.snap.workspaces.get(ctx.scope.ws()).ok_or_else(|| ApiError::not_found("workspace"))?;
        for key in input.filter.tags.keys() {
            if !ws.settings.tag_keys.iter().any(|k| k == key) {
                return Err(ApiError::invalid(format!("the tag key \"{key}\" is not declared in this workspace")));
            }
        }
        Ok(WorkspaceLimit(input))
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.0.clone()
    }
}

impl Resource for EndUserOverride {
    const PLURAL: &'static str = "end-user-overrides";
    const LABEL: &'static str = "end-user override";
    const SCOPE: ScopeKind = ScopeKind::Workspace;
    type Input = EndUserOverride;
    type View = EndUserOverride;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(format!("{}|{}", doc.limit_id, doc.end_user))
    }

    fn build(input: Self::Input, _existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        let ws = ctx.snap.workspaces.get(ctx.scope.ws()).ok_or_else(|| ApiError::not_found("workspace"))?;
        let limit = ws
            .limits
            .iter()
            .find(|l| l.id == input.limit_id && l.owner_id == ws.id)
            .ok_or_else(|| ApiError::invalid("limit_id does not name a limit of this workspace"))?;
        if limit.spec.subject != LimitSubject::EndUser {
            return Err(ApiError::invalid("only end-user limits can be overridden per end user"));
        }
        if input.end_user.trim().is_empty() {
            return Err(ApiError::invalid("end_user must not be empty"));
        }
        if !input.exempt && input.amount.is_none_or(|a| !a.is_finite() || a < 0.0) {
            return Err(ApiError::invalid("give an amount of zero or more, or set exempt"));
        }
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }
}

#[derive(Debug, Default, Deserialize)]
struct StateQuery {
    /// Show the state for this end user.
    end_user: Option<String>,
    /// Show the state for this API key (its id). Default: the first key of the workspace.
    key_id: Option<String>,
}

/// The live state of every limit that applies in a workspace.
async fn state(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Query(q): Query<StateQuery>,
) -> ApiResult<Json<Vec<LimitState>>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !WorkspaceLimit::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let ws = &snap.workspaces[scope.ws()];
    // Key limits are per key: without a key there is nothing to show for them.
    let key = snap
        .keys
        .values()
        .filter(|k| k.workspace_id == ws.id && k.valid_until.is_none())
        .find(|k| q.key_id.as_deref().is_none_or(|id| id == k.id));
    let Some(key) = key else {
        return Ok(Json(Vec::new()));
    };
    // From the stored totals: this instance may serve no traffic at all.
    let applicable = st.limits.applicable_for(ws, key, q.end_user.as_deref().unwrap_or_default(), Utc::now());
    Ok(Json(st.limits.stored_state(&st.store, &applicable).await?))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResetInput {
    /// The key id or end user whose counters are cleared. For workspace limits, leave empty.
    #[serde(default)]
    pub subject: String,
}

/// Clears a subject's counters for one limit.
async fn reset(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<ResetInput>,
) -> ApiResult<StatusCode> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !WorkspaceLimit::can_write(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("limit"))?;
    let limit = load::<WorkspaceLimit>(&st, id, &scope).await?;
    let subject = match limit.doc.0.subject {
        LimitSubject::Workspace => scope.ws().to_owned(),
        _ if input.subject.trim().is_empty() => {
            return Err(ApiError::invalid("subject is required: the key id or the end user to reset"));
        }
        _ => input.subject.trim().to_owned(),
    };
    st.limits.reset_stored(&st.store, &limit.id, &subject).await?;
    let mut audit = p.audit(scope.org.as_deref(), scope.ws.as_deref());
    audit.action = "reset_counters".into();
    audit.kind = WorkspaceLimit::KIND.into();
    audit.object_id = limit.id.clone();
    audit.detail = serde_json::json!({ "subject": subject });
    st.store.audit(&audit).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes(mut router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    router = register::<OrgLimit>(router, api, Ops::ALL);
    router = register::<WorkspaceLimit>(router, api, Ops::ALL);
    router = register::<EndUserOverride>(router, api, Ops::ALL);

    let base = collection_path(ScopeKind::Workspace, "limits");
    let state_schema = api.schema::<LimitState>();
    let reset_in = api.schema::<ResetInput>();
    // `limit-state` is a sibling of `limits`, so that it cannot collide with a limit id.
    let state_path = collection_path(ScopeKind::Workspace, "limit-state");
    api.add(
        api.op("get", &state_path, "limitState", "limits", "Used and remaining allowance of every limit that applies")
            .query("end_user", "Show end-user limits for this end user.")
            .query("key_id", "Show key limits for this key.")
            .ok(serde_json::json!({ "type": "array", "items": state_schema })),
    );
    api.add(
        api.op(
            "post",
            &format!("{base}/{{id}}/reset"),
            "resetLimit",
            "limits",
            "Clear a subject's counters for one limit",
        )
        .body(reset_in)
        .no_content(),
    );
    router.route(&state_path, get(state)).route(&format!("{base}/{{id}}/reset"), post(reset))
}
