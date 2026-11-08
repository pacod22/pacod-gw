//! Registered agents of a workspace (runbook section 21.3).

use axum::Router;
use serde_json::Value;

use crate::mgmt::AdminState;
use crate::mgmt::crud::{BuildCtx, Ops, Resource, ScopeKind, register};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{Agent, ApiKey, Doc, Obj, is_slug};
use crate::snapshot::Snapshot;
use crate::store::Tx;

impl Resource for Agent {
    const PLURAL: &'static str = "agents";
    const LABEL: &'static str = "agent";
    const SCOPE: ScopeKind = ScopeKind::Workspace;
    const STRICT: bool = true;
    type Input = Agent;
    type View = Agent;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if !is_slug(&input.name) || input.name.len() > 100 {
            return Err(ApiError::invalid("name must be lower-case letters, digits and hyphens, at most 100"));
        }
        if existing.is_some_and(|e| e.doc.name != input.name) {
            return Err(ApiError::invalid("an agent keeps its name: its calls carry it in every report"));
        }
        let bundles = ctx.snap.workspaces.get(ctx.scope.ws()).map(|w| &w.tool_bundles);
        if let Some(b) = input.tool_bundles.iter().find(|b| bundles.is_none_or(|all| !all.contains_key(*b))) {
            return Err(ApiError::invalid(format!("tool_bundles: the workspace has no active tool bundle \"{b}\"")));
        }
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }

    fn effects(obj: &Obj<Self>, _snap: &Snapshot) -> Vec<String> {
        vec![format!(
            "Calls that name {} are then taken for an unregistered agent's, where the workspace accepts those.",
            obj.doc.name
        )]
    }

    /// Keys speak for agents: one that a key names stays.
    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let keys: Vec<String> = tx
            .list(ApiKey::KIND, obj.parent.as_deref())
            .await?
            .into_iter()
            .filter(|r| {
                r.doc.get("agents").and_then(Value::as_array).is_some_and(|a| a.iter().any(|n| n == &obj.doc.name))
            })
            .filter_map(|r| r.name)
            .collect();
        if !keys.is_empty() {
            return Err(ApiError::conflict(format!("keys speak for this agent: {}", keys.join(", ")))
                .with_details(serde_json::json!({ "keys": keys })));
        }
        Ok(())
    }
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    register::<Agent>(router, api, Ops::ALL)
}
