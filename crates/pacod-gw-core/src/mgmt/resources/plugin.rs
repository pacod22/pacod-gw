//! Plugins: what this gateway offers, the platform's installs, and the bindings of
//! organisations and workspaces (`docs/plugin-concept.md`, section 4).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde::Serialize;

use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{BuildCtx, Ops, Params, Resource, ScopeKind, collection_path, register, resolve_scope};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{
    Catalogue, Doc, ModelState, Obj, OnError, OrgIntentCatalogue, OrgPluginBinding, PluginBinding, PluginInstall,
    PluginMode, WorkspaceIntentCatalogue, WorkspacePluginBinding,
};
use crate::plugins::api::{Egress, ModelUse};
use crate::snapshot::{CatalogueRt, ModelRt, OrgRt, Snapshot, WorkspaceRt};
use crate::store::{ParentFilter, Tx};

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
    /// Where in a call it works: `on_request` (it reads calls before they are planned, and may
    /// block them) or `answers` (it answers calls from kept answers).
    pub hooks: Vec<String>,
    /// The gateway installed it on its first start, with a binding for every workspace.
    pub installed_by_default: bool,
    /// The mode of the platform's binding (the install's default binding), which applies to
    /// every workspace whose organisation and own bind the plugin otherwise not. `None`
    /// without one, or while the install is switched off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_mode: Option<PluginMode>,
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
                hooks: m.hooks.iter().map(|h| h.name().to_owned()).collect(),
                installed_by_default: m.installed_by_default,
                platform_mode: install.filter(|i| i.enabled).and_then(|i| i.default_binding.as_ref()).map(|b| b.mode),
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

    fn build(mut input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if let Some(e) = existing
            && e.doc.plugin != input.plugin
        {
            return Err(ApiError::invalid("an install's plugin cannot change: install the other plugin instead"));
        }
        let plugin = compiled(&input.plugin)?;
        let manifest = plugin.manifest();
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
        if let Some(b) = input.default_binding.as_mut() {
            check_default_binding(plugin.as_ref(), b, &input.plugin, ctx.snap)?;
        }
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        let mut install = obj.doc.clone();
        install.default_binding = install.default_binding.map(PluginBinding::tagged);
        install
    }
}

/// The checks every binding passes. `third_parties` is what the scope allows, `None` for what
/// the platform approved.
fn check_binding(
    b: &PluginBinding,
    snap: &Snapshot,
    scope: &crate::plugins::Scope<'_>,
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
    crate::plugins::try_compile(plugin.as_ref(), &b.settings, scope).map_err(|problems| {
        ApiError::invalid(format!("settings: {}", problems.join("; ")))
            .with_details(serde_json::json!({ "problems": problems }))
    })
}

/// The platform's binding is checked on its own: it names nothing of an organisation or a
/// workspace, so a plugin whose settings name a catalogue or a model cannot have one.
fn check_default_binding(
    plugin: &dyn crate::plugins::api::Plugin,
    b: &mut PluginBinding,
    install_of: &str,
    snap: &Snapshot,
) -> ApiResult<()> {
    if b.plugin.is_empty() {
        b.plugin = install_of.to_owned();
    }
    if b.plugin != install_of {
        return Err(ApiError::invalid("default_binding.plugin must be the install's plugin"));
    }
    b.untag_settings().map_err(|e| ApiError::invalid(format!("default_binding.{e}")))?;
    if b.locked {
        return Err(ApiError::invalid("default_binding.locked has no meaning: organisations bind their own"));
    }
    if b.timeout_ms.is_some_and(|t| !(50..=60_000).contains(&t)) {
        return Err(ApiError::invalid("default_binding.timeout_ms must be between 50 and 60000"));
    }
    if !b.settings.is_object() && !b.settings.is_null() {
        return Err(ApiError::invalid("default_binding.settings must be an object"));
    }
    if b.mode == PluginMode::Off {
        return Ok(());
    }
    let empty = std::collections::HashMap::new();
    let lookup = |_: &str| None;
    let scope = crate::plugins::Scope {
        organisation_id: "",
        workspace_id: "",
        catalogue: &lookup,
        models: crate::plugins::ModelScope {
            models: &snap.models,
            names: &snap.model_names,
            org_grants: &empty,
            ws_grants: None,
            templates: None,
        },
    };
    crate::plugins::try_compile(plugin, &b.settings, &scope).map_err(|problems| {
        ApiError::invalid(format!("default_binding.settings: {}", problems.join("; ")))
            .with_details(serde_json::json!({ "problems": problems }))
    })
}

/// An organisation's binding is checked against the organisation alone: templates belong to
/// workspaces and are checked when the binding is compiled for each of them.
fn org_scope<'a>(
    snap: &'a Snapshot,
    org: &'a crate::snapshot::OrgRt,
    lookup: &'a crate::plugins::CatalogueLookup<'a>,
) -> crate::plugins::Scope<'a> {
    crate::plugins::Scope {
        organisation_id: &org.id,
        workspace_id: "",
        catalogue: lookup,
        models: crate::plugins::ModelScope {
            models: &snap.models,
            names: &snap.model_names,
            org_grants: &org.grants,
            ws_grants: None,
            templates: None,
        },
    }
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

    fn build(mut input: Self::Input, _existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        input.untag_settings().map_err(ApiError::invalid)?;
        let org = ctx.snap.organisations.get(ctx.scope.org()).ok_or_else(|| ApiError::not_found("organisation"))?;
        let lookup = |name: &str| org.catalogues.get(name).map(crate::snapshot::CatalogueRt::view);
        let scope = org_scope(ctx.snap, org, &lookup);
        check_binding(&input, ctx.snap, &scope, org.doc.settings.plugin_third_parties.as_deref())?;
        Ok(OrgPluginBinding(input))
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.0.clone().tagged()
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

    fn build(mut input: Self::Input, _existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        input.untag_settings().map_err(ApiError::invalid)?;
        if input.locked {
            return Err(ApiError::invalid("locked belongs to an organisation's binding: it binds its workspaces"));
        }
        let ws = ctx.snap.workspaces.get(ctx.scope.ws()).ok_or_else(|| ApiError::not_found("workspace"))?;
        let org = ctx.snap.organisations.get(&ws.organisation_id).ok_or_else(|| ApiError::not_found("organisation"))?;
        if org.plugin_bindings.get(&input.plugin).is_some_and(|b| b.locked) {
            return Err(ApiError::conflict(format!(
                "the organisation has locked its binding of the plugin \"{}\": its workspaces follow it",
                input.plugin
            )));
        }
        let lookup = |name: &str| ws.catalogue(org, name).map(crate::snapshot::CatalogueRt::view);
        let scope = crate::plugins::Scope::of_workspace(ctx.snap, org, ws, &lookup);
        check_binding(&input, ctx.snap, &scope, ws.settings.plugin_third_parties.as_deref())?;
        Ok(WorkspacePluginBinding(input))
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.0.clone().tagged()
    }
}

fn check_catalogue(c: &Catalogue) -> ApiResult<()> {
    let problems = c.problems();
    if problems.is_empty() {
        return Ok(());
    }
    Err(ApiError::invalid(problems.join("; ")).with_details(serde_json::json!({ "problems": problems })))
}

/// The bindings, of a scope and below it, whose settings name a catalogue.
async fn bindings_naming(tx: &mut Tx, name: &str, org: &str, ws: Option<&str>) -> ApiResult<Vec<String>> {
    let names = |rows: Vec<crate::store::Row>| -> Vec<(Option<String>, String)> {
        rows.into_iter()
            .filter(|r| r.doc.pointer("/settings/catalogue").and_then(|v| v.as_str()) == Some(name))
            .map(|r| (r.parent, r.id))
            .collect()
    };
    let mut out = Vec::new();
    match ws {
        // A workspace's catalogue serves its own binding, and an organisation's binding there.
        Some(ws) => {
            out.extend(names(tx.list(WorkspacePluginBinding::KIND, Some(ws)).await?).into_iter().map(|(_, id)| id));
        }
        // An organisation's catalogue serves its binding and its workspaces'.
        None => {
            out.extend(names(tx.list(OrgPluginBinding::KIND, Some(org)).await?).into_iter().map(|(_, id)| id));
            let workspaces: Vec<String> =
                tx.list(crate::model::Workspace::KIND, Some(org)).await?.into_iter().map(|r| r.id).collect();
            for (parent, id) in names(tx.list(WorkspacePluginBinding::KIND, None).await?) {
                if parent.is_some_and(|p| workspaces.contains(&p)) {
                    out.push(id);
                }
            }
        }
    }
    Ok(out)
}

fn in_use(bindings: Vec<String>) -> ApiResult<()> {
    if bindings.is_empty() {
        return Ok(());
    }
    Err(ApiError::conflict(format!("plugin bindings name this catalogue: {}", bindings.join(", ")))
        .with_details(serde_json::json!({ "bindings": bindings })))
}

impl Resource for OrgIntentCatalogue {
    const PLURAL: &'static str = "intent-catalogues";
    const LABEL: &'static str = "intent catalogue";
    const SCOPE: ScopeKind = ScopeKind::Organisation;
    const STRICT: bool = true;
    type Input = Catalogue;
    type View = Catalogue;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.0.name.clone())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, _ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        check_catalogue(&input)?;
        if let Some(e) = existing
            && e.doc.0.name != input.name
        {
            return Err(ApiError::invalid("a catalogue's name cannot change: bindings name it"));
        }
        Ok(OrgIntentCatalogue(input))
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.0.clone()
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let org = obj.parent.clone().unwrap_or_default();
        in_use(bindings_naming(tx, &obj.doc.0.name, &org, None).await?)
    }
}

impl Resource for WorkspaceIntentCatalogue {
    const PLURAL: &'static str = "intent-catalogues";
    const LABEL: &'static str = "intent catalogue";
    const SCOPE: ScopeKind = ScopeKind::Workspace;
    const STRICT: bool = true;
    type Input = Catalogue;
    type View = Catalogue;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.0.name.clone())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, _ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        check_catalogue(&input)?;
        if let Some(e) = existing
            && e.doc.0.name != input.name
        {
            return Err(ApiError::invalid("a catalogue's name cannot change: bindings name it"));
        }
        Ok(WorkspaceIntentCatalogue(input))
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.0.clone()
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let ws = obj.parent.clone().unwrap_or_default();
        in_use(bindings_naming(tx, &obj.doc.0.name, "", Some(&ws)).await?)
    }
}

/// An intent binding to start from in an organisation or a workspace, and what keeps it from
/// being created there as it is.
#[derive(Debug, Serialize, JsonSchema)]
pub struct BindingTemplate {
    pub binding: PluginBinding,
    /// Why the binding cannot be created here as it is, from the checks a creation runs.
    /// Empty: it can.
    pub problems: Vec<String>,
    /// At a workspace: the mode of the organisation's binding of the plugin, which a binding of
    /// the workspace's own replaces there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaces: Option<PluginMode>,
}

/// The models of a scope that can classify, cheapest first: classification runs on every call.
fn classifiers<'a>(snap: &'a Snapshot, scope: &crate::plugins::Scope<'_>) -> Vec<&'a ModelRt> {
    let now = chrono::Utc::now();
    let cost = |m: &ModelRt| {
        if m.doc.free {
            return 0.0;
        }
        snap.price_at(&m.id, now).and_then(|p| p.rates.input).unwrap_or(f64::INFINITY)
    };
    let mut out: Vec<&ModelRt> = snap
        .models
        .values()
        .map(|m| m.as_ref())
        .filter(|m| m.doc.state == ModelState::Active)
        .filter(|m| scope.models.check(&m.doc.name, ModelUse::StructuredChat).is_ok())
        .collect();
    out.sort_by(|a, b| cost(a).total_cmp(&cost(b)).then_with(|| a.doc.name.cmp(&b.doc.name)));
    out
}

/// The template's binding: shadow mode, the built-in catalogue if the scope has it (else its
/// first), and the cheapest model that can classify, or Jev when no such model is there.
fn template_binding(
    snap: &Snapshot,
    org: &OrgRt,
    ws: Option<&WorkspaceRt>,
    scope: &crate::plugins::Scope<'_>,
) -> PluginBinding {
    let id = pacod_gw_plugin_intent::ID;
    let built_in = pacod_gw_plugin_intent::catalogue::template().name;
    let mut names: Vec<&String> =
        org.catalogues.keys().chain(ws.into_iter().flat_map(|w| w.catalogues.keys())).collect();
    names.sort();
    names.dedup();
    let catalogue =
        names.iter().find(|n| **n == &built_in).or(names.first()).map(|n| n.to_string()).unwrap_or_default();
    let install = snap.plugin_installs.get(id).filter(|i| i.enabled);
    let allows = |engine: &str| install.is_none_or(|i| i.engines.is_empty() || i.engines.iter().any(|e| e == engine));
    let jev = pacod_gw_plugin_intent::settings::JevSettings::default().model;
    let settings = match classifiers(snap, scope).first() {
        Some(m) if allows("llm") => {
            serde_json::json!({ "catalogue": catalogue, "engine": "llm", "llm": { "model": m.doc.name } })
        }
        _ if allows("jev") && scope.models.check(&jev, ModelUse::Decision).is_ok() => {
            serde_json::json!({ "catalogue": catalogue, "engine": "jev", "jev": { "model": jev } })
        }
        _ => serde_json::json!({ "catalogue": catalogue, "engine": "llm", "llm": { "model": "" } }),
    };
    PluginBinding {
        plugin: id.into(),
        mode: PluginMode::Shadow,
        priority: 100,
        timeout_ms: None,
        on_error: OnError::Closed,
        expose: false,
        locked: false,
        settings,
    }
}

async fn binding_template(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<BindingTemplate>> {
    let snap = st.config.snapshot();
    let kind = if params.contains_key("ws") { ScopeKind::Workspace } else { ScopeKind::Organisation };
    let ids = resolve_scope(kind, &params, &snap)?;
    let org = snap.organisations.get(ids.org()).ok_or_else(|| ApiError::not_found("organisation"))?;
    let ws = match kind {
        ScopeKind::Workspace => Some(snap.workspaces.get(ids.ws()).ok_or_else(|| ApiError::not_found("workspace"))?),
        _ => None,
    };
    // The template is for those who may create the binding.
    let allowed = match ws {
        Some(w) => p.can_admin_workspace(&w.id, &org.id),
        None => p.can_admin_org(&org.id),
    };
    if !allowed {
        return Err(ApiError::forbidden());
    }
    // The scope's own binding of the plugin, switched off ones included: a second is refused.
    let (kind_name, parent) = match ws {
        Some(w) => (WorkspacePluginBinding::KIND, w.id.clone()),
        None => (OrgPluginBinding::KIND, org.id.clone()),
    };
    let own = st.store.list(kind_name, ParentFilter::Is(&parent), None, 1000).await?;
    let has_own = own.iter().any(|r| r.doc.get("plugin").and_then(|v| v.as_str()) == Some(pacod_gw_plugin_intent::ID));
    let lookup = |name: &str| {
        match ws {
            Some(w) => w.catalogue(org, name),
            None => org.catalogues.get(name),
        }
        .map(CatalogueRt::view)
    };
    let scope = match ws {
        Some(w) => crate::plugins::Scope::of_workspace(&snap, org, w, &lookup),
        None => org_scope(&snap, org, &lookup),
    };
    let binding = template_binding(&snap, org, ws.map(|w| w.as_ref()), &scope);

    // What creating it would run into, in the order the creation checks.
    let mut problems = Vec::new();
    if has_own {
        problems.push(format!("there is already a binding of the plugin \"{}\" here: change that one", binding.plugin));
    }
    let from_org = org.plugin_bindings.get(&binding.plugin);
    if ws.is_some() && from_org.is_some_and(|b| b.locked) {
        problems.push(format!(
            "the organisation has locked its binding of the plugin \"{}\": its workspaces follow it",
            binding.plugin
        ));
    }
    let third_parties = match ws {
        Some(w) => w.settings.plugin_third_parties.as_deref(),
        None => org.doc.settings.plugin_third_parties.as_deref(),
    };
    if let Err(e) = check_binding(&binding, &snap, &scope, third_parties) {
        let listed = e.details.as_ref().and_then(|d| d.get("problems")).and_then(|v| v.as_array());
        match listed {
            Some(list) => problems.extend(list.iter().filter_map(|v| v.as_str()).map(String::from)),
            None => problems.push(e.message),
        }
    }
    let replaces = ws.and(from_org).map(|b| b.mode);
    Ok(Json(BindingTemplate { binding: binding.tagged(), problems, replaces }))
}

async fn catalogue_template(_p: Principal) -> Json<Catalogue> {
    Json(pacod_gw_plugin_intent::catalogue::template())
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
    router = register::<WorkspacePluginBinding>(router, api, Ops::ALL);
    router = register::<OrgIntentCatalogue>(router, api, Ops::ALL);
    router = register::<WorkspaceIntentCatalogue>(router, api, Ops::ALL);
    let catalogue = api.schema::<Catalogue>();
    api.add(
        api.op(
            "get",
            "/intent-catalogue-template",
            "intentCatalogueTemplate",
            "plugins",
            "The built-in intent catalogue that a new catalogue can start from",
        )
        .ok(catalogue),
    );
    router = router.route("/intent-catalogue-template", get(catalogue_template));
    let template = api.schema::<BindingTemplate>();
    for kind in [ScopeKind::Organisation, ScopeKind::Workspace] {
        let path = collection_path(kind, "plugin-binding-template");
        let (op, summary) = match kind {
            ScopeKind::Workspace => (
                "workspacePluginBindingTemplate",
                "An intent binding to start from in this workspace, and what keeps it from being created as it is",
            ),
            _ => (
                "orgPluginBindingTemplate",
                "An intent binding to start from in this organisation, and what keeps it from being created as it is",
            ),
        };
        api.add(api.op("get", &path, op, "plugins", summary).ok(template.clone()));
        router = router.route(&path, get(binding_template));
    }
    router
}
