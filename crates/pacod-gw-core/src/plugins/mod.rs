//! Plugins (`docs/plugin-concept.md`): the registry of the plugins compiled into this binary,
//! the bindings a snapshot compiles for each workspace, and the hook that runs them on a call.
//!
//! A plugin is Rust code behind a Cargo feature. Adding one is a release; switching it on is
//! configuration: a platform install, then a binding at an organisation or a workspace.
//! Nothing is loaded at run time (feature catalogue SEC-03).

pub mod answers;
pub mod host;
pub mod run;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

pub use pacod_gw_plugin as api;
use pacod_gw_plugin::{Bound, Catalogued, CompileCtx, Hook, Manifest, ModelUse, Plugin};
use parking_lot::RwLock;

use crate::model::settings::ResolvedSettings;
use crate::model::{
    ModelKind, Obj, OnError, OrgGrant, OrgPluginBinding, PluginBinding, PluginInstall, PluginMode, WorkspaceGrant,
    WorkspacePluginBinding,
};
use crate::snapshot::{ModelRt, TemplateRt};

/// The plugins of this binary, by id. Filled with the built-in ones on first use.
static REGISTRY: LazyLock<RwLock<Vec<Arc<dyn Plugin>>>> = LazyLock::new(|| RwLock::new(built_in()));

/// The plugins compiled in, one per enabled Cargo feature.
fn built_in() -> Vec<Arc<dyn Plugin>> {
    let intent: Option<Arc<dyn Plugin>> = if cfg!(feature = "plugin-intent") {
        Some(Arc::new(pacod_gw_plugin_intent::IntentPlugin::new()))
    } else {
        None
    };
    let cache: Option<Arc<dyn Plugin>> =
        if cfg!(feature = "plugin-cache") { Some(Arc::new(pacod_gw_plugin_cache::CachePlugin::new())) } else { None };
    intent.into_iter().chain(cache).collect()
}

/// The hooks of a plugin, also when this binary lacks it: a binding of the response cache on an
/// instance built without it must not count as a binding that checks every call.
pub fn hooks_of(plugin_id: &str) -> &'static [Hook] {
    match find(plugin_id) {
        Some(p) => p.manifest().hooks,
        None if plugin_id == pacod_gw_plugin_cache::ID => &[Hook::Answers],
        None => &[Hook::OnRequest],
    }
}

/// The plugins this binary offers.
pub fn registry() -> Vec<Arc<dyn Plugin>> {
    REGISTRY.read().clone()
}

/// The compiled plugin with this id.
pub fn find(id: &str) -> Option<Arc<dyn Plugin>> {
    REGISTRY.read().iter().find(|p| p.manifest().id == id).cloned()
}

/// Adds a plugin, or replaces the one with the same id. The gateway's own plugins are there
/// from the start; this is for tests and for programs that embed the core.
pub fn register(plugin: Arc<dyn Plugin>) {
    let mut r = REGISTRY.write();
    let id = plugin.manifest().id;
    r.retain(|p| p.manifest().id != id);
    r.push(plugin);
}

/// What a plugin is shown of a call: who makes it, what the caller said about it, and its text.
#[allow(clippy::too_many_arguments)]
pub fn input(
    caller: &crate::dataplane::auth::Caller,
    params: &crate::dataplane::common::GatewayParams,
    interface: &str,
    provider: Option<&str>,
    path: &str,
    model: String,
    stream: bool,
    body: &[u8],
    dialect: api::Dialect,
) -> api::RequestInput {
    let declared = &caller.workspace.settings.tag_keys;
    api::RequestInput {
        call_id: String::new(),
        organisation_id: caller.key.organisation_id.clone(),
        workspace_id: caller.key.workspace_id.clone(),
        key_id: caller.key.id.clone(),
        key_name: caller.key.doc.name.clone(),
        interface: interface.to_owned(),
        provider: provider.map(str::to_owned),
        path: path.to_owned(),
        model,
        stream,
        body_bytes: body.len(),
        params: api::CallParams {
            task: params.task.clone(),
            complexity: params.complexity.map(|c| c.name().to_owned()),
            agent_id: params.agent_id.clone(),
            user_id: params.user_id.clone(),
            end_user: params.end_user.clone(),
            session_id: params.session_id.clone(),
            tags: params
                .tags
                .iter()
                .filter(|(k, _)| declared.contains(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        },
        conversation: api::conversation(dialect, body),
    }
}

/// The dialect a proxied body is read in, for a path the gateway reads a dialect from.
/// Embeddings carry no conversation, and System One's questions are about a text the caller
/// chose, not a turn of a conversation.
pub fn proxy_dialect(d: crate::extract::Dialect) -> Option<api::Dialect> {
    use crate::extract::Dialect as D;
    match d {
        D::OpenAiChat => Some(api::Dialect::OpenAiChat),
        D::OpenAiResponses => Some(api::Dialect::OpenAiResponses),
        D::Anthropic => Some(api::Dialect::Anthropic),
        D::Gemini => Some(api::Dialect::Gemini),
        D::Ollama => Some(api::Dialect::Ollama),
        D::OpenAiEmbeddings | D::GeminiEmbed | D::SystemOne | D::OpenAiImages | D::OpenAiTranscription | D::Rerank => {
            None
        }
    }
}

/// A binding as one workspace has it, compiled.
pub struct ActiveBinding {
    /// The binding object's id.
    pub id: String,
    pub plugin: String,
    pub mode: PluginMode,
    pub priority: u32,
    pub timeout: Duration,
    pub on_error: OnError,
    pub expose: bool,
    /// What answers calls, or why nothing can: the plugin is not built into this binary, its
    /// engine's third party is not approved here, or its settings no longer compile.
    pub bound: Result<Arc<dyn Bound>, String>,
    pub manifest: Option<&'static Manifest>,
    /// The binding's settings, as stored: the console's try panel compiles variants of them.
    pub settings: serde_json::Value,
    /// Where in a call the plugin works.
    pub hooks: &'static [Hook],
}

impl ActiveBinding {
    pub fn has(&self, hook: Hook) -> bool {
        self.hooks.contains(&hook)
    }
}

impl std::fmt::Debug for ActiveBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveBinding")
            .field("id", &self.id)
            .field("plugin", &self.plugin)
            .field("mode", &self.mode)
            .field("ready", &self.bound.is_ok())
            .finish_non_exhaustive()
    }
}

/// Looks up a catalogue by the name a binding's settings give.
pub type CatalogueLookup<'a> = dyn Fn(&str) -> Option<Catalogued<'a>> + 'a;

/// Where a binding is compiled: the organisation and workspace, the catalogues they hold, and
/// the models they may use.
pub struct Scope<'a> {
    pub organisation_id: &'a str,
    /// Empty for an organisation's binding checked on its own.
    pub workspace_id: &'a str,
    pub catalogue: &'a CatalogueLookup<'a>,
    pub models: ModelScope<'a>,
}

impl Scope<'_> {
    fn ctx<'b>(&'b self, settings: &'b serde_json::Value, check: &'b CheckModel<'b>) -> CompileCtx<'b> {
        let named = settings.get("catalogue").and_then(|v| v.as_str()).unwrap_or_default();
        CompileCtx {
            settings,
            catalogue: (!named.is_empty()).then(|| (self.catalogue)(named)).flatten(),
            organisation_id: self.organisation_id,
            workspace_id: self.workspace_id,
            check_model: check,
        }
    }
}

type CheckModel<'a> = dyn Fn(&str, ModelUse) -> Result<(), String> + 'a;

/// The models a scope may use, for the plugins that call one.
#[derive(Clone, Copy)]
pub struct ModelScope<'a> {
    /// The repository's models, by object id, and their ids by name.
    pub models: &'a HashMap<String, Arc<ModelRt>>,
    pub names: &'a HashMap<String, String>,
    /// The organisation's grants, by model object id.
    pub org_grants: &'a HashMap<String, OrgGrant>,
    /// The workspace's own grants; `None` when it has none (all of its organisation's).
    pub ws_grants: Option<&'a HashMap<String, WorkspaceGrant>>,
    /// The workspace's templates; `None` at an organisation, whose workspaces each hold their
    /// own and are checked when the binding is compiled for them.
    pub templates: Option<&'a HashMap<String, Arc<TemplateRt>>>,
}

impl ModelScope<'_> {
    /// Whether a model (by its id) or a `@template` may be used for `purpose`, and why not.
    pub fn check(&self, name: &str, purpose: ModelUse) -> Result<(), String> {
        if let Some(t) = name.strip_prefix('@') {
            return match (purpose, self.templates) {
                (ModelUse::Decision, _) => Err(format!("\"{name}\": a decision model is named by its id")),
                (ModelUse::StructuredChat, Some(ts)) if !ts.contains_key(t) => {
                    Err(format!("the workspace has no template \"{t}\""))
                }
                (ModelUse::StructuredChat, _) => Ok(()),
            };
        }
        let m = self
            .names
            .get(name)
            .and_then(|id| self.models.get(id))
            .ok_or_else(|| format!("no model \"{name}\" is in the repository"))?;
        if !self.org_grants.contains_key(&m.id) || self.ws_grants.is_some_and(|g| !g.contains_key(&m.id)) {
            return Err(format!("the model \"{name}\" is not granted here"));
        }
        match purpose {
            ModelUse::StructuredChat if m.doc.kind != ModelKind::Chat => {
                Err(format!("the model \"{name}\" is not a chat model"))
            }
            ModelUse::StructuredChat if !m.doc.capabilities.structured_output => {
                Err(format!("the model \"{name}\" does not offer JSON-schema output (capabilities.structured_output)"))
            }
            ModelUse::StructuredChat => Ok(()),
            ModelUse::Decision if m.doc.kind != ModelKind::Decision => {
                Err(format!("the model \"{name}\" is not a decision model (kind decision)"))
            }
            ModelUse::Decision => Ok(()),
        }
    }
}

/// The id of the platform's binding of a plugin: its install's default binding.
pub fn default_binding_id(plugin_id: &str) -> String {
    format!("platform:{plugin_id}")
}

/// The bindings that apply to one workspace, compiled: its own, else its organisation's,
/// unless the organisation locked its binding, else the platform's (the install's default
/// binding). Bindings that are off, or whose plugin the platform has not installed, are left
/// out; the latter are reported in `problems`.
pub fn bind_workspace(
    installs: &HashMap<String, PluginInstall>,
    org: &[Obj<OrgPluginBinding>],
    own: &[Obj<WorkspacePluginBinding>],
    scope: &Scope<'_>,
    settings: &ResolvedSettings,
    problems: &mut BTreeSet<String>,
) -> Vec<Arc<ActiveBinding>> {
    let mut out = Vec::new();
    let defaults: Vec<(String, &PluginBinding)> = installs
        .values()
        .filter(|i| i.enabled)
        .filter_map(|i| i.default_binding.as_ref().filter(|b| b.plugin == i.plugin))
        // An instance built without the plugin leaves the platform's binding out quietly,
        // rather than reporting it for every workspace.
        .filter(|b| find(&b.plugin).is_some())
        .map(|b| (default_binding_id(&b.plugin), b))
        .collect();
    let ids: BTreeSet<&str> = org
        .iter()
        .map(|b| b.doc.0.plugin.as_str())
        .chain(own.iter().map(|b| b.doc.0.plugin.as_str()))
        .chain(defaults.iter().map(|(_, b)| b.plugin.as_str()))
        .collect();
    for plugin_id in ids {
        let from_org = org.iter().find(|b| b.doc.0.plugin == plugin_id).map(|b| (b.id.as_str(), &b.doc.0));
        let from_ws = own.iter().find(|b| b.doc.0.plugin == plugin_id).map(|b| (b.id.as_str(), &b.doc.0));
        let from_platform = defaults.iter().find(|(_, b)| b.plugin == plugin_id).map(|(id, b)| (id.as_str(), *b));
        let effective = match (from_org, from_ws, from_platform) {
            (Some(o), _, _) if o.1.locked => o,
            (_, Some(w), _) => w,
            (Some(o), None, _) => o,
            (None, None, Some(p)) => p,
            (None, None, None) => continue,
        };
        let (id, b) = effective;
        if b.mode == PluginMode::Off {
            continue;
        }
        let Some(install) = installs.get(plugin_id).filter(|i| i.enabled) else {
            problems.insert(format!("plugin binding {id} names the plugin \"{plugin_id}\", which is not installed"));
            continue;
        };
        let active = compile_one(id, b, install, scope, settings);
        if let Err(why) = &active.bound {
            problems.insert(format!("plugin binding {id} in workspace {}: {why}", scope.workspace_id));
        }
        out.push(Arc::new(active));
    }
    out.sort_by_key(|b| b.priority);
    out
}

fn compile_one(
    id: &str,
    b: &PluginBinding,
    install: &PluginInstall,
    scope: &Scope<'_>,
    settings: &ResolvedSettings,
) -> ActiveBinding {
    let plugin = find(&b.plugin);
    let manifest = plugin.as_ref().map(|p| p.manifest());
    let default_ms = plugin.as_ref().map_or(2_000, |p| p.default_timeout_ms(&b.settings));
    let bound = match &plugin {
        None => Err(format!("the plugin \"{}\" is not built into this gateway", b.plugin)),
        Some(p) => {
            check_engines(p.as_ref(), &b.settings, install, settings.plugin_third_parties.as_deref()).and_then(|()| {
                let check = |name: &str, purpose: ModelUse| scope.models.check(name, purpose);
                p.compile(&scope.ctx(&b.settings, &check)).map_err(|problems| problems.join("; "))
            })
        }
    };
    ActiveBinding {
        id: id.to_owned(),
        plugin: b.plugin.clone(),
        mode: b.mode,
        priority: b.priority,
        timeout: Duration::from_millis(u64::from(b.timeout_ms.unwrap_or(default_ms).clamp(50, 60_000))),
        on_error: b.on_error,
        expose: b.expose,
        bound,
        manifest,
        settings: b.settings.clone(),
        hooks: hooks_of(&b.plugin),
    }
}

/// Checks that a binding's settings compile, for the management API: what would fail on the
/// request path is refused when the binding is written.
pub fn try_compile(plugin: &dyn Plugin, settings: &serde_json::Value, scope: &Scope<'_>) -> Result<(), Vec<String>> {
    compile(plugin, settings, scope).map(|_| ())
}

/// Compiles a binding's settings in a scope.
pub fn compile(
    plugin: &dyn Plugin,
    settings: &serde_json::Value,
    scope: &Scope<'_>,
) -> Result<Arc<dyn Bound>, Vec<String>> {
    let check = |name: &str, purpose: ModelUse| scope.models.check(name, purpose);
    plugin.compile(&scope.ctx(settings, &check))
}

impl<'a> Scope<'a> {
    /// The scope of a workspace in a snapshot: its catalogues and its organisation's, the
    /// models it may use, its templates.
    pub fn of_workspace(
        snap: &'a crate::snapshot::Snapshot,
        org: &'a crate::snapshot::OrgRt,
        ws: &'a crate::snapshot::WorkspaceRt,
        lookup: &'a CatalogueLookup<'a>,
    ) -> Self {
        Scope {
            organisation_id: &org.id,
            workspace_id: &ws.id,
            catalogue: lookup,
            models: ModelScope {
                models: &snap.models,
                names: &snap.model_names,
                org_grants: &org.grants,
                ws_grants: ws.grants.as_ref(),
                templates: Some(&ws.templates),
            },
        }
    }
}

/// Whether the engines a binding's settings use are allowed: by the install, and, for those
/// that send text to a third party, by the platform's approval and the scope's setting.
pub fn check_engines(
    plugin: &dyn Plugin,
    settings: &serde_json::Value,
    install: &PluginInstall,
    allowed_parties: Option<&[String]>,
) -> Result<(), String> {
    let manifest = plugin.manifest();
    for engine in plugin.engines(settings) {
        if !install.engines.is_empty() && !install.engines.iter().any(|e| e == engine) {
            return Err(format!(
                "the platform does not allow the engine \"{engine}\" of the plugin \"{}\"",
                manifest.id
            ));
        }
        if let Some(party) = manifest.third_party(engine) {
            if !install.third_parties.iter().any(|t| t == party) {
                return Err(format!(
                    "the engine \"{engine}\" sends text to {party}, which the platform has not approved for the plugin \"{}\"",
                    manifest.id
                ));
            }
            if allowed_parties.is_some_and(|allowed| !allowed.iter().any(|t| t == party)) {
                return Err(format!(
                    "the engine \"{engine}\" sends text to {party}, which this organisation does not allow"
                ));
            }
        }
    }
    Ok(())
}
