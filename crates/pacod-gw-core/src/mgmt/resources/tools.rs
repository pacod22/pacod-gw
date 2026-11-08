//! Tool servers of an organisation (runbook section 21): registering them, their credentials,
//! probing what they offer, and approving their tools (catalogue MCP-01, MCP-03, MCP-10).
//!
//! An organisation's administrators register a server and its credential. The gateway asks the
//! server for its tools (a probe) and keeps what it saw outside the configuration; an
//! administrator approves each tool, which pins its definition. Callers are served the pinned
//! definitions only. A later probe that sees a pinned definition changed holds the tool
//! (`changed`) until it is approved again, so that a server cannot change what models read
//! about a tool, or what it takes, behind the administrators' backs.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::crypto::envelope::{fingerprint, seal};
use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{BuildCtx, Item, Ops, Params, Resource, ScopeKind, register, resolve_scope};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::tools::{CONTENT_TYPES, MAX_TOOLS_PER_SERVER};
use crate::model::{
    ApiKey, ConnectorCredential, CredentialScope, CredentialState, Doc, Obj, PinState, Status, ToolBundle, ToolPin,
    ToolServer, is_slug, new_id,
};
use crate::snapshot::Snapshot;
use crate::store::Tx;
use crate::store::probes::{ProbeRun, ProbedItem};
use crate::tools::{grant, names, pin, probe};

/// The largest tool list a probe reads: every definition of a server with many tools.
const MAX_PROBE_BYTES: usize = 16 << 20;
/// Servers probed in one round of upkeep at most, so that a round stays short.
const PROBES_PER_ROUND: usize = 5;
/// Headers a credential may not be sent in: the transport's own, and the protocols'.
const RESERVED_HEADERS: [&str; 9] = [
    "host",
    "accept",
    "connection",
    "transfer-encoding",
    "cookie",
    "upgrade",
    "te",
    "mcp-protocol-version",
    "a2a-version",
];

impl Resource for ToolServer {
    const PLURAL: &'static str = "tool-servers";
    const LABEL: &'static str = "tool server";
    const SCOPE: ScopeKind = ScopeKind::Organisation;
    const STRICT: bool = true;
    type Input = ToolServer;
    type View = ToolServer;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(mut input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if !names::is_server_name(&input.name) {
            return Err(ApiError::invalid(
                "name must be lower-case letters, digits and hyphens, at most 20, starting with a letter",
            ));
        }
        if existing.is_some_and(|e| e.doc.name != input.name) {
            return Err(ApiError::invalid("a tool server keeps its name: callers call its tools by it"));
        }
        input.url = input.url.trim().to_owned();
        let hub =
            ctx.st.gw.tools.as_ref().ok_or_else(|| ApiError::unavailable("this gateway calls no tool servers"))?;
        hub.egress.check_url(&input.url).map_err(|e| ApiError::invalid(format!("url: {e}")))?;
        let header = input.auth.header.trim().to_ascii_lowercase();
        if axum::http::HeaderName::from_bytes(header.as_bytes()).is_err()
            || RESERVED_HEADERS.contains(&header.as_str())
            || header.starts_with("content-")
            || header.starts_with("mcp-")
        {
            return Err(ApiError::invalid(format!(
                "auth.header: {header:?} cannot carry the credential; use authorization or a header of the server's own"
            )));
        }
        input.auth.header = header;
        if input.auth.prefix.chars().any(|c| c.is_control()) {
            return Err(ApiError::invalid("auth.prefix must not hold control characters"));
        }
        if !(1_000..=300_000).contains(&input.timeout_ms) {
            return Err(ApiError::invalid("timeout_ms must be between 1000 and 300000"));
        }
        if !(1024..=16 << 20).contains(&input.result.max_bytes) {
            return Err(ApiError::invalid("result.max_bytes must be between 1024 and 16777216"));
        }
        if let Some(t) = input.result.content_types.iter().find(|t| !CONTENT_TYPES.contains(&t.as_str())) {
            return Err(ApiError::invalid(format!("result.content_types: {t:?} is not one of {CONTENT_TYPES:?}")));
        }
        if let Some(workspaces) = &input.workspaces {
            for ws in workspaces {
                if ctx.snap.workspaces.get(ws).is_none_or(|w| w.organisation_id != ctx.scope.org()) {
                    return Err(ApiError::invalid(format!("workspaces: {ws} is not a workspace of the organisation")));
                }
            }
        }
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }

    fn effects(obj: &Obj<Self>, _snap: &Snapshot) -> Vec<String> {
        vec![format!(
            "The tools of {} leave /mcp, and its credentials and approvals are deleted with it.",
            obj.doc.name
        )]
    }

    /// The server's approvals and credentials go with it.
    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let org = obj.parent.clone();
        let entry = || crate::store::AuditEntry {
            organisation_id: org.clone(),
            ..crate::store::AuditEntry::system("tool_server_deleted")
        };
        for row in tx.list(ToolPin::KIND, Some(&obj.id)).await? {
            tx.delete(&row.id, entry()).await?;
        }
        for row in tx.list(ConnectorCredential::KIND, None).await? {
            if row.doc.get("owner_id").and_then(Value::as_str) == Some(obj.id.as_str()) {
                tx.delete(&row.id, entry()).await?;
            }
        }
        Ok(())
    }
}

// --- bundles ---------------------------------------------------------------------------------

impl Resource for ToolBundle {
    const PLURAL: &'static str = "tool-bundles";
    const LABEL: &'static str = "tool bundle";
    const SCOPE: ScopeKind = ScopeKind::Workspace;
    const STRICT: bool = true;
    type Input = ToolBundle;
    type View = ToolBundle;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(mut input: Self::Input, existing: Option<&Obj<Self>>, _ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if !is_slug(&input.name) {
            return Err(ApiError::invalid("name must be lower-case letters, digits and hyphens"));
        }
        if existing.is_some_and(|e| e.doc.name != input.name) {
            return Err(ApiError::invalid("a tool bundle keeps its name: keys name it"));
        }
        for (field, entries) in [("allow", &mut input.allow), ("deny", &mut input.deny)] {
            for entry in entries.iter_mut() {
                *entry = entry.trim().to_owned();
                grant::Item::parse(entry).map_err(|e| ApiError::invalid(format!("{field}: {e}")))?;
            }
            entries.dedup();
        }
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }

    /// Keys name bundles: one that a key names stays.
    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let keys: Vec<String> = tx
            .list(ApiKey::KIND, obj.parent.as_deref())
            .await?
            .into_iter()
            .filter(|r| {
                r.doc
                    .get("tool_bundles")
                    .and_then(Value::as_array)
                    .is_some_and(|b| b.iter().any(|n| n == &obj.doc.name))
            })
            .filter_map(|r| r.name)
            .collect();
        if !keys.is_empty() {
            return Err(ApiError::conflict(format!("keys name this tool bundle: {}", keys.join(", ")))
                .with_details(serde_json::json!({ "keys": keys })));
        }
        Ok(())
    }
}

// --- credentials -----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ToolCredentialInput {
    #[serde(default)]
    pub label: String,
    /// The secret the server is called with. Encrypted before it is stored, never returned.
    #[schemars(extend("writeOnly" = true))]
    pub secret: String,
    /// A workspace whose calls use this credential instead of the organisation's. Unset: the
    /// organisation's.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Use this credential from now on. Default true.
    #[serde(default = "yes")]
    pub make_current: bool,
    /// How long the credential it replaces is kept for a rollback, in hours. Default 24.
    #[serde(default)]
    pub grace_hours: Option<u32>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ToolCredentialStateInput {
    pub state: CredentialState,
}

/// A credential of a tool server as the API returns it. The secret is never part of it.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ToolCredentialView {
    pub owner_id: String,
    pub scope: CredentialScope,
    pub label: String,
    /// Identifies the secret without revealing it.
    pub fingerprint: String,
    pub state: CredentialState,
    pub retire_at: Option<DateTime<Utc>>,
    /// The key-encryption key this credential is encrypted with.
    pub kek_id: String,
}

fn credential_view(obj: &Obj<ConnectorCredential>) -> Item<ToolCredentialView> {
    let d = &obj.doc;
    Item {
        meta: obj.meta(),
        data: ToolCredentialView {
            owner_id: d.owner_id.clone(),
            scope: d.scope.clone(),
            label: d.label.clone(),
            fingerprint: d.fingerprint.clone(),
            state: d.state,
            retire_at: d.retire_at,
            kek_id: d.sealed.kek_id.clone(),
        },
    }
}

/// The server named by the path, in the organisation named by the path, which the principal
/// may read.
async fn server_of(
    st: &AdminState,
    p: &Principal,
    params: &HashMap<String, String>,
    snap: &Snapshot,
) -> ApiResult<Obj<ToolServer>> {
    let scope = resolve_scope(ScopeKind::Organisation, params, snap)?;
    if !p.can_read_org(scope.org()) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("tool server"))?;
    let server = st.store.get_obj::<ToolServer>(id).await.map_err(|_| ApiError::not_found("tool server"))?;
    if server.parent.as_deref() != Some(scope.org()) {
        return Err(ApiError::not_found("tool server"));
    }
    Ok(server)
}

/// Whether the principal may manage credentials of `scope` for `server`: the organisation's
/// administrators for its own, a workspace's for the workspace's, of a workspace the server is
/// open to.
fn can_manage(p: &Principal, server: &Obj<ToolServer>, scope: &CredentialScope, snap: &Snapshot) -> bool {
    let org = server.parent.as_deref().unwrap_or_default();
    match scope {
        CredentialScope::Organisation(o) => o == org && p.can_admin_org(org),
        CredentialScope::Workspace(ws) => {
            snap.workspaces.get(ws).is_some_and(|w| w.organisation_id == org && p.can_admin_workspace(ws, org))
                && server.doc.workspaces.as_ref().is_none_or(|open| open.contains(ws))
        }
        CredentialScope::Global => false,
    }
}

fn workspace_of(scope: &CredentialScope) -> Option<&str> {
    match scope {
        CredentialScope::Workspace(ws) => Some(ws),
        _ => None,
    }
}

async fn create_credential(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<ToolCredentialInput>,
) -> ApiResult<(StatusCode, Json<Item<ToolCredentialView>>)> {
    let snap = st.config.snapshot();
    let server = server_of(&st, &p, &params, &snap).await?;
    let org = server.parent.clone().unwrap_or_default();
    let scope = match input.workspace_id.as_deref().filter(|w| !w.is_empty()) {
        Some(ws) => CredentialScope::Workspace(ws.to_owned()),
        None => CredentialScope::Organisation(org.clone()),
    };
    if !can_manage(&p, &server, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let secret = input.secret.trim();
    if secret.is_empty() {
        return Err(ApiError::invalid("secret must not be empty"));
    }
    if secret.chars().any(|c| c.is_control()) {
        return Err(ApiError::invalid("secret must be one line of text: it is sent in a header"));
    }
    let id = new_id(ConnectorCredential::PREFIX);
    let kek = st.boot.keks.first().ok_or_else(|| ApiError::internal("no key-encryption key"))?;
    let doc = ConnectorCredential {
        owner_id: server.id.clone(),
        scope: scope.clone(),
        label: input.label.trim().to_owned(),
        // The credential id binds the ciphertext to this record.
        sealed: seal(kek, &id, secret.as_bytes()),
        fingerprint: fingerprint(secret),
        state: if input.make_current { CredentialState::Current } else { CredentialState::Retiring },
        retire_at: None,
    };
    let grace = Duration::hours(i64::from(input.grace_hours.unwrap_or(24).min(720)));
    let mut tx = st.store.begin().await?;
    if doc.state == CredentialState::Current {
        retire_current(&mut tx, &p, &org, &server.id, &scope, &id, grace).await?;
    }
    let audit = p.audit(Some(&org), workspace_of(&scope));
    let row = tx
        .create(
            ConnectorCredential::KIND,
            &id,
            Some(&server.id),
            None,
            &serde_json::to_value(&doc).map_err(ApiError::internal)?,
            audit,
        )
        .await?;
    st.commit(tx).await?;
    Ok((StatusCode::CREATED, Json(credential_view(&row.into_obj()?))))
}

/// Makes the current credential of a scope retiring for `grace`, except `replacing_with`.
async fn retire_current(
    tx: &mut Tx,
    p: &Principal,
    org: &str,
    server_id: &str,
    scope: &CredentialScope,
    replacing_with: &str,
    grace: Duration,
) -> ApiResult<()> {
    for row in tx.list(ConnectorCredential::KIND, Some(server_id)).await? {
        let mut old: Obj<ConnectorCredential> = row.into_obj()?;
        if old.id != replacing_with && &old.doc.scope == scope && old.doc.state == CredentialState::Current {
            old.doc.state = CredentialState::Retiring;
            old.doc.retire_at = Some(Utc::now() + grace);
            let mut audit = p.audit(Some(org), workspace_of(scope));
            audit.action = "retire".into();
            tx.update(&old.id, None, None, &serde_json::to_value(&old.doc).map_err(ApiError::internal)?, audit).await?;
        }
    }
    Ok(())
}

async fn list_credentials(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<Vec<Item<ToolCredentialView>>>> {
    let snap = st.config.snapshot();
    let server = server_of(&st, &p, &params, &snap).await?;
    let mut tx = st.store.begin().await?;
    let rows = tx.list(ConnectorCredential::KIND, Some(&server.id)).await?;
    let mut out = Vec::new();
    for row in rows {
        let obj: Obj<ConnectorCredential> = row.into_obj()?;
        // A workspace's own credential is shown to whoever reads that workspace.
        let readable = match &obj.doc.scope {
            CredentialScope::Workspace(ws) => p.can_read_workspace(ws, server.parent.as_deref().unwrap_or_default()),
            _ => true,
        };
        if readable {
            out.push(credential_view(&obj));
        }
    }
    Ok(Json(out))
}

async fn load_credential(
    st: &AdminState,
    p: &Principal,
    params: &HashMap<String, String>,
    snap: &Snapshot,
) -> ApiResult<(Obj<ToolServer>, Obj<ConnectorCredential>)> {
    let server = server_of(st, p, params, snap).await?;
    let cid = params.get("cid").ok_or_else(|| ApiError::not_found("credential"))?;
    let cred = st.store.get_obj::<ConnectorCredential>(cid).await.map_err(|_| ApiError::not_found("credential"))?;
    if cred.doc.owner_id != server.id {
        return Err(ApiError::not_found("credential"));
    }
    if !can_manage(p, &server, &cred.doc.scope, snap) {
        return Err(ApiError::forbidden());
    }
    Ok((server, cred))
}

async fn set_credential_state(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<ToolCredentialStateInput>,
) -> ApiResult<Json<Item<ToolCredentialView>>> {
    let snap = st.config.snapshot();
    let (server, mut cred) = load_credential(&st, &p, &params, &snap).await?;
    let org = server.parent.clone().unwrap_or_default();
    let mut tx = st.store.begin().await?;
    if input.state == CredentialState::Current {
        retire_current(&mut tx, &p, &org, &server.id, &cred.doc.scope, &cred.id, Duration::hours(24)).await?;
        cred.doc.retire_at = None;
    }
    cred.doc.state = input.state;
    let mut audit = p.audit(Some(&org), workspace_of(&cred.doc.scope));
    audit.action =
        format!("set_state:{}", serde_json::to_value(input.state).map_err(ApiError::internal)?.as_str().unwrap_or(""));
    let row =
        tx.update(&cred.id, None, None, &serde_json::to_value(&cred.doc).map_err(ApiError::internal)?, audit).await?;
    st.commit(tx).await?;
    Ok(Json(credential_view(&row.into_obj()?)))
}

async fn delete_credential(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<StatusCode> {
    let snap = st.config.snapshot();
    let (server, cred) = load_credential(&st, &p, &params, &snap).await?;
    let mut tx = st.store.begin().await?;
    let audit = p.audit(server.parent.as_deref(), workspace_of(&cred.doc.scope));
    tx.delete(&cred.id, audit).await?;
    st.commit(tx).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- probing and approvals -------------------------------------------------------------------

/// Where a tool stands: what the server lists now against what was approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// Listed by the server, never approved: not served.
    New,
    /// Served as approved.
    Approved,
    /// The server's definition differs from the approved one: not served until approved again.
    Changed,
    /// Approved, but the last probe did not list it: served, and calls will likely fail.
    Gone,
    /// Switched off by an administrator.
    Disabled,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SeenView {
    pub definition: Value,
    pub hash: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PinView {
    pub exposed_name: String,
    pub definition: Value,
    pub hash: String,
    pub state: PinState,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ToolView {
    /// The tool's own name on its server.
    pub tool: String,
    pub status: ToolStatus,
    /// What the server lists now, when a probe saw it.
    pub seen: Option<SeenView>,
    /// What was approved, when it was.
    pub pin: Option<PinView>,
    /// The name it would be exposed under: `<server>__<tool>`; `null` when an alias is needed.
    pub suggested_name: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProbeRunView {
    pub probed_at: DateTime<Utc>,
    /// The protocol revision the server speaks; empty when the probe failed.
    pub version: String,
    pub ok: bool,
    pub problem: String,
    pub tools: i32,
}

/// What the gateway knows of a server's tools.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ToolsView {
    /// The last probe; `null` before the first.
    pub probe: Option<ProbeRunView>,
    pub tools: Vec<ToolView>,
}

fn status_of(pin: Option<&ToolPin>, seen: Option<&ProbedItem>, run: Option<&ProbeRun>) -> ToolStatus {
    let Some(pin) = pin else { return ToolStatus::New };
    if pin.state == PinState::Disabled {
        return ToolStatus::Disabled;
    }
    let listed = seen.filter(|s| run.is_none_or(|r| !r.ok || s.last_seen >= r.probed_at));
    match listed {
        None => ToolStatus::Gone,
        Some(s) if s.hash != pin.hash || pin.state == PinState::Changed => ToolStatus::Changed,
        Some(_) => ToolStatus::Approved,
    }
}

async fn tools_view(st: &AdminState, server: &Obj<ToolServer>) -> ApiResult<ToolsView> {
    let (run, seen) = st.store.probes_of(&server.id).await.map_err(ApiError::internal)?;
    let mut tx = st.store.begin().await?;
    let pins: Vec<Obj<ToolPin>> =
        tx.list(ToolPin::KIND, Some(&server.id)).await?.into_iter().map(|r| r.into_obj()).collect::<Result<_, _>>()?;
    drop(tx);
    let mut names: Vec<String> =
        seen.iter().map(|s| s.item.clone()).chain(pins.iter().map(|p| p.doc.tool.clone())).collect();
    names.sort();
    names.dedup();
    let tools = names
        .into_iter()
        .map(|tool| {
            let pin = pins.iter().find(|p| p.doc.tool == tool).map(|p| &p.doc);
            let item = seen.iter().find(|s| s.item == tool);
            ToolView {
                status: status_of(pin, item, run.as_ref()),
                seen: item.map(|s| SeenView {
                    definition: s.definition.clone(),
                    hash: s.hash.clone(),
                    first_seen: s.first_seen,
                    last_seen: s.last_seen,
                }),
                pin: pin.map(|p| PinView {
                    exposed_name: p.exposed_name.clone(),
                    definition: p.definition.clone(),
                    hash: p.hash.clone(),
                    state: p.state,
                    approved_by: p.approved_by.clone(),
                    approved_at: p.approved_at,
                }),
                suggested_name: names::exposed(&server.doc.name, &tool),
                tool,
            }
        })
        .collect();
    Ok(ToolsView {
        probe: run.map(|r| ProbeRunView {
            probed_at: r.probed_at,
            version: r.version,
            ok: r.ok,
            problem: r.problem,
            tools: r.tools,
        }),
        tools,
    })
}

async fn list_tools(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<ToolsView>> {
    let snap = st.config.snapshot();
    let server = server_of(&st, &p, &params, &snap).await?;
    Ok(Json(tools_view(&st, &server).await?))
}

/// Probes an active server now, records what it saw, and holds the approved tools whose
/// definition changed. A failed probe is recorded too. Returns the probe's problem, if any.
pub async fn probe_server(st: &AdminState, server_id: &str) -> anyhow::Result<Option<String>> {
    let snap = st.config.snapshot();
    let Some(rt) = snap.organisations.values().find_map(|o| o.tool_servers.values().find(|s| s.id == server_id)) else {
        anyhow::bail!("the tool server is not active");
    };
    let hub = st.gw.tools.as_ref().ok_or_else(|| anyhow::anyhow!("this gateway calls no tool servers"))?;
    let now = Utc::now();
    let probed = match probe::probe(&hub.client, &rt.target(None, MAX_PROBE_BYTES)).await {
        Ok(p) => p,
        Err(e) => {
            let problem = e.to_string();
            st.store.probe_failed(server_id, &problem, now).await?;
            return Ok(Some(problem));
        }
    };
    let items: Vec<(String, Value, String)> =
        probed.tools.iter().map(|t| (t.name.clone(), t.definition.clone(), t.hash.clone())).collect();
    st.store.probe_record(server_id, &probed.version, &items, now).await?;
    hold_changed(st, &rt.organisation_id, server_id, &probed.tools).await?;
    Ok(None)
}

/// Holds the approved tools of a server whose definition the server now lists otherwise.
async fn hold_changed(st: &AdminState, org: &str, server_id: &str, seen: &[probe::Seen]) -> anyhow::Result<()> {
    let mut tx = st.store.begin().await?;
    let mut held = 0;
    for row in tx.list(ToolPin::KIND, Some(server_id)).await? {
        let mut pin: Obj<ToolPin> = row.into_obj()?;
        let changed = seen.iter().find(|s| s.name == pin.doc.tool).is_some_and(|s| s.hash != pin.doc.hash);
        if pin.doc.state == PinState::Approved && changed {
            pin.doc.state = PinState::Changed;
            let audit = crate::store::AuditEntry {
                organisation_id: Some(org.to_owned()),
                action: "definition_changed".into(),
                detail: serde_json::json!({ "tool": pin.doc.tool, "approved_hash": pin.doc.hash }),
                ..crate::store::AuditEntry::system("tool_probe")
            };
            tx.update(&pin.id, Some(pin.version), None, &serde_json::to_value(&pin.doc)?, audit).await?;
            held += 1;
        }
    }
    if held > 0 {
        st.commit(tx).await.map_err(|e| anyhow::anyhow!(e.message))?;
        tracing::warn!(server = server_id, held, "tool definitions changed; the tools are held until approved again");
    }
    Ok(())
}

async fn probe_now(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<ToolsView>> {
    let snap = st.config.snapshot();
    let server = server_of(&st, &p, &params, &snap).await?;
    if !p.can_admin_org(server.parent.as_deref().unwrap_or_default()) {
        return Err(ApiError::forbidden());
    }
    if server.doc.status != Status::Active {
        return Err(ApiError::conflict("the tool server is disabled; enable it to probe it"));
    }
    probe_server(&st, &server.id).await.map_err(ApiError::internal)?;
    Ok(Json(tools_view(&st, &server).await?))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApproveInput {
    /// The hash of the definition approved, as the tool list shows it. When the server listed
    /// another definition since, the approval is refused (`409`): read the list again.
    pub hash: String,
    /// The name callers call the tool by: `<server>__<name>`. Default `<server>__<tool>`; an
    /// alias is needed where that is not a function name providers accept.
    #[serde(default)]
    pub exposed_name: Option<String>,
}

async fn approve(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<ApproveInput>,
) -> ApiResult<Json<ToolsView>> {
    let snap = st.config.snapshot();
    let server = server_of(&st, &p, &params, &snap).await?;
    let org = server.parent.clone().unwrap_or_default();
    if !p.can_admin_org(&org) {
        return Err(ApiError::forbidden());
    }
    let tool = params.get("tool").ok_or_else(|| ApiError::not_found("tool"))?.clone();
    let (_, seen) = st.store.probes_of(&server.id).await.map_err(ApiError::internal)?;
    let item = seen.into_iter().find(|s| s.item == tool).ok_or_else(|| ApiError::not_found("tool"))?;
    if item.hash != input.hash {
        return Err(ApiError::conflict("the server lists another definition of this tool now; read the list again"));
    }
    pin::header_params(&item.definition).map_err(|e| {
        ApiError::invalid(format!("{tool} cannot be served: {e}; callers of the 2026-07-28 revision drop such a tool"))
    })?;
    let exposed = match input.exposed_name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) if names::fits(&server.doc.name, name) => name.to_owned(),
        Some(name) => {
            return Err(ApiError::invalid(format!(
                "exposed_name: {name:?} must be {}__ and letters, digits, underscores or hyphens, at most 64 in all",
                server.doc.name
            )));
        }
        None => names::exposed(&server.doc.name, &tool).ok_or_else(|| {
            ApiError::invalid(format!("{tool:?} is not a function name providers accept; give an exposed_name"))
        })?,
    };
    let mut tx = st.store.begin().await?;
    let pins: Vec<Obj<ToolPin>> =
        tx.list(ToolPin::KIND, Some(&server.id)).await?.into_iter().map(|r| r.into_obj()).collect::<Result<_, _>>()?;
    if pins.iter().any(|p| p.doc.exposed_name == exposed && p.doc.tool != tool) {
        return Err(ApiError::conflict(format!("another tool of the server is exposed as {exposed}")));
    }
    let doc = ToolPin {
        tool: tool.clone(),
        exposed_name: exposed,
        definition: item.definition,
        hash: item.hash,
        state: PinState::Approved,
        approved_by: p.id.clone(),
        approved_at: Utc::now(),
    };
    let mut audit = p.audit(Some(&org), None);
    audit.action = "approve".into();
    let value = serde_json::to_value(&doc).map_err(ApiError::internal)?;
    match pins.iter().find(|p| p.doc.tool == tool) {
        Some(existing) => {
            tx.update(&existing.id, Some(existing.version), Some(&tool), &value, audit).await?;
        }
        None => {
            if pins.len() >= MAX_TOOLS_PER_SERVER {
                return Err(ApiError::conflict(format!("a server has at most {MAX_TOOLS_PER_SERVER} approved tools")));
            }
            tx.create(ToolPin::KIND, &new_id(ToolPin::PREFIX), Some(&server.id), Some(&tool), &value, audit).await?;
        }
    }
    st.commit(tx).await?;
    Ok(Json(tools_view(&st, &server).await?))
}

async fn disable(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<ToolsView>> {
    let snap = st.config.snapshot();
    let server = server_of(&st, &p, &params, &snap).await?;
    let org = server.parent.clone().unwrap_or_default();
    if !p.can_admin_org(&org) {
        return Err(ApiError::forbidden());
    }
    let tool = params.get("tool").ok_or_else(|| ApiError::not_found("tool"))?;
    let mut tx = st.store.begin().await?;
    let row = tx
        .list(ToolPin::KIND, Some(&server.id))
        .await?
        .into_iter()
        .find(|r| r.name.as_deref() == Some(tool.as_str()))
        .ok_or_else(|| ApiError::not_found("approved tool"))?;
    let mut pin: Obj<ToolPin> = row.into_obj()?;
    pin.doc.state = PinState::Disabled;
    let mut audit = p.audit(Some(&org), None);
    audit.action = "disable".into();
    tx.update(&pin.id, Some(pin.version), None, &serde_json::to_value(&pin.doc).map_err(ApiError::internal)?, audit)
        .await?;
    st.commit(tx).await?;
    Ok(Json(tools_view(&st, &server).await?))
}

/// Probes the active tool servers whose last probe is older than the global setting
/// `tool_probe_interval_hours`, a few per round, and forgets what probes saw of servers that no
/// longer exist.
pub async fn upkeep(st: &AdminState) {
    let hours = st.config.snapshot().settings.tool_probe_interval_hours;
    let runs = match st.store.probe_runs().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "upkeep: reading the tool servers' probes failed");
            return;
        }
    };
    let snap = st.config.snapshot();
    let active: Vec<String> =
        snap.organisations.values().flat_map(|o| o.tool_servers.values().map(|s| s.id.clone())).collect();
    if hours > 0 {
        let due_before = Utc::now() - Duration::hours(i64::from(hours));
        let due = active
            .iter()
            .filter(|id| runs.iter().find(|r| &r.owner_id == *id).is_none_or(|r| r.probed_at < due_before))
            .take(PROBES_PER_ROUND);
        for id in due {
            match probe_server(st, id).await {
                Ok(None) => {}
                Ok(Some(problem)) => tracing::warn!(server = %id, %problem, "upkeep: a tool server's probe failed"),
                Err(e) => tracing::warn!(server = %id, error = %e, "upkeep: probing a tool server failed"),
            }
        }
    }
    // A disabled server keeps what was seen of it; a deleted one does not.
    if runs.iter().any(|r| !active.contains(&r.owner_id)) {
        let Ok(mut tx) = st.store.begin().await else { return };
        let Ok(rows) = tx.list(ToolServer::KIND, None).await else { return };
        drop(tx);
        for r in runs.iter().filter(|r| rows.iter().all(|row| row.id != r.owner_id)) {
            if let Err(e) = st.store.probes_forget(&r.owner_id).await {
                tracing::warn!(error = %e, "upkeep: forgetting a deleted tool server's probes failed");
            }
        }
    }
}

pub fn routes(mut router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    router = register::<ToolServer>(router, api, Ops::ALL);
    router = register::<ToolBundle>(router, api, Ops::ALL);
    let base = "/orgs/{org}/tool-servers/{id}";
    let coll = format!("{base}/credentials");
    let one = format!("{base}/credentials/{{cid}}");
    let cred = api.schema::<Item<ToolCredentialView>>();
    let cred_input = api.schema::<ToolCredentialInput>();
    let state_input = api.schema::<ToolCredentialStateInput>();
    api.add(
        api.op(
            "get",
            &coll,
            "listToolServerCredentials",
            "tool servers",
            "List a tool server's credentials (no secrets)",
        )
        .ok(serde_json::json!({ "type": "array", "items": cred })),
    );
    api.add(
        api.op("post", &coll, "createToolServerCredential", "tool servers", "Add or rotate a tool server's credential")
            .body(cred_input)
            .created(cred.clone()),
    );
    api.add(
        api.op(
            "post",
            &format!("{one}/state"),
            "setToolServerCredentialState",
            "tool servers",
            "Make a tool server's credential current or disable it",
        )
        .body(state_input)
        .ok(cred),
    );
    api.add(
        api.op("delete", &one, "deleteToolServerCredential", "tool servers", "Delete a tool server's credential")
            .no_content(),
    );
    let tools = api.schema::<ToolsView>();
    api.add(
        api.op(
            "get",
            &format!("{base}/tools"),
            "listToolServerTools",
            "tool servers",
            "The server's tools: what it lists and what was approved",
        )
        .ok(tools.clone()),
    );
    api.add(
        api.op("post", &format!("{base}/probe"), "probeToolServer", "tool servers", "Ask the server for its tools now")
            .ok(tools.clone()),
    );
    let approve_input = api.schema::<ApproveInput>();
    api.add(
        api.op(
            "post",
            &format!("{base}/tools/{{tool}}/approve"),
            "approveTool",
            "tool servers",
            "Approve a tool as the server lists it now, under the name callers call it by",
        )
        .body(approve_input)
        .ok(tools.clone()),
    );
    api.add(
        api.op(
            "post",
            &format!("{base}/tools/{{tool}}/disable"),
            "disableTool",
            "tool servers",
            "Stop serving an approved tool",
        )
        .ok(tools),
    );
    router
        .route(&coll, get(list_credentials).post(create_credential))
        .route(&one, delete(delete_credential))
        .route(&format!("{one}/state"), post(set_credential_state))
        .route(&format!("{base}/tools"), get(list_tools))
        .route(&format!("{base}/probe"), post(probe_now))
        .route(&format!("{base}/tools/{{tool}}/approve"), post(approve))
        .route(&format!("{base}/tools/{{tool}}/disable"), post(disable))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn pin(hash: &str, state: PinState) -> ToolPin {
        ToolPin {
            tool: "t".into(),
            exposed_name: "s__t".into(),
            definition: json!({}),
            hash: hash.into(),
            state,
            approved_by: "u".into(),
            approved_at: Utc::now(),
        }
    }

    fn seen(hash: &str, last_seen: DateTime<Utc>) -> ProbedItem {
        ProbedItem { item: "t".into(), definition: json!({}), hash: hash.into(), first_seen: last_seen, last_seen }
    }

    fn run(at: DateTime<Utc>, ok: bool) -> ProbeRun {
        ProbeRun { owner_id: "s".into(), probed_at: at, version: String::new(), ok, problem: String::new(), tools: 1 }
    }

    #[test]
    fn a_tool_stands_by_what_the_server_lists_against_what_was_approved() {
        let now = Utc::now();
        let earlier = now - Duration::hours(1);
        let last = run(now, true);
        assert_eq!(status_of(None, Some(&seen("a", now)), Some(&last)), ToolStatus::New);
        assert_eq!(
            status_of(Some(&pin("a", PinState::Approved)), Some(&seen("a", now)), Some(&last)),
            ToolStatus::Approved
        );
        assert_eq!(
            status_of(Some(&pin("a", PinState::Approved)), Some(&seen("b", now)), Some(&last)),
            ToolStatus::Changed
        );
        // Held after a change, it stays held when the server goes back, until it is approved.
        assert_eq!(
            status_of(Some(&pin("a", PinState::Changed)), Some(&seen("a", now)), Some(&last)),
            ToolStatus::Changed
        );
        assert_eq!(
            status_of(Some(&pin("a", PinState::Approved)), Some(&seen("a", earlier)), Some(&last)),
            ToolStatus::Gone
        );
        // A failed probe says nothing about what the server lists.
        assert_eq!(
            status_of(Some(&pin("a", PinState::Approved)), Some(&seen("a", earlier)), Some(&run(now, false))),
            ToolStatus::Approved
        );
        assert_eq!(
            status_of(Some(&pin("a", PinState::Disabled)), Some(&seen("b", now)), Some(&last)),
            ToolStatus::Disabled
        );
    }
}
