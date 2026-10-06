//! Routing schemes of an organisation (concept section 7.5).

use axum::Router;

use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{BuildCtx, Ops, Resource, ScopeIds, ScopeKind, register};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{Doc, Obj, Organisation, RoutingScheme, Template, Workspace};
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

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    register::<RoutingScheme>(router, api, Ops::ALL)
}
