//! Routing schemes of an organisation (concept section 7.5).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{BuildCtx, Ops, Params, Resource, ScopeIds, ScopeKind, register, resolve_scope};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{Doc, Obj, Organisation, RoutingScheme, Template, Workspace, default_routing_schemes};
use crate::routing::{CompileError, compile};
use crate::snapshot::Snapshot;
use crate::store::Tx;

pub fn compile_error(e: CompileError) -> ApiError {
    ApiError::invalid(e.to_string()).with_details(serde_json::json!({ "code": e.code, "rule": e.rule }))
}

/// What names the scheme `name` of an organisation: the organisation, its workspaces and the
/// current versions of their templates.
pub async fn scheme_references(tx: &mut Tx, org: &str, name: &str) -> ApiResult<Vec<String>> {
    let mut by = Vec::new();
    if let Some(row) = tx.get(org).await? {
        let o: Obj<Organisation> = row.into_obj()?;
        if o.doc.settings.routing_scheme.as_deref() == Some(name) {
            by.push(format!("organisation {}", o.doc.name));
        }
    }
    for row in tx.list(Workspace::KIND, Some(org)).await? {
        let ws: Obj<Workspace> = row.into_obj()?;
        if ws.doc.settings.routing_scheme.as_deref() == Some(name) {
            by.push(format!("workspace {}", ws.doc.name));
        }
        for row in tx.list(Template::KIND, Some(&ws.id)).await? {
            let t: Obj<Template> = row.into_obj()?;
            if t.doc.current().is_some_and(|v| v.spec.routing_scheme.as_deref() == Some(name)) {
                by.push(format!("template {}/{}", ws.doc.name, t.doc.name));
            }
        }
    }
    Ok(by)
}

fn in_use(by: Vec<String>) -> ApiError {
    ApiError::conflict("the routing scheme is in use; change what names it first")
        .with_details(serde_json::json!({ "code": "routing_scheme_in_use", "by": by }))
}

impl Resource for RoutingScheme {
    const PLURAL: &'static str = "routing-schemes";
    const LABEL: &'static str = "routing scheme";
    const SCOPE: ScopeKind = ScopeKind::Organisation;
    const STRICT: bool = true;
    type Input = RoutingScheme;
    type View = RoutingScheme;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(input: Self::Input, _existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        // The same compiler as the snapshot: what is accepted here is what runs.
        compile(ctx.id, 0, &input).map_err(compile_error)?;
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }

    async fn check(tx: &mut Tx, doc: &Self, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<()> {
        // Schemes are named, not referenced by id: a rename would orphan its users.
        if let Some(e) = existing
            && e.doc.name != doc.name
        {
            let by = scheme_references(tx, ctx.scope.org(), &e.doc.name).await?;
            if !by.is_empty() {
                return Err(in_use(by));
            }
        }
        Ok(())
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let org = obj.parent.as_deref().unwrap_or_default();
        let by = scheme_references(tx, org, &obj.doc.name).await?;
        if !by.is_empty() {
            return Err(in_use(by));
        }
        Ok(())
    }

    /// Workspace admins choose a scheme for their templates, so they may read them.
    fn can_read(p: &Principal, scope: &ScopeIds, snap: &Snapshot) -> bool {
        p.sees_org(scope.org(), snap)
    }
}

/// The routing scheme an organisation assigns, for templates that name none and whose
/// workspace names none.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RoutingAssignment {
    /// The organisation's scheme. Unset: templates without a scheme of their own or of their
    /// workspace use their models by priority.
    pub scheme: Option<String>,
    /// The organisation's scheme applies to every workspace and template; their own
    /// assignments are kept but do not apply.
    pub locked: bool,
    /// The organisation's config version, to send back on change.
    pub version: i32,
    /// Whether the caller may change the assignment: the organisation's administrators.
    pub writable: bool,
    /// Workspaces and templates that name a scheme of their own: with the lock, theirs does
    /// not apply.
    pub own_assignments: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutingAssignmentInput {
    /// A scheme of the organisation, or none.
    pub scheme: Option<String>,
    pub locked: bool,
    /// The version last read, to detect a concurrent change.
    #[serde(default)]
    pub version: Option<i32>,
}

fn own_assignments(snap: &Snapshot, org: &str) -> Vec<String> {
    let mut out = Vec::new();
    for ws in snap.workspaces.values().filter(|w| w.organisation_id == org) {
        if let Some(s) = &ws.doc.settings.routing_scheme {
            out.push(format!("workspace {}: {s}", ws.doc.name));
        }
        for t in ws.templates.values() {
            if let Some(s) = &t.spec.routing_scheme {
                out.push(format!("template {}/{}: {s}", ws.doc.name, t.name));
            }
        }
    }
    out.sort();
    out
}

async fn assignment_of(st: &AdminState, p: &Principal, org: &str) -> ApiResult<RoutingAssignment> {
    let obj: Obj<Organisation> = st.store.get_obj(org).await.map_err(|_| ApiError::not_found("organisation"))?;
    let snap = st.config.snapshot();
    Ok(RoutingAssignment {
        scheme: obj.doc.settings.routing_scheme.clone(),
        locked: obj.doc.settings.locked.iter().any(|l| l == "routing_scheme"),
        version: obj.version,
        writable: p.can_admin_org(org),
        own_assignments: own_assignments(&snap, org),
    })
}

async fn get_assignment(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<RoutingAssignment>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Organisation, &params, &snap)?;
    if !RoutingScheme::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    Ok(Json(assignment_of(&st, &p, scope.org()).await?))
}

/// Sets the organisation's scheme and whether it is locked. Like the organisation's other
/// settings, this is for its admins to decide.
async fn put_assignment(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<RoutingAssignmentInput>,
) -> ApiResult<Json<RoutingAssignment>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Organisation, &params, &snap)?;
    let org = scope.org();
    if !p.can_admin_org(org) {
        return Err(ApiError::forbidden());
    }
    if let Some(name) = &input.scheme
        && !snap.organisations.get(org).is_some_and(|o| o.schemes.contains_key(name))
    {
        return Err(ApiError::invalid(format!("the organisation has no routing scheme \"{name}\""))
            .with_details(serde_json::json!({ "code": "routing_scheme_unknown" })));
    }
    let mut obj: Obj<Organisation> = st.store.get_obj(org).await.map_err(|_| ApiError::not_found("organisation"))?;
    if input.version.is_some_and(|v| v != obj.version) {
        return Err(ApiError::conflict("the organisation was changed in the meantime; reload and try again")
            .with_details(serde_json::json!({ "code": "version_conflict" })));
    }
    let settings = &mut obj.doc.settings;
    settings.routing_scheme = input.scheme.clone();
    settings.locked.retain(|l| l != "routing_scheme");
    if input.locked {
        settings.locked.push("routing_scheme".into());
    }
    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(Some(org), None);
    audit.action = "assign_routing_scheme".into();
    tx.update(
        &obj.id,
        Some(obj.version),
        Some(&obj.doc.name),
        &serde_json::to_value(&obj.doc).map_err(ApiError::internal)?,
        audit,
    )
    .await?;
    st.commit(tx).await?;
    Ok(Json(assignment_of(&st, &p, org).await?))
}

async fn defaults(_p: Principal) -> Json<Vec<RoutingScheme>> {
    Json(default_routing_schemes())
}

pub fn routes(mut router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    router = register::<RoutingScheme>(router, api, Ops::ALL);
    let scheme = api.schema::<RoutingScheme>();
    api.add(
        api.op(
            "get",
            "/routing-scheme-defaults",
            "routingSchemeDefaults",
            "routing-schemes",
            "The built-in routing schemes an organisation can load",
        )
        .ok(serde_json::json!({ "type": "array", "items": scheme })),
    );
    let path = "/orgs/{org}/routing-assignment";
    let view = api.schema::<RoutingAssignment>();
    let input = api.schema::<RoutingAssignmentInput>();
    api.add(
        api.op(
            "get",
            path,
            "getRoutingAssignment",
            "routing-schemes",
            "The organisation's routing scheme and its lock",
        )
        .ok(view.clone()),
    );
    api.add(
        api.op(
            "put",
            path,
            "setRoutingAssignment",
            "routing-schemes",
            "Assign the organisation's routing scheme and lock it (organisation administrators)",
        )
        .body(input)
        .ok(view),
    );
    router.route("/routing-scheme-defaults", get(defaults)).route(path, get(get_assignment).put(put_assignment))
}
