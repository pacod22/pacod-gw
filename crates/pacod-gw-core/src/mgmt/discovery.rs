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
use crate::model::{Doc, Family, Model, ModelState, Status, new_id};
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
    let started = Utc::now();
    let snap = st.config.snapshot();
    let mut result = serde_json::Map::new();
    let mut failed = 0;
    let mut providers: Vec<_> = snap.providers.values().filter(|p| p.doc.status == Status::Active).collect();
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
                "display_name": m.display_name, "context_window": m.context_window, "max_output_tokens": m.max_output_tokens
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
}

#[derive(Debug, Deserialize)]
struct ProposalQuery {
    /// `open` (default), `accepted`, `dismissed`, `resolved` or `all`.
    status: Option<String>,
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
    let rows = st.store.proposals((status != "all").then_some(status.as_str())).await?;
    Ok(Json(
        rows.into_iter()
            .map(|proposal| ProposalView {
                provider: snap.providers.get(&proposal.provider_id).map(|p| p.doc.name.clone()).unwrap_or_default(),
                proposal,
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

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AcceptInput {
    /// The new model's id. Defaults to `<provider>/<the provider's id>`.
    #[serde(default)]
    pub name: Option<String>,
    /// The capability preset to start from. Defaults to the family's usual one.
    #[serde(default)]
    pub capability_preset: Option<String>,
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
    let preset_name = input.capability_preset.unwrap_or_else(|| {
        if provider.doc.family == Family::Anthropic { "anthropic-adaptive" } else { "openai-compatible" }.into()
    });
    let capabilities = capability_presets()
        .into_iter()
        .find(|p| p.name == preset_name)
        .map(|p| p.capabilities)
        .ok_or_else(|| ApiError::invalid(format!("unknown capability preset \"{preset_name}\"")))?;
    let number = |key: &str| proposal.details.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    let model: Model = serde_json::from_value(json!({
        "name": input.name.unwrap_or_else(|| format!("{}/{}", provider.doc.name, proposal.upstream_id)),
        "provider_id": provider.id,
        "upstream_id": proposal.upstream_id,
        "display_name": proposal.details.get("display_name").and_then(Value::as_str).unwrap_or_default(),
        "context_window": number("context_window"),
        "max_output_tokens": number("max_output_tokens"),
        "capabilities": capabilities,
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
