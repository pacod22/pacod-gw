//! Plugins: what this gateway offers, the platform's installs, and the bindings of
//! organisations and workspaces (`docs/plugin-concept.md`, section 4).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde::Serialize;

use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{BuildCtx, Ops, Resource, ScopeKind, register};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{Obj, OrgPluginBinding, PluginBinding, PluginInstall, PluginMode, WorkspacePluginBinding};
use crate::plugins::api::Egress;
use crate::snapshot::Snapshot;

/// One engine of a plugin.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EngineInfo {
    pub id: String,
    /// Where the engine sends the text it looks at: `none`, `provider` (a model the workspace
    /// may use) or `third_party`.
    pub egress: String,
    /// For `third_party`: the name the platform approves on the install, for example
    /// `typesafe`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub third_party: Option<String>,
}

/// A plugin compiled into this gateway.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PluginInfo {
    pub id: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub engines: Vec<EngineInfo>,
    /// Whether the platform installed it, and whether that install is enabled.
    pub installed: bool,
    pub enabled: bool,
}

/// The plugins compiled into this gateway, and whether the platform installed them.
async fn list(State(st): State<AdminState>, _p: Principal) -> ApiResult<Json<Vec<PluginInfo>>> {
    let snap = st.config.snapshot();
    let mut out: Vec<PluginInfo> = crate::plugins::registry()
        .iter()
        .map(|p| {
            let m = p.manifest();
            let install = snap.plugin_installs.get(m.id);
            PluginInfo {
                id: m.id.into(),
                version: m.version.into(),
                title: m.title.into(),
                description: m.description.into(),
                engines: m
                    .engines
                    .iter()
                    .map(|e| {
                        let (egress, third_party) = match e.egress {
                            Egress::None => ("none", None),
                            Egress::Provider => ("provider", None),
                            Egress::ThirdParty(t) => ("third_party", Some(t.to_owned())),
                        };
                        EngineInfo { id: e.id.into(), egress: egress.into(), third_party }
                    })
                    .collect(),
                installed: install.is_some(),
                enabled: install.is_some_and(|i| i.enabled),
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Json(out))
}

fn compiled(id: &str) -> ApiResult<std::sync::Arc<dyn crate::plugins::api::Plugin>> {
    crate::plugins::find(id).ok_or_else(|| {
        ApiError::invalid(format!("no plugin \"{id}\" is built into this gateway: GET /plugins lists those that are"))
    })
}

impl Resource for PluginInstall {
    const PLURAL: &'static str = "plugin-installs";
    const LABEL: &'static str = "plugin install";
    const SCOPE: ScopeKind = ScopeKind::Global;
    type Input = PluginInstall;
    type View = PluginInstall;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.plugin.clone())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, _ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if let Some(e) = existing
            && e.doc.plugin != input.plugin
        {
            return Err(ApiError::invalid("an install's plugin cannot change: install the other plugin instead"));
        }
        let manifest = compiled(&input.plugin)?.manifest();
        for e in &input.engines {
            if !manifest.engines.iter().any(|x| x.id == e) {
                return Err(ApiError::invalid(format!("the plugin \"{}\" has no engine \"{e}\"", input.plugin)));
            }
        }
        for t in &input.third_parties {
            if !manifest.engines.iter().any(|x| matches!(x.egress, Egress::ThirdParty(name) if name == t)) {
                return Err(ApiError::invalid(format!(
                    "no engine of the plugin \"{}\" sends text to \"{t}\"",
                    input.plugin
                )));
            }
        }
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }
}

/// The checks every binding passes. `third_parties` is what the scope allows, `None` for what
/// the platform approved.
fn check_binding(
    b: &PluginBinding,
    snap: &Snapshot,
    organisation_id: &str,
    workspace_id: &str,
    third_parties: Option<&[String]>,
) -> ApiResult<()> {
    let plugin = compiled(&b.plugin)?;
    if b.timeout_ms.is_some_and(|t| !(50..=60_000).contains(&t)) {
        return Err(ApiError::invalid("timeout_ms must be between 50 and 60000"));
    }
    if !b.settings.is_object() && !b.settings.is_null() {
        return Err(ApiError::invalid("settings must be an object"));
    }
    if b.mode == PluginMode::Off {
        return Ok(());
    }
    let install = snap
        .plugin_installs
        .get(&b.plugin)
        .filter(|i| i.enabled)
        .ok_or_else(|| ApiError::conflict(format!("the platform has not installed the plugin \"{}\"", b.plugin)))?;
    crate::plugins::check_engines(plugin.as_ref(), &b.settings, install, third_parties).map_err(ApiError::invalid)?;
    crate::plugins::try_compile(plugin.as_ref(), &b.settings, organisation_id, workspace_id, &|_| None).map_err(
        |problems| {
            ApiError::invalid(format!("settings: {}", problems.join("; ")))
                .with_details(serde_json::json!({ "problems": problems }))
        },
    )
}

impl Resource for OrgPluginBinding {
    const PLURAL: &'static str = "plugin-bindings";
    const LABEL: &'static str = "plugin binding";
    const SCOPE: ScopeKind = ScopeKind::Organisation;
    type Input = PluginBinding;
    type View = PluginBinding;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.0.plugin.clone())
    }

    fn build(input: Self::Input, _existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        let org = ctx.snap.organisations.get(ctx.scope.org()).ok_or_else(|| ApiError::not_found("organisation"))?;
        check_binding(&input, ctx.snap, &org.id, "", org.doc.settings.plugin_third_parties.as_deref())?;
        Ok(OrgPluginBinding(input))
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.0.clone()
    }
}

impl Resource for WorkspacePluginBinding {
    const PLURAL: &'static str = "plugin-bindings";
    const LABEL: &'static str = "plugin binding";
    const SCOPE: ScopeKind = ScopeKind::Workspace;
    type Input = PluginBinding;
    type View = PluginBinding;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.0.plugin.clone())
    }

    fn build(input: Self::Input, _existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if input.locked {
            return Err(ApiError::invalid("locked belongs to an organisation's binding: it binds its workspaces"));
        }
        let ws = ctx.snap.workspaces.get(ctx.scope.ws()).ok_or_else(|| ApiError::not_found("workspace"))?;
        if let Some(org) = ctx.snap.organisations.get(&ws.organisation_id)
            && org.plugin_bindings.get(&input.plugin).is_some_and(|b| b.locked)
        {
            return Err(ApiError::conflict(format!(
                "the organisation has locked its binding of the plugin \"{}\": its workspaces follow it",
                input.plugin
            )));
        }
        check_binding(&input, ctx.snap, &ws.organisation_id, &ws.id, ws.settings.plugin_third_parties.as_deref())?;
        Ok(WorkspacePluginBinding(input))
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.0.clone()
    }
}

pub fn routes(mut router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let info = api.schema::<PluginInfo>();
    api.add(
        api.op("get", "/plugins", "listPlugins", "plugins", "The plugins compiled into this gateway")
            .ok(serde_json::json!({ "type": "array", "items": info })),
    );
    router = router.route("/plugins", get(list));
    router = register::<PluginInstall>(router, api, Ops::ALL);
    router = register::<OrgPluginBinding>(router, api, Ops::ALL);
    register::<WorkspacePluginBinding>(router, api, Ops::ALL)
}
