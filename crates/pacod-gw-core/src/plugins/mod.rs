//! Plugins (`docs/plugin-concept.md`): the registry of the plugins compiled into this binary,
//! the bindings a snapshot compiles for each workspace, and the hook that runs them on a call.
//!
//! A plugin is Rust code behind a Cargo feature. Adding one is a release; switching it on is
//! configuration: a platform install, then a binding at an organisation or a workspace.
//! Nothing is loaded at run time (feature catalogue SEC-03).

pub mod host;
pub mod run;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

pub use pacod_gw_plugin as api;
use pacod_gw_plugin::{Bound, Catalogued, CompileCtx, Manifest, Plugin};
use parking_lot::RwLock;

use crate::model::settings::ResolvedSettings;
use crate::model::{Obj, OnError, OrgPluginBinding, PluginBinding, PluginInstall, PluginMode, WorkspacePluginBinding};

/// The plugins of this binary, by id. Filled with the built-in ones on first use.
static REGISTRY: LazyLock<RwLock<Vec<Arc<dyn Plugin>>>> = LazyLock::new(|| RwLock::new(built_in()));

/// The plugins compiled in, one per enabled Cargo feature.
fn built_in() -> Vec<Arc<dyn Plugin>> {
    Vec::new()
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
/// Embeddings carry no conversation.
pub fn proxy_dialect(d: crate::extract::Dialect) -> Option<api::Dialect> {
    use crate::extract::Dialect as D;
    match d {
        D::OpenAiChat => Some(api::Dialect::OpenAiChat),
        D::OpenAiResponses => Some(api::Dialect::OpenAiResponses),
        D::Anthropic => Some(api::Dialect::Anthropic),
        D::Gemini => Some(api::Dialect::Gemini),
        D::Ollama => Some(api::Dialect::Ollama),
        D::OpenAiEmbeddings | D::GeminiEmbed => None,
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

/// The bindings that apply to one workspace, compiled: its own, else its organisation's,
/// unless the organisation locked its binding. Bindings that are off, or whose plugin the
/// platform has not installed, are left out; the latter are reported in `problems`.
#[allow(clippy::too_many_arguments)]
pub fn bind_workspace(
    installs: &HashMap<String, PluginInstall>,
    org: &[Obj<OrgPluginBinding>],
    own: &[Obj<WorkspacePluginBinding>],
    organisation_id: &str,
    workspace_id: &str,
    settings: &ResolvedSettings,
    catalogue: &CatalogueLookup<'_>,
    problems: &mut BTreeSet<String>,
) -> Vec<Arc<ActiveBinding>> {
    let mut out = Vec::new();
    let ids: BTreeSet<&str> =
        org.iter().map(|b| b.doc.0.plugin.as_str()).chain(own.iter().map(|b| b.doc.0.plugin.as_str())).collect();
    for plugin_id in ids {
        let from_org = org.iter().find(|b| b.doc.0.plugin == plugin_id).map(|b| (b.id.as_str(), &b.doc.0));
        let from_ws = own.iter().find(|b| b.doc.0.plugin == plugin_id).map(|b| (b.id.as_str(), &b.doc.0));
        let effective = match (from_org, from_ws) {
            (Some(o), _) if o.1.locked => o,
            (_, Some(w)) => w,
            (Some(o), None) => o,
            (None, None) => continue,
        };
        let (id, b) = effective;
        if b.mode == PluginMode::Off {
            continue;
        }
        let Some(install) = installs.get(plugin_id).filter(|i| i.enabled) else {
            problems.insert(format!("plugin binding {id} names the plugin \"{plugin_id}\", which is not installed"));
            continue;
        };
        out.push(Arc::new(compile_one(id, b, install, organisation_id, workspace_id, settings, catalogue)));
    }
    out.sort_by_key(|b| b.priority);
    out
}

fn compile_one(
    id: &str,
    b: &PluginBinding,
    install: &PluginInstall,
    organisation_id: &str,
    workspace_id: &str,
    settings: &ResolvedSettings,
    catalogue: &CatalogueLookup<'_>,
) -> ActiveBinding {
    let plugin = find(&b.plugin);
    let manifest = plugin.as_ref().map(|p| p.manifest());
    let default_ms = plugin.as_ref().map_or(2_000, |p| p.default_timeout_ms(&b.settings));
    let bound = match &plugin {
        None => Err(format!("the plugin \"{}\" is not built into this gateway", b.plugin)),
        Some(p) => {
            check_engines(p.as_ref(), &b.settings, install, settings.plugin_third_parties.as_deref()).and_then(|()| {
                let named = b.settings.get("catalogue").and_then(|v| v.as_str()).unwrap_or_default();
                let ctx = CompileCtx {
                    settings: &b.settings,
                    catalogue: (!named.is_empty()).then(|| catalogue(named)).flatten(),
                    organisation_id,
                    workspace_id,
                };
                p.compile(&ctx).map_err(|problems| problems.join("; "))
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
    }
}

/// Checks that a binding's settings compile, for the management API: what would fail on the
/// request path is refused when the binding is written.
pub fn try_compile(
    plugin: &dyn Plugin,
    settings: &serde_json::Value,
    organisation_id: &str,
    workspace_id: &str,
    catalogue: &CatalogueLookup<'_>,
) -> Result<(), Vec<String>> {
    let named = settings.get("catalogue").and_then(|v| v.as_str()).unwrap_or_default();
    let ctx = CompileCtx {
        settings,
        catalogue: (!named.is_empty()).then(|| catalogue(named)).flatten(),
        organisation_id,
        workspace_id,
    };
    plugin.compile(&ctx).map(|_| ())
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
