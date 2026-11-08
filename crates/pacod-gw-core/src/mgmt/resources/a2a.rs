//! Remote agents of an organisation (runbook section 21.4): registering them, their credentials,
//! reading their card, and approving it (catalogue ING-14, MCP-15).
//!
//! As with tool servers, what a probe read is kept outside the configuration; an approval pins
//! the card (`agent_card`), and a later probe that finds the card changed holds the agent until
//! the card is approved again: a card says what the agent does and what it takes, and callers
//! are served the approved one.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::tools::{
    ProbeRunView, ToolCredentialInput, ToolCredentialStateInput, ToolCredentialView, ToolStatus, check_auth,
    create_credential_for, delete_credential_for, list_credentials_for, owner_of, set_credential_state_for,
};
use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{BuildCtx, Item, Ops, Params, Resource, ScopeKind, register};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{AgentCardPin, ConnectorCredential, Doc, Obj, PinState, RemoteAgent, new_id};
use crate::snapshot::Snapshot;
use crate::store::Tx;
use crate::tools::card::{self, VERSION};
use crate::tools::client::Target;
use crate::tools::names;

const AGENT: (&str, &str) = (RemoteAgent::KIND, "remote agent");
/// The item a probe of an agent records: its card.
const CARD: &str = "agent-card";
/// The largest card read: cards with many skills are long.
const MAX_CARD_BYTES: usize = 1 << 20;

impl Resource for RemoteAgent {
    const PLURAL: &'static str = "remote-agents";
    const LABEL: &'static str = "remote agent";
    const SCOPE: ScopeKind = ScopeKind::Organisation;
    const STRICT: bool = true;
    type Input = RemoteAgent;
    type View = RemoteAgent;

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
            return Err(ApiError::invalid("a remote agent keeps its name: callers reach it by it"));
        }
        input.card_url = input.card_url.trim().to_owned();
        let hub =
            ctx.st.gw.tools.as_ref().ok_or_else(|| ApiError::unavailable("this gateway calls no remote agents"))?;
        hub.egress.check_url(&input.card_url).map_err(|e| ApiError::invalid(format!("card_url: {e}")))?;
        check_auth(&mut input.auth)?;
        if !(1_000..=600_000).contains(&input.timeout_ms) {
            return Err(ApiError::invalid("timeout_ms must be between 1000 and 600000"));
        }
        if !(1024..=16 << 20).contains(&input.max_bytes) {
            return Err(ApiError::invalid("max_bytes must be between 1024 and 16777216"));
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
            "/a2a/{} stops answering, and its credentials and approved card are deleted with it.",
            obj.doc.name
        )]
    }

    /// The agent's approved card and credentials go with it.
    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let org = obj.parent.clone();
        let entry = || crate::store::AuditEntry {
            organisation_id: org.clone(),
            ..crate::store::AuditEntry::system("remote_agent_deleted")
        };
        for row in tx.list(AgentCardPin::KIND, Some(&obj.id)).await? {
            tx.delete(&row.id, entry()).await?;
        }
        for row in tx.list(ConnectorCredential::KIND, Some(&obj.id)).await? {
            tx.delete(&row.id, entry()).await?;
        }
        Ok(())
    }
}

// --- credentials: those of tool servers -------------------------------------------------------

async fn create_credential(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<ToolCredentialInput>,
) -> ApiResult<(StatusCode, Json<Item<ToolCredentialView>>)> {
    create_credential_for(&st, &p, &params, input, AGENT.0, AGENT.1).await
}

async fn list_credentials(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<Vec<Item<ToolCredentialView>>>> {
    list_credentials_for(&st, &p, &params, AGENT.0, AGENT.1).await
}

async fn set_credential_state(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<ToolCredentialStateInput>,
) -> ApiResult<Json<Item<ToolCredentialView>>> {
    set_credential_state_for(&st, &p, &params, input, AGENT.0, AGENT.1).await
}

async fn delete_credential(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<StatusCode> {
    delete_credential_for(&st, &p, &params, AGENT.0, AGENT.1).await
}

// --- the card ---------------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct SeenCardView {
    /// The card as the agent serves it now, without its signatures.
    pub card: Value,
    pub hash: String,
    pub interface_url: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// What the gateway knows of an agent's card.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CardView {
    /// The last probe; `null` before the first.
    pub probe: Option<ProbeRunView>,
    /// `new` (never approved), `approved`, `changed` (the agent serves another card than the
    /// approved one: held), or `disabled`.
    pub status: ToolStatus,
    pub seen: Option<SeenCardView>,
    pub pin: Option<AgentCardPin>,
}

async fn card_view(st: &AdminState, agent_id: &str) -> ApiResult<CardView> {
    let (run, seen) = st.store.probes_of(agent_id).await.map_err(ApiError::internal)?;
    let seen = seen.into_iter().find(|s| s.item == CARD);
    let mut tx = st.store.begin().await?;
    let pin: Option<AgentCardPin> = tx
        .list(AgentCardPin::KIND, Some(agent_id))
        .await?
        .into_iter()
        .next()
        .map(|r| r.into_obj())
        .transpose()?
        .map(|o| o.doc);
    drop(tx);
    let status = match (&pin, &seen) {
        (None, _) => ToolStatus::New,
        (Some(p), _) if p.state == PinState::Disabled => ToolStatus::Disabled,
        (Some(p), Some(s)) if p.state == PinState::Changed || s.hash != p.hash => ToolStatus::Changed,
        (Some(_), _) => ToolStatus::Approved,
    };
    let seen = seen.and_then(|s| {
        let read = card::read_card(s.definition.clone()).ok()?;
        Some(SeenCardView {
            card: s.definition,
            hash: s.hash,
            interface_url: read.interface_url,
            first_seen: s.first_seen,
            last_seen: s.last_seen,
        })
    });
    Ok(CardView {
        probe: run.map(|r| ProbeRunView {
            probed_at: r.probed_at,
            version: r.version,
            ok: r.ok,
            problem: r.problem,
            tools: r.tools,
        }),
        status,
        seen,
        pin,
    })
}

async fn get_card(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<CardView>> {
    let snap = st.config.snapshot();
    let owner = owner_of(&st, &p, &params, &snap, AGENT.0, AGENT.1).await?;
    Ok(Json(card_view(&st, &owner.id).await?))
}

/// Reads an active agent's card now and records it, holding the approved card if it changed.
/// Returns the probe's problem, if any.
pub async fn probe_agent(st: &AdminState, agent_id: &str) -> anyhow::Result<Option<String>> {
    let snap = st.config.snapshot();
    let Some(rt) = snap.organisations.values().find_map(|o| o.remote_agents.values().find(|a| a.id == agent_id)) else {
        anyhow::bail!("the remote agent is not active");
    };
    let hub = st.gw.tools.as_ref().ok_or_else(|| anyhow::anyhow!("this gateway calls no remote agents"))?;
    let target = Target {
        url: &rt.doc.card_url,
        auth: rt
            .credential
            .as_ref()
            .map(|s| (rt.doc.auth.header.as_str(), format!("{}{}", rt.doc.auth.prefix, s.value.as_str()))),
        timeout: std::time::Duration::from_millis(u64::from(rt.doc.timeout_ms.min(30_000))),
        max_bytes: MAX_CARD_BYTES,
    };
    let now = Utc::now();
    let read = match hub.client.get_json(&target).await.map_err(|e| e.to_string()).and_then(card::read_card) {
        Ok(read) => read,
        Err(problem) => {
            st.store.probe_failed(agent_id, &problem, now).await?;
            return Ok(Some(problem));
        }
    };
    st.store.probe_record(agent_id, VERSION, &[(CARD.to_owned(), read.card.clone(), read.hash.clone())], now).await?;
    // An approved card that is no longer what the agent serves is held.
    let mut tx = st.store.begin().await?;
    if let Some(row) = tx.list(AgentCardPin::KIND, Some(agent_id)).await?.into_iter().next() {
        let mut pin: Obj<AgentCardPin> = row.into_obj()?;
        if pin.doc.state == PinState::Approved && pin.doc.hash != read.hash {
            pin.doc.state = PinState::Changed;
            let audit = crate::store::AuditEntry {
                organisation_id: Some(rt.organisation_id.clone()),
                action: "card_changed".into(),
                detail: serde_json::json!({ "approved_hash": pin.doc.hash }),
                ..crate::store::AuditEntry::system("agent_probe")
            };
            tx.update(&pin.id, Some(pin.version), None, &serde_json::to_value(&pin.doc)?, audit).await?;
            st.commit(tx).await.map_err(|e| anyhow::anyhow!(e.message))?;
            tracing::warn!(agent = agent_id, "a remote agent's card changed; the agent is held until approved again");
        }
    }
    Ok(None)
}

async fn probe_now(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<CardView>> {
    let snap = st.config.snapshot();
    let owner = owner_of(&st, &p, &params, &snap, AGENT.0, AGENT.1).await?;
    if !p.can_admin_org(&owner.org) {
        return Err(ApiError::forbidden());
    }
    let active = snap.organisations.get(&owner.org).is_some_and(|o| o.remote_agents.values().any(|a| a.id == owner.id));
    if !active {
        return Err(ApiError::conflict("the remote agent is disabled; enable it to read its card"));
    }
    probe_agent(&st, &owner.id).await.map_err(ApiError::internal)?;
    Ok(Json(card_view(&st, &owner.id).await?))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApproveCardInput {
    /// The hash of the card approved, as the card view shows it. When the agent served another
    /// card since, the approval is refused (`409`).
    pub hash: String,
}

async fn approve(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<ApproveCardInput>,
) -> ApiResult<Json<CardView>> {
    let snap = st.config.snapshot();
    let owner = owner_of(&st, &p, &params, &snap, AGENT.0, AGENT.1).await?;
    if !p.can_admin_org(&owner.org) {
        return Err(ApiError::forbidden());
    }
    let (_, seen) = st.store.probes_of(&owner.id).await.map_err(ApiError::internal)?;
    let seen = seen.into_iter().find(|s| s.item == CARD).ok_or_else(|| ApiError::conflict("read the card first"))?;
    if seen.hash != input.hash {
        return Err(ApiError::conflict("the agent serves another card now; read it again"));
    }
    let read = card::read_card(seen.definition).map_err(ApiError::invalid)?;
    // The card names where the agent is called: an address of its choosing, held to the list.
    let hub = st.gw.tools.as_ref().ok_or_else(|| ApiError::unavailable("this gateway calls no remote agents"))?;
    hub.egress.check_url(&read.interface_url).map_err(|e| ApiError::invalid(format!("the card's interface: {e}")))?;
    let doc = AgentCardPin {
        card: read.card,
        hash: read.hash,
        interface_url: read.interface_url,
        tenant: read.tenant,
        state: PinState::Approved,
        approved_by: p.id.clone(),
        approved_at: Utc::now(),
    };
    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(Some(&owner.org), None);
    audit.action = "approve".into();
    let value = serde_json::to_value(&doc).map_err(ApiError::internal)?;
    match tx.list(AgentCardPin::KIND, Some(&owner.id)).await?.into_iter().next() {
        Some(row) => {
            tx.update(&row.id, Some(row.version), Some("card"), &value, audit).await?;
        }
        None => {
            tx.create(AgentCardPin::KIND, &new_id(AgentCardPin::PREFIX), Some(&owner.id), Some("card"), &value, audit)
                .await?;
        }
    }
    st.commit(tx).await?;
    Ok(Json(card_view(&st, &owner.id).await?))
}

/// Reads the cards of active agents whose last probe is older than the global setting
/// `tool_probe_interval_hours`, a few per round.
pub async fn upkeep(st: &AdminState) {
    let hours = st.config.snapshot().settings.tool_probe_interval_hours;
    if hours == 0 {
        return;
    }
    let runs = match st.store.probe_runs().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "upkeep: reading the remote agents' probes failed");
            return;
        }
    };
    let snap = st.config.snapshot();
    let due_before = Utc::now() - Duration::hours(i64::from(hours));
    let due: Vec<String> = snap
        .organisations
        .values()
        .flat_map(|o| o.remote_agents.values().map(|a| a.id.clone()))
        .filter(|id| runs.iter().find(|r| &r.owner_id == id).is_none_or(|r| r.probed_at < due_before))
        .take(5)
        .collect();
    for id in due {
        match probe_agent(st, &id).await {
            Ok(None) => {}
            Ok(Some(problem)) => tracing::warn!(agent = %id, %problem, "upkeep: reading a remote agent's card failed"),
            Err(e) => tracing::warn!(agent = %id, error = %e, "upkeep: probing a remote agent failed"),
        }
    }
}

pub fn routes(mut router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    router = register::<RemoteAgent>(router, api, Ops::ALL);
    let base = "/orgs/{org}/remote-agents/{id}";
    let coll = format!("{base}/credentials");
    let one = format!("{base}/credentials/{{cid}}");
    let cred = api.schema::<Item<ToolCredentialView>>();
    let cred_input = api.schema::<ToolCredentialInput>();
    let state_input = api.schema::<ToolCredentialStateInput>();
    api.add(
        api.op(
            "get",
            &coll,
            "listRemoteAgentCredentials",
            "remote agents",
            "List a remote agent's credentials (no secrets)",
        )
        .ok(serde_json::json!({ "type": "array", "items": cred })),
    );
    api.add(
        api.op(
            "post",
            &coll,
            "createRemoteAgentCredential",
            "remote agents",
            "Add or rotate a remote agent's credential",
        )
        .body(cred_input)
        .created(cred.clone()),
    );
    api.add(
        api.op(
            "post",
            &format!("{one}/state"),
            "setRemoteAgentCredentialState",
            "remote agents",
            "Make a credential current or disable it",
        )
        .body(state_input)
        .ok(cred),
    );
    api.add(
        api.op("delete", &one, "deleteRemoteAgentCredential", "remote agents", "Delete a remote agent's credential")
            .no_content(),
    );
    let view = api.schema::<CardView>();
    api.add(
        api.op(
            "get",
            &format!("{base}/card"),
            "getRemoteAgentCard",
            "remote agents",
            "The agent's card: as it serves it, and as approved",
        )
        .ok(view.clone()),
    );
    api.add(
        api.op("post", &format!("{base}/probe"), "probeRemoteAgent", "remote agents", "Read the agent's card now")
            .ok(view.clone()),
    );
    let approve_input = api.schema::<ApproveCardInput>();
    api.add(
        api.op(
            "post",
            &format!("{base}/card/approve"),
            "approveRemoteAgentCard",
            "remote agents",
            "Approve the agent's card as it serves it now",
        )
        .body(approve_input)
        .ok(view),
    );
    router
        .route(&coll, get(list_credentials).post(create_credential))
        .route(&one, delete(delete_credential))
        .route(&format!("{one}/state"), post(set_credential_state))
        .route(&format!("{base}/card"), get(get_card))
        .route(&format!("{base}/probe"), post(probe_now))
        .route(&format!("{base}/card/approve"), post(approve))
}
