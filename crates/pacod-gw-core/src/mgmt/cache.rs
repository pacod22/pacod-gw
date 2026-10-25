//! The response cache in the management API (`docs/plugin-concept.md`, section 15): what its
//! store holds, which binding answers a workspace's calls, and clearing it.
//!
//! A clear is a configuration object (`ResponseCacheClear`), so that every instance learns of it
//! through the configuration it polls; answers asked for before it are misses from then on. The
//! space they take is freed besides: by this process for a store every instance shares, by each
//! gateway for its own memory (`answers::forget_cleared`).

use std::time::Duration;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::AdminState;
use super::auth::Principal;
use super::crud::{Params, ScopeKind, collection_path, resolve_scope};
use super::error::{ApiError, ApiResult};
use super::monitoring::{members, platform_reader};
use super::openapi::OpenApi;
use crate::answers::Forget;
use crate::cluster::report::AnswerFigures;
use crate::model::{
    Doc, Obj, OrgPluginBinding, PluginBinding, PluginInstall, PluginMode, ResponseCacheClear, WorkspacePluginBinding,
};
use crate::snapshot::{ClearRt, Snapshot};
use crate::store::ParentFilter;

const PLUGIN: &str = pacod_gw_plugin_cache::ID;
/// How long the status waits for a shared store.
const INSPECT_FOR: Duration = Duration::from_secs(1);
/// The clears the platform's status lists.
const RECENT_CLEARS: usize = 20;

#[derive(Debug, Serialize, JsonSchema)]
pub struct PlatformCacheView {
    /// Where the gateways keep answers: `valkey`, `memory`, `mixed` when they differ, or
    /// `unknown` when no gateway reported it.
    pub store: String,
    /// The shared store, when this instance reaches it.
    pub valkey: Option<ValkeyView>,
    /// Why there are no figures of a shared store.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valkey_note: Option<String>,
    pub instances: Vec<InstanceCache>,
    pub install: InstallView,
    /// The platform's own clear, which reaches every workspace.
    pub cleared: Option<ClearView>,
    /// The latest clears of every scope, newest first.
    pub recent_clears: Vec<ScopeClear>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ValkeyView {
    pub reachable: bool,
    /// Keys in its database, which holds the answers alone.
    pub keys: Option<u64>,
    pub used_memory_bytes: Option<u64>,
    /// 0 when no limit is set.
    pub max_memory_bytes: Option<u64>,
    pub eviction_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InstanceCache {
    pub id: String,
    pub role: String,
    pub hostname: String,
    /// False for an instance that serves only the management API: its store holds nothing.
    pub serves_calls: bool,
    /// Unset when the instance published no recent figures, or is of an older release.
    pub figures: Option<AnswerFigures>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InstallView {
    /// The install's object id.
    pub id: Option<String>,
    pub installed: bool,
    pub enabled: bool,
    /// The platform's binding, which every workspace without one of its own or its
    /// organisation's gets.
    pub default_binding: Option<PluginBinding>,
    pub version: Option<i32>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ClearView {
    pub at: DateTime<Utc>,
    /// The principal's id.
    pub by: String,
    pub by_kind: String,
    /// The user's email, when the principal is a known user.
    pub by_display: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ScopeClear {
    /// `platform`, `organisation` or `workspace`.
    pub scope: String,
    pub organisation_id: Option<String>,
    pub workspace_id: Option<String>,
    /// The organisation's or workspace's name.
    pub name: Option<String>,
    #[serde(flatten)]
    pub clear: ClearView,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ScopeCacheView {
    /// Where this instance keeps answers: `valkey` or `memory`.
    pub store: String,
    /// The binding that answers the scope's calls: for an organisation, the one its workspaces
    /// get unless they have their own.
    pub binding: Option<EffectiveBinding>,
    /// Why no binding answers calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why_none: Option<String>,
    /// Why the binding cannot answer calls although it is on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    pub cleared: ClearTimes,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EffectiveBinding {
    /// `workspace`, `organisation` or `platform`.
    pub source: String,
    pub binding_id: String,
    pub mode: PluginMode,
    /// The plugin's settings, tagged with `plugin`.
    pub settings: Value,
    pub timeout_ms: Option<u64>,
    /// Set by the organisation: its workspaces cannot have their own.
    pub locked: bool,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct ClearTimes {
    /// The scope's own clear.
    pub own: Option<ClearView>,
    /// The organisation's, for a workspace.
    pub organisation: Option<ClearView>,
    pub platform: Option<ClearView>,
    /// The latest of them: answers asked for before it are misses.
    pub effective: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ClearResult {
    pub scope: String,
    pub cleared_at: DateTime<Utc>,
    pub config_version: Option<i64>,
    /// How the space is freed: `started` here for the store every instance shares,
    /// `per_instance` by each gateway for its own memory, `not_available` when the gateways
    /// share a Valkey this instance does not reach (the answers then expire in their time).
    pub reclaim: String,
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let platform = api.schema::<PlatformCacheView>();
    let scope = api.schema::<ScopeCacheView>();
    let result = api.schema::<ClearResult>();
    let org = collection_path(ScopeKind::Organisation, "response-cache");
    let ws = collection_path(ScopeKind::Workspace, "response-cache");
    let ops: [(&str, &str, &str, &str, &Value); 6] = [
        (
            "get",
            "/response-cache",
            "responseCacheStatus",
            "The response cache: its store, each instance's figures, the platform's binding and the latest clears (platform roles)",
            &platform,
        ),
        (
            "post",
            "/response-cache/clear",
            "responseCacheClear",
            "Clear the response cache of every workspace (platform admins). Answers kept before are misses from then on",
            &result,
        ),
        (
            "get",
            &org,
            "orgResponseCache",
            "The binding of the response cache the organisation's workspaces get, and when it was cleared",
            &scope,
        ),
        (
            "post",
            &format!("{org}/clear"),
            "orgResponseCacheClear",
            "Clear the response cache of every workspace of the organisation (organisation admins)",
            &result,
        ),
        (
            "get",
            &ws,
            "workspaceResponseCache",
            "The binding of the response cache that answers the workspace's calls, and when it was cleared",
            &scope,
        ),
        (
            "post",
            &format!("{ws}/clear"),
            "workspaceResponseCacheClear",
            "Clear the workspace's response cache (workspace admins)",
            &result,
        ),
    ];
    for (method, path, id, summary, schema) in ops {
        api.add(api.op(method, path, id, "plugins", summary).ok(schema.clone()));
    }
    router
        .route("/response-cache", get(platform_status))
        .route("/response-cache/clear", post(platform_clear))
        .route(&org, get(org_status))
        .route(&format!("{org}/clear"), post(org_clear))
        .route(&ws, get(workspace_status))
        .route(&format!("{ws}/clear"), post(workspace_clear))
}

// --- status ------------------------------------------------------------------------------------

async fn platform_status(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<PlatformCacheView>> {
    if !platform_reader(&p) {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let instances: Vec<InstanceCache> = members(&st)
        .await
        .into_iter()
        .map(|m| InstanceCache {
            serves_calls: m.row.role != crate::bootstrap::Role::Admin.as_str(),
            id: m.row.id,
            role: m.row.role,
            hostname: m.row.hostname,
            figures: m.report.and_then(|r| r.answers),
        })
        .collect();
    let store = store_of(&instances);

    let (valkey, valkey_note) = if st.gw.answers.shared() {
        let view = match tokio::time::timeout(INSPECT_FOR, st.gw.answers.inspect()).await {
            Ok(Ok(Some(f))) => ValkeyView {
                reachable: true,
                keys: Some(f.keys),
                used_memory_bytes: Some(f.used_memory),
                max_memory_bytes: Some(f.max_memory),
                eviction_policy: Some(f.eviction_policy),
                error: None,
            },
            Ok(Ok(None)) => unreachable_view("the store gave no figures".into()),
            Ok(Err(e)) => unreachable_view(e.to_string()),
            Err(_) => unreachable_view("no answer in time".into()),
        };
        (Some(view), None)
    } else if store == "valkey" || store == "mixed" {
        (None, Some("this management instance has no PGW_VALKEY_URL, so it cannot read the shared store".into()))
    } else {
        (None, None)
    };

    let install = st
        .store
        .list(PluginInstall::KIND, ParentFilter::Any, None, 1000)
        .await?
        .into_iter()
        .filter_map(|r| r.into_obj::<PluginInstall>().ok())
        .find(|o| o.doc.plugin == PLUGIN);
    let install = match install {
        Some(o) => InstallView {
            id: Some(o.id),
            installed: true,
            enabled: o.doc.enabled,
            default_binding: o.doc.default_binding.map(PluginBinding::tagged),
            version: Some(o.version),
        },
        None => InstallView { id: None, installed: false, enabled: false, default_binding: None, version: None },
    };

    Ok(Json(PlatformCacheView {
        store,
        valkey,
        valkey_note,
        instances,
        install,
        cleared: snap.answer_clears.platform.as_ref().map(|c| clear_view(&snap, c)),
        recent_clears: recent_clears(&snap),
    }))
}

fn unreachable_view(error: String) -> ValkeyView {
    ValkeyView {
        reachable: false,
        keys: None,
        used_memory_bytes: None,
        max_memory_bytes: None,
        eviction_policy: None,
        error: Some(error),
    }
}

/// Where the instances that serve calls keep answers.
fn store_of(instances: &[InstanceCache]) -> String {
    let mut kinds: Vec<&str> = instances
        .iter()
        .filter(|i| i.serves_calls)
        .filter_map(|i| i.figures.as_ref())
        .map(|f| f.store.as_str())
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    match kinds.as_slice() {
        [] => "unknown".into(),
        [one] => (*one).to_owned(),
        _ => "mixed".into(),
    }
}

fn recent_clears(snap: &Snapshot) -> Vec<ScopeClear> {
    let c = &snap.answer_clears;
    let mut out: Vec<ScopeClear> = Vec::new();
    if let Some(p) = &c.platform {
        out.push(ScopeClear {
            scope: "platform".into(),
            organisation_id: None,
            workspace_id: None,
            name: None,
            clear: clear_view(snap, p),
        });
    }
    for (id, clear) in &c.organisations {
        out.push(ScopeClear {
            scope: "organisation".into(),
            organisation_id: Some(id.clone()),
            workspace_id: None,
            name: snap.organisations.get(id).map(|o| o.doc.name.clone()),
            clear: clear_view(snap, clear),
        });
    }
    for (id, clear) in &c.workspaces {
        let ws = snap.workspaces.get(id);
        out.push(ScopeClear {
            scope: "workspace".into(),
            organisation_id: ws.map(|w| w.organisation_id.clone()),
            workspace_id: Some(id.clone()),
            name: ws.map(|w| w.doc.name.clone()),
            clear: clear_view(snap, clear),
        });
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.clear.at));
    out.truncate(RECENT_CLEARS);
    out
}

fn clear_view(snap: &Snapshot, c: &ClearRt) -> ClearView {
    let by_display = (c.by_kind == "user").then(|| snap.users.get(&c.by).map(|u| u.doc.email.clone())).flatten();
    ClearView { at: c.at, by: c.by.clone(), by_kind: c.by_kind.clone(), by_display }
}

async fn org_status(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<ScopeCacheView>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Organisation, &params, &snap)?;
    if !p.can_read_org(scope.org()) {
        return Err(ApiError::forbidden());
    }
    let org = snap.organisations.get(scope.org()).ok_or_else(|| ApiError::not_found("organisation"))?;
    let own = org.plugin_bindings.get(PLUGIN);
    let (binding, why_none) = match own {
        Some(b) if b.mode != PluginMode::Off => {
            (Some(effective("organisation", &org_binding_id(&st, &org.id).await?, b)), None)
        }
        Some(_) => (None, Some("switched off for the organisation".to_owned())),
        None => platform_binding(&snap),
    };
    let c = &snap.answer_clears;
    Ok(Json(ScopeCacheView {
        store: st.gw.answers.kind().into(),
        binding,
        why_none,
        problem: None,
        cleared: ClearTimes {
            own: c.organisations.get(&org.id).map(|x| clear_view(&snap, x)),
            organisation: None,
            platform: c.platform.as_ref().map(|x| clear_view(&snap, x)),
            effective: [c.platform.as_ref(), c.organisations.get(&org.id)].into_iter().flatten().map(|x| x.at).max(),
        },
    }))
}

async fn workspace_status(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<ScopeCacheView>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !p.can_read_workspace(scope.ws(), scope.org()) {
        return Err(ApiError::forbidden());
    }
    let ws = snap.workspaces.get(scope.ws()).ok_or_else(|| ApiError::not_found("workspace"))?;
    let org = snap.organisations.get(&ws.organisation_id).ok_or_else(|| ApiError::not_found("organisation"))?;
    let active = ws.plugins.iter().find(|b| b.plugin == PLUGIN);
    let (binding, why_none, problem) = match active {
        Some(b) => {
            let source = if b.id.starts_with(&crate::plugins::default_binding_id("")) {
                "platform"
            } else if b.id.starts_with(&format!("{}_", OrgPluginBinding::PREFIX)) {
                "organisation"
            } else {
                "workspace"
            };
            let locked = source == "organisation" && org.plugin_bindings.get(PLUGIN).is_some_and(|o| o.locked);
            let binding = EffectiveBinding {
                source: source.into(),
                binding_id: b.id.clone(),
                mode: b.mode,
                settings: tagged(b.settings.clone()),
                timeout_ms: Some(b.timeout.as_millis() as u64),
                locked,
            };
            (Some(binding), None, b.bound.as_ref().err().cloned())
        }
        None => {
            let own: Vec<Obj<WorkspacePluginBinding>> = st
                .store
                .list(WorkspacePluginBinding::KIND, ParentFilter::Is(&ws.id), None, 1000)
                .await?
                .into_iter()
                .filter_map(|r| r.into_obj().ok())
                .collect();
            let own_mode = own.iter().find(|b| b.doc.0.plugin == PLUGIN).map(|b| b.doc.0.mode);
            (None, Some(why_none(&snap, org.plugin_bindings.get(PLUGIN), own_mode)), None)
        }
    };
    let c = &snap.answer_clears;
    Ok(Json(ScopeCacheView {
        store: st.gw.answers.kind().into(),
        binding,
        why_none,
        problem,
        cleared: ClearTimes {
            own: c.workspaces.get(&ws.id).map(|x| clear_view(&snap, x)),
            organisation: c.organisations.get(&org.id).map(|x| clear_view(&snap, x)),
            platform: c.platform.as_ref().map(|x| clear_view(&snap, x)),
            effective: c.effective(&org.id, &ws.id),
        },
    }))
}

/// The platform's binding, or why there is none.
fn platform_binding(snap: &Snapshot) -> (Option<EffectiveBinding>, Option<String>) {
    match snap.plugin_installs.get(PLUGIN) {
        None => (None, Some("the plugin is not installed".into())),
        Some(i) if !i.enabled => (None, Some("the plugin's install is switched off".into())),
        Some(i) => match i.default_binding.as_ref().filter(|b| b.plugin == PLUGIN) {
            Some(b) if b.mode != PluginMode::Off => {
                (Some(effective("platform", &crate::plugins::default_binding_id(PLUGIN), b)), None)
            }
            Some(_) => (None, Some("switched off for the platform".into())),
            None => {
                (None, Some("no binding: neither the platform, the organisation nor the workspace binds it".into()))
            }
        },
    }
}

/// Why no binding answers a workspace's calls. Bindings that are off are not in the snapshot,
/// so the workspace's own comes from the store.
fn why_none(snap: &Snapshot, org: Option<&PluginBinding>, own: Option<PluginMode>) -> String {
    match snap.plugin_installs.get(PLUGIN) {
        None => return "the plugin is not installed".into(),
        Some(i) if !i.enabled => return "the plugin's install is switched off".into(),
        Some(_) => {}
    }
    match (org, own) {
        (Some(o), _) if o.locked => "switched off by the organisation for its workspaces".into(),
        (_, Some(PluginMode::Off)) => "switched off for this workspace".into(),
        (Some(o), None) if o.mode == PluginMode::Off => "switched off for the organisation".into(),
        _ => platform_binding(snap).1.unwrap_or_else(|| "the binding cannot be used here".into()),
    }
}

fn effective(source: &str, id: &str, b: &PluginBinding) -> EffectiveBinding {
    EffectiveBinding {
        source: source.into(),
        binding_id: id.into(),
        mode: b.mode,
        settings: b.clone().tagged().settings,
        timeout_ms: b.timeout_ms.map(u64::from),
        locked: b.locked,
    }
}

fn tagged(settings: Value) -> Value {
    PluginBinding {
        plugin: PLUGIN.into(),
        mode: PluginMode::Off,
        priority: 0,
        timeout_ms: None,
        on_error: Default::default(),
        expose: false,
        locked: false,
        settings,
    }
    .tagged()
    .settings
}

/// The id of the organisation's binding: the snapshot keeps its document only.
async fn org_binding_id(st: &AdminState, org: &str) -> ApiResult<String> {
    Ok(st
        .store
        .list(OrgPluginBinding::KIND, ParentFilter::Is(org), None, 1000)
        .await?
        .into_iter()
        .filter_map(|r| r.into_obj::<OrgPluginBinding>().ok())
        .find(|b| b.doc.0.plugin == PLUGIN)
        .map(|b| b.id)
        .unwrap_or_default())
}

// --- clearing ----------------------------------------------------------------------------------

async fn platform_clear(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<ClearResult>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    clear(&st, &p, Scope::Platform).await
}

async fn org_clear(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<ClearResult>> {
    let scope = resolve_scope(ScopeKind::Organisation, &params, &st.config.snapshot())?;
    if !p.can_admin_org(scope.org()) {
        return Err(ApiError::forbidden());
    }
    clear(&st, &p, Scope::Organisation(scope.org())).await
}

async fn workspace_clear(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<ClearResult>> {
    let scope = resolve_scope(ScopeKind::Workspace, &params, &st.config.snapshot())?;
    if !p.can_admin_workspace(scope.ws(), scope.org()) {
        return Err(ApiError::forbidden());
    }
    clear(&st, &p, Scope::Workspace(scope.org(), scope.ws())).await
}

enum Scope<'a> {
    Platform,
    Organisation(&'a str),
    /// The organisation, then the workspace.
    Workspace(&'a str, &'a str),
}

async fn clear(st: &AdminState, p: &Principal, scope: Scope<'_>) -> ApiResult<Json<ClearResult>> {
    let (name, scope_id, org, ws) = match scope {
        Scope::Platform => ("platform", None, None, None),
        Scope::Organisation(o) => ("organisation", Some(o), Some(o), None),
        Scope::Workspace(o, w) => ("workspace", Some(w), Some(o), Some(w)),
    };
    let id = ResponseCacheClear::id_for(scope_id);
    let mut audit = p.audit(org, ws);
    audit.action = "clear".into();
    let mut tx = st.store.begin().await?;
    let mut cleared_at = Utc::now();
    let existing = tx.get_kind(ResponseCacheClear::KIND, &id).await?;
    let doc = |at| ResponseCacheClear { cleared_at: at, by: p.id.clone(), by_kind: p.kind.as_str().to_owned() };
    match existing {
        Some(row) => {
            // A clock that went back must not undo an earlier clear.
            if let Ok(old) = row.into_obj::<ResponseCacheClear>() {
                cleared_at = cleared_at.max(old.doc.cleared_at);
            }
            tx.update(&id, None, None, &serde_json::to_value(doc(cleared_at)).map_err(ApiError::internal)?, audit)
                .await?;
        }
        None => {
            tx.create(
                ResponseCacheClear::KIND,
                &id,
                scope_id,
                None,
                &serde_json::to_value(doc(cleared_at)).map_err(ApiError::internal)?,
                audit,
            )
            .await?;
        }
    }
    let config_version = st.commit(tx).await?;

    let reclaim = if st.gw.answers.shared() {
        let workspaces: Option<Vec<String>> = match scope {
            Scope::Platform => None,
            Scope::Organisation(o) => Some(
                st.config
                    .snapshot()
                    .workspaces
                    .values()
                    .filter(|w| w.organisation_id == o)
                    .map(|w| w.id.clone())
                    .collect(),
            ),
            Scope::Workspace(_, w) => Some(vec![w.to_owned()]),
        };
        let store = st.gw.answers.clone();
        tokio::spawn(async move {
            let which = workspaces.as_deref().map_or(Forget::All, Forget::Workspaces);
            match store.forget(which).await {
                Ok(n) => tracing::info!(removed = n, "the answers of a cleared response cache were removed"),
                Err(e) => {
                    tracing::warn!(error = %e, "the answers of a cleared response cache could not be removed; they are misses all the same and expire in their time")
                }
            }
        });
        "started"
    } else if members(st)
        .await
        .iter()
        .any(|m| m.report.as_ref().and_then(|r| r.answers.as_ref()).is_some_and(|a| a.store == "valkey"))
    {
        // The gateways share a Valkey this instance does not reach: their answers are misses
        // all the same, and expire in their time.
        "not_available"
    } else {
        "per_instance"
    };
    Ok(Json(ClearResult { scope: name.into(), cleared_at, config_version, reclaim: reclaim.into() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(role: &str, store: Option<&str>) -> InstanceCache {
        InstanceCache {
            id: "i".into(),
            role: role.into(),
            hostname: "h".into(),
            serves_calls: role != "admin",
            figures: store.map(|s| AnswerFigures { store: s.into(), ..Default::default() }),
        }
    }

    #[test]
    fn the_store_is_the_one_the_gateways_report() {
        assert_eq!(store_of(&[]), "unknown");
        assert_eq!(store_of(&[instance("admin", Some("memory")), instance("gateway", Some("valkey"))]), "valkey");
        assert_eq!(store_of(&[instance("gateway", Some("valkey")), instance("all", Some("valkey"))]), "valkey");
        assert_eq!(store_of(&[instance("gateway", Some("memory")), instance("all", Some("valkey"))]), "mixed");
        assert_eq!(store_of(&[instance("gateway", None)]), "unknown", "an older release");
    }
}
