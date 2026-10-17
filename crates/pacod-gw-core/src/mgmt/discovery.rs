//! Model discovery on a schedule (concept section 6.4). A run asks every active provider for
//! its model list. A model it lists that the repository lacks becomes a proposal for a draft;
//! a repository model it no longer lists is flagged. Nothing is created or activated without
//! an admin accepting a proposal.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::AdminState;
use super::auth::Principal;
use super::crud::{Item, Params};
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use super::resources::provider::discover;
use crate::model::repo::capability_presets;
use crate::model::{Doc, Family, Model, ModelKind, ModelState, Status, new_id};
use crate::store::AuditEntry;
use crate::store::discovery::Proposal;
use crate::store::jobs::JobRun;

/// The job's name in `jobs` and `job_runs`.
pub const JOB: &str = "discovery";

/// Asks every active provider for its model list and records what differs from the repository.
/// Returns the run's id.
pub async fn run(st: &AdminState, trigger: &str) -> anyhow::Result<String> {
    let run_id = new_id("run");
    st.store.job_start_run(JOB, &run_id, trigger, None).await?;
    st.announce("job", json!({ "job": JOB, "run_id": run_id, "status": "started", "trigger": trigger }));
    // The proposals' times are the database's: so is the start that marks what this run saw.
    let started = st.store.discovery_clock().await?;
    let snap = st.config.snapshot();
    let mut result = serde_json::Map::new();
    let mut failed = 0;
    // Vertex AI's list is not read yet (see `discover`); its providers are left out of runs.
    let mut providers: Vec<_> =
        snap.providers.values().filter(|p| p.doc.status == Status::Active && p.vertex.is_none()).collect();
    providers.sort_by(|a, b| a.doc.name.cmp(&b.doc.name));
    for provider in providers {
        let found = match discover(st, &snap, provider).await {
            Ok(d) => d,
            Err(e) => {
                failed += 1;
                result.insert(provider.doc.name.clone(), json!({ "error": e.message }));
                continue;
            }
        };
        let mut new = 0;
        for m in found.listed.iter().filter(|m| m.model.is_none()) {
            let details = json!({
                "display_name": m.display_name, "context_window": m.context_window, "max_output_tokens": m.max_output_tokens,
                "vision": m.vision, "reasons": m.reasons, "embedding": m.embedding
            });
            st.store.proposal_seen(&new_id("dsp"), &provider.id, &m.upstream_id, "new", None, &details).await?;
            new += 1;
        }
        for name in &found.missing {
            if let Some(model) = snap.model_by_name(name) {
                let details = json!({ "model": name });
                st.store
                    .proposal_seen(
                        &new_id("dsp"),
                        &provider.id,
                        &model.doc.upstream_id,
                        "gone",
                        Some(&model.id),
                        &details,
                    )
                    .await?;
            }
        }
        let resolved = st.store.proposals_resolve_unseen(&provider.id, started).await?;
        result.insert(
            provider.doc.name.clone(),
            json!({ "listed": found.listed.len(), "new": new, "gone": found.missing.len(), "resolved": resolved }),
        );
    }
    let message = if failed > 0 { format!("{failed} providers could not be asked") } else { String::new() };
    st.store.job_finish_run(&run_id, failed == 0, &message, &Value::Object(result)).await?;
    st.announce(
        "job",
        json!({ "job": JOB, "run_id": run_id, "status": if failed == 0 { "finished" } else { "failed" }, "message": message }),
    );
    Ok(run_id)
}

/// Whether a run is due: asked for, or the interval since the last one has passed.
pub async fn due(st: &AdminState) -> anyhow::Result<bool> {
    let state = st.store.job_state(JOB).await?;
    if state.current_run.is_some() {
        return Ok(false);
    }
    if state.run_requested {
        return Ok(true);
    }
    let hours = st.config.snapshot().settings.discovery_interval_hours;
    if hours == 0 || !st.boot.scheduled_discovery {
        return Ok(false);
    }
    Ok(state.last_started_at.is_none_or(|t| Utc::now() - t >= chrono::Duration::hours(i64::from(hours))))
}

// --- the API ---------------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProposalView {
    #[serde(flatten)]
    pub proposal: Proposal,
    /// The provider's name.
    pub provider: String,
    /// For a new model: the capability preset accepting it starts from, unless another is
    /// chosen. Follows from the model's id and what the provider reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_preset: Option<String>,
    /// For a new model: the kind it is accepted as, unless another is chosen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_kind: Option<ModelKind>,
}

#[derive(Debug, Deserialize)]
struct ProposalQuery {
    /// `open` (default), `accepted`, `dismissed`, `resolved` or `all`.
    status: Option<String>,
    /// Only this provider's proposals.
    provider_id: Option<String>,
}

/// What discovery suggests for a proposal of a new model.
fn suggest(family: Family, proposal: &Proposal) -> (ModelKind, &'static str) {
    let flag = |key: &str| proposal.details.get(key).and_then(Value::as_bool);
    crate::model::repo::suggestion(family, &proposal.upstream_id, flag("vision"), flag("reasons"), flag("embedding"))
}

async fn proposals(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<ProposalQuery>,
) -> ApiResult<Json<Vec<ProposalView>>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let status = q.status.unwrap_or_else(|| "open".into());
    let snap = st.config.snapshot();
    let rows = st.store.proposals((status != "all").then_some(status.as_str()), q.provider_id.as_deref()).await?;
    Ok(Json(
        rows.into_iter()
            .map(|proposal| {
                let provider = snap.providers.get(&proposal.provider_id);
                let suggested = provider.filter(|_| proposal.kind == "new").map(|p| suggest(p.doc.family, &proposal));
                ProposalView {
                    provider: provider.map(|p| p.doc.name.clone()).unwrap_or_default(),
                    suggested_preset: suggested.map(|(_, preset)| preset.to_owned()),
                    suggested_kind: suggested.map(|(kind, _)| kind),
                    proposal,
                }
            })
            .collect(),
    ))
}

async fn runs(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<Vec<JobRun>>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    Ok(Json(st.store.job_runs(JOB, 30).await?))
}

/// Asks for a run; a management instance starts it within half a minute.
async fn run_now(State(st): State<AdminState>, p: Principal) -> ApiResult<StatusCode> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    st.store.job_request_run(JOB).await?;
    Ok(StatusCode::ACCEPTED)
}

/// How a model accepted from discovery is named when no name is given.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Naming {
    /// `<provider>/<the provider's id>`.
    #[default]
    Prefixed,
    /// The provider's id as it is, for example `gemini-3.8-flash` (as some applications name Gemini models).
    Bare,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AcceptInput {
    /// The new model's id. Defaults to what `naming` says.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub naming: Naming,
    /// The capability preset to start from. Defaults to the one the model's id and what the
    /// provider reported suggest (`suggested_preset` of the proposal).
    #[serde(default)]
    pub capability_preset: Option<String>,
    /// The kind of model. Defaults to `suggested_kind` of the proposal.
    #[serde(default)]
    pub kind: Option<ModelKind>,
}

/// Creates a draft model from a proposal of a new model.
async fn accept(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    body: Option<Json<AcceptInput>>,
) -> ApiResult<(StatusCode, Json<Item<Model>>)> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let input = body.map(|b| b.0).unwrap_or_default();
    let id = params.get("id").ok_or_else(|| ApiError::not_found("proposal"))?;
    let proposal = st.store.proposal(id).await?.ok_or_else(|| ApiError::not_found("proposal"))?;
    if proposal.kind != "new" || proposal.status != "open" {
        return Err(ApiError::conflict("only an open proposal of a new model can be accepted; dismiss the others"));
    }
    let snap = st.config.snapshot();
    let provider =
        snap.providers.get(&proposal.provider_id).ok_or_else(|| ApiError::conflict("the provider no longer exists"))?;
    let flag = |key: &str| proposal.details.get(key).and_then(Value::as_bool);
    let (suggested_kind, suggested_preset) = suggest(provider.doc.family, &proposal);
    let chosen = input.capability_preset.is_some();
    let preset_name = input.capability_preset.unwrap_or_else(|| suggested_preset.into());
    let kind = input.kind.unwrap_or(suggested_kind);
    let mut capabilities = capability_presets()
        .into_iter()
        .find(|p| p.name == preset_name)
        .map(|p| p.capabilities)
        .ok_or_else(|| ApiError::invalid(format!("unknown capability preset \"{preset_name}\"")))?;
    // What the provider says about images wins over the suggested preset's guess, not over a
    // preset the administrator chose.
    if let (Some(v), false) = (flag("vision"), chosen) {
        capabilities.vision = v;
    }
    let number = |key: &str| proposal.details.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    let upstream = proposal.upstream_id.trim_start_matches("models/");
    let name = input.name.unwrap_or_else(|| match input.naming {
        Naming::Prefixed => format!("{}/{upstream}", provider.doc.name),
        Naming::Bare => upstream.to_owned(),
    });
    // A model on one's own Ollama server costs nothing; Ollama Cloud bills.
    let free = provider.doc.family == Family::Ollama && !provider.doc.base_url.contains("ollama.com");
    let model: Model = serde_json::from_value(json!({
        "name": name,
        "free": free,
        "provider_id": provider.id,
        "upstream_id": proposal.upstream_id,
        "display_name": proposal.details.get("display_name").and_then(Value::as_str).unwrap_or_default(),
        "context_window": number("context_window"),
        "max_output_tokens": number("max_output_tokens"),
        "capabilities": capabilities,
        "kind": kind,
        "state": ModelState::Draft,
        "notes": format!("proposed by model discovery on {}", proposal.first_seen.date_naive()),
    }))
    .map_err(ApiError::internal)?;
    if !crate::model::repo::is_model_name(&model.name) {
        return Err(ApiError::invalid("name may contain letters, digits and / - _ . : and must not start with @"));
    }
    let model_id = new_id(Model::PREFIX);
    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(None, None);
    audit.action = "create_from_discovery".into();
    let row = tx
        .create(
            Model::KIND,
            &model_id,
            None,
            Some(&model.name),
            &serde_json::to_value(&model).map_err(ApiError::internal)?,
            audit,
        )
        .await?;
    st.commit(tx).await?;
    st.store.proposal_decide(id, "accepted", &p.id).await?;
    let obj = row.into_obj::<Model>()?;
    Ok((StatusCode::CREATED, Json(Item { meta: obj.meta(), data: obj.doc })))
}

async fn dismiss(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<StatusCode> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("proposal"))?;
    if !st.store.proposal_decide(id, "dismissed", &p.id).await? {
        return Err(ApiError::conflict("the proposal is not open"));
    }
    let mut entry: AuditEntry = p.audit(None, None);
    entry.action = "dismiss_discovery_proposal".into();
    entry.object_id = id.clone();
    st.store.audit(&entry).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let proposal = api.schema::<ProposalView>();
    let run = api.schema::<JobRun>();
    let accept_in = api.schema::<AcceptInput>();
    let model = api.schema::<Item<Model>>();
    let array = |item: Value| json!({ "type": "array", "items": item });
    api.add(
        api.op("get", "/discovery/proposals", "listDiscoveryProposals", "models", "What model discovery found")
            .query("status", "open (default), accepted, dismissed, resolved or all.")
            .query("provider_id", "Only this provider's proposals.")
            .ok(array(proposal)),
    );
    api.add(
        api.op("get", "/discovery/runs", "listDiscoveryRuns", "models", "Runs of model discovery, newest first")
            .ok(array(run)),
    );
    api.add(
        api.op("post", "/discovery/runs", "runDiscovery", "models", "Run model discovery now")
            .response("202", "Accepted", None),
    );
    api.add(
        api.op(
            "post",
            "/discovery/proposals/{id}/accept",
            "acceptDiscoveryProposal",
            "models",
            "Create a draft model from a proposal",
        )
        .body(accept_in)
        .created(model),
    );
    api.add(
        api.op("post", "/discovery/proposals/{id}/dismiss", "dismissDiscoveryProposal", "models", "Dismiss a proposal")
            .no_content(),
    );
    router
        .route("/discovery/proposals", get(proposals))
        .route("/discovery/runs", get(runs).post(run_now))
        .route("/discovery/proposals/{id}/accept", post(accept))
        .route("/discovery/proposals/{id}/dismiss", post(dismiss))
}
