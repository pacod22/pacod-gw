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
use super::crud::{Item, Params, ScopeIds, load};
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use super::resources::provider::{Discovered, discover};
use super::resources::repo::write_model_fields;
use crate::model::repo::{Change, Reported, capability_presets, model_fact};
use crate::model::{Doc, Family, Model, ModelKind, ModelState, Status, new_id};
use crate::store::AuditEntry;
use crate::store::discovery::Proposal;
use crate::store::jobs::JobRun;

/// The job's name in `jobs` and `job_runs`.
pub const JOB: &str = "discovery";

/// Asks every active provider for its model list and records what differs from the repository.
/// Returns the run's id. A run that breaks off is closed with what it found so far: a run left
/// open keeps every later one from starting.
pub async fn run(st: &AdminState, trigger: &str) -> anyhow::Result<String> {
    let run_id = new_id("run");
    st.store.job_start_run(JOB, &run_id, trigger, None).await?;
    st.announce("job", json!({ "job": JOB, "run_id": run_id, "status": "started", "trigger": trigger }));
    let mut result = serde_json::Map::new();
    let mut asked = ask_providers(st, &mut result).await;
    // The broker is one more source, after the providers' own lists (runbook section 20).
    if st.boot.broker.is_some() {
        match super::broker::propose(st).await {
            Ok(found) => {
                result.insert("broker".into(), found);
            }
            Err(e) => {
                result.insert("broker".into(), json!({ "error": format!("{e:#}") }));
                if let Ok(n) = &mut asked {
                    *n += 1;
                }
            }
        }
    }
    let ok = matches!(asked, Ok(0));
    let message = match &asked {
        Ok(0) => String::new(),
        Ok(failed) => format!("{failed} providers could not be asked"),
        Err(e) => format!("the run broke off: {e:#}"),
    };
    // When the run broke off because the database failed, closing it may fail as well. The next
    // round closes it then (`close_left_open`).
    let closed = st.store.job_finish_run(&run_id, ok, &message, &Value::Object(result)).await;
    if closed.is_ok() {
        st.announce(
            "job",
            json!({ "job": JOB, "run_id": run_id, "status": if ok { "finished" } else { "failed" }, "message": message }),
        );
    }
    asked?;
    closed?;
    Ok(run_id)
}

/// The work of a run: asks each provider and records the differences in `result`, by provider
/// name. Returns how many providers could not be asked.
async fn ask_providers(st: &AdminState, result: &mut serde_json::Map<String, Value>) -> anyhow::Result<usize> {
    // The proposals' times are the database's: so is the start that marks what this run saw.
    let started = st.store.discovery_clock().await?;
    let snap = st.config.snapshot();
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
            let details = details_of(m);
            st.store.proposal_seen(&new_id("dsp"), &provider.id, &m.upstream_id, "new", None, &details).await?;
            new += 1;
        }
        let mut updated = 0;
        for (model, listed) in repository_matches(&snap, provider, &found.listed) {
            let mut changes = listed.reported.differences(&model.doc, listed.context_window, listed.max_output_tokens);
            preset_changes(provider.doc.family, &model.doc, &listed.reported, &mut changes);
            // What a price page states is the provider's word on the model; the list does not
            // argue with it.
            changes.retain(|field, _| !model.doc.facts_from.contains_key(field));
            if changes.is_empty() {
                continue;
            }
            // `reported` alone decides whether a dismissed proposal opens again.
            let reported: serde_json::Map<String, Value> =
                changes.iter().map(|(k, c)| (k.clone(), c.reported.clone())).collect();
            let details = json!({ "model": model.doc.name, "changes": changes, "reported": reported });
            st.store
                .proposal_seen(
                    &new_id("dsp"),
                    &provider.id,
                    &model.doc.upstream_id,
                    "update",
                    Some(&model.id),
                    &details,
                )
                .await?;
            updated += 1;
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
            json!({ "listed": found.listed.len(), "new": new, "gone": found.missing.len(), "updated": updated, "resolved": resolved }),
        );
    }
    Ok(failed)
}

/// The repository models of a provider that its list names, with the entry that names each.
/// Retired models are left out: they are expected to stay as they were. Where several models
/// have one upstream id, the first by name is compared, because a proposal is kept per upstream
/// id; the others are edited by hand.
fn repository_matches<'a>(
    snap: &'a crate::snapshot::Snapshot,
    provider: &crate::snapshot::ProviderRt,
    listed: &'a [Discovered],
) -> Vec<(&'a crate::snapshot::ModelRt, &'a Discovered)> {
    let mut models: Vec<_> = snap
        .models
        .values()
        .filter(|m| m.doc.provider_id == provider.id && m.doc.state != ModelState::Retired)
        .collect();
    models.sort_by(|a, b| a.doc.name.cmp(&b.doc.name));
    let mut seen = std::collections::HashSet::new();
    models
        .into_iter()
        .filter_map(|m| Some((m.as_ref(), listed.iter().find(|d| d.upstream_id == m.doc.upstream_id)?)))
        .filter(|(m, _)| seen.insert(m.doc.upstream_id.clone()))
        .collect()
}

/// Capabilities that no provider reports and that the preset the gateway suggests for a model
/// turns on, where the model has them off: JSON mode on a model without JSON-schema output
/// (where it lets the gateway emulate one), and structured output where the provider says
/// nothing about it. A model stored before its preset had them gets them this way, once an
/// admin applies the proposal. Only ever on: a capability an admin switched off stays off.
fn preset_changes(
    family: Family,
    model: &Model,
    reported: &Reported,
    changes: &mut std::collections::BTreeMap<String, Change>,
) {
    let (kind, preset) = reported.suggestion(family, &model.upstream_id);
    // A model of another kind than the suggestion is not what the preset describes.
    let Some(preset) = capability_presets().into_iter().find(|p| p.name == preset).filter(|_| kind == model.kind)
    else {
        return;
    };
    let (have, suggested) = (&model.capabilities, &preset.capabilities);
    let mut propose = |field: &str| {
        changes.entry(field.to_owned()).or_insert_with(|| Change {
            current: json!(false),
            reported: json!(true),
            source: "preset".into(),
        });
    };
    // With JSON-schema output of its own, JSON mode changes nothing that matters to a caller.
    if suggested.json_mode && !have.json_mode && !have.structured_output && family != Family::Anthropic {
        propose("capabilities.json_mode");
    }
    if suggested.structured_output && !have.structured_output && reported.structured_output.is_none() {
        propose("capabilities.structured_output");
    }
}

/// What a proposal keeps of a listed model: its name and limits, and what the provider says
/// it can do.
fn details_of(m: &Discovered) -> Value {
    let mut details = serde_json::to_value(&m.reported).unwrap_or_else(|_| json!({}));
    details["display_name"] = json!(m.display_name);
    details["context_window"] = json!(m.context_window);
    details["max_output_tokens"] = json!(m.max_output_tokens);
    details
}

/// What a proposal says the provider reported.
fn reported_of(proposal: &Proposal) -> Reported {
    serde_json::from_value(proposal.details.clone()).unwrap_or_default()
}

/// What a run that was closed by a later round says.
pub const LEFT_OPEN: &str = "did not finish: the instance running it stopped, or could not record its end";

/// Closes a run that is still open when an upkeep round begins. Runs happen only in that round,
/// which one management instance at a time holds under a lease it keeps while the round lasts,
/// and the rounds of one instance follow each other: an open run was left by an instance that
/// stopped during it, or that could not record its end. Left open, it would keep every later run
/// from starting, asked for or scheduled.
pub async fn close_left_open(st: &AdminState) -> anyhow::Result<()> {
    let Some(open) = st.store.job_state(JOB).await?.current_run else { return Ok(()) };
    if st.store.job_finish_run(&open, false, LEFT_OPEN, &json!({})).await? {
        st.announce("job", json!({ "job": JOB, "run_id": open, "status": "failed", "message": LEFT_OPEN }));
    }
    Ok(())
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
    /// Only those this source found: `provider` or `broker`.
    source: Option<String>,
}

/// What discovery suggests for a proposal of a new model.
fn suggest(family: Family, proposal: &Proposal) -> (ModelKind, &'static str) {
    reported_of(proposal).suggestion(family, &proposal.upstream_id)
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
            .filter(|proposal| q.source.as_deref().is_none_or(|s| s == proposal.source))
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
    let (suggested_kind, suggested_preset) = suggest(provider.doc.family, &proposal);
    let chosen = input.capability_preset.is_some();
    let preset_name = input.capability_preset.unwrap_or_else(|| suggested_preset.into());
    let kind = input.kind.unwrap_or(suggested_kind);
    let mut capabilities = capability_presets()
        .into_iter()
        .find(|p| p.name == preset_name)
        .map(|p| p.capabilities)
        .ok_or_else(|| ApiError::invalid(format!("unknown capability preset \"{preset_name}\"")))?;
    // What the provider says about the model wins over the suggested preset's guesses, not
    // over a preset the administrator chose.
    if !chosen {
        reported_of(&proposal).apply_to(&mut capabilities);
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
        // The broker resource the model is, when the broker listed it.
        "broker_resource_id": proposal.details.get("broker_resource_id").and_then(Value::as_str),
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
    // A draft cannot be activated without a price: the tracker looks for one now rather than
    // at its next scheduled run.
    super::resources::pricejob::request_run_for_model(&st, &snap, &model_id, &model).await;
    let obj = row.into_obj::<Model>()?;
    Ok((StatusCode::CREATED, Json(Item { meta: obj.meta(), data: obj.doc })))
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ApplyInput {
    /// The changes to apply, by the names the proposal gives them. Defaults to all of them.
    #[serde(default)]
    pub fields: Option<Vec<String>>,
}

/// Applies an update proposal to its model: what the provider now reports, or what the
/// suggested preset turns on. Refused when a field has changed since discovery saw it, so that
/// a hand edit is not overwritten by an older reading.
async fn apply(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    body: Option<Json<ApplyInput>>,
) -> ApiResult<Json<Item<Model>>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let input = body.map(|b| b.0).unwrap_or_default();
    let id = params.get("id").ok_or_else(|| ApiError::not_found("proposal"))?;
    let proposal = st.store.proposal(id).await?.ok_or_else(|| ApiError::not_found("proposal"))?;
    if proposal.kind != "update" || proposal.status != "open" {
        return Err(ApiError::conflict("only an open update proposal can be applied"));
    }
    let changes: std::collections::BTreeMap<String, Change> =
        serde_json::from_value(proposal.details["changes"].clone()).map_err(ApiError::internal)?;
    let fields = input.fields.unwrap_or_else(|| changes.keys().cloned().collect());
    if fields.is_empty() {
        return Err(ApiError::invalid("fields names nothing to apply"));
    }
    let model_id = proposal.model_id.clone().ok_or_else(|| ApiError::conflict("the proposal names no model"))?;
    let scope = ScopeIds::default();
    let existing = load::<Model>(&st, &model_id, &scope).await?;
    let mut values = Vec::new();
    for field in &fields {
        let change =
            changes.get(field).ok_or_else(|| ApiError::invalid(format!("the proposal does not change \"{field}\"")))?;
        if model_fact(&existing.doc, field).as_ref() != Some(&change.current) {
            return Err(ApiError::conflict(format!(
                "the model's {field} changed after discovery read it; run discovery again"
            )));
        }
        values.push((field.clone(), change.reported.clone()));
    }
    // The model's own rules apply as to any other change of it.
    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(None, None);
    audit.action = "update_from_discovery".into();
    audit.detail = json!({ "proposal": id, "fields": fields });
    let obj = write_model_fields(&st, &mut tx, &existing, &values, &p.id, None, audit).await?;
    st.commit(tx).await?;
    st.store.proposal_decide(id, "accepted", &p.id).await?;
    Ok(Json(Item { meta: obj.meta(), data: obj.doc }))
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
    let apply_in = api.schema::<ApplyInput>();
    let model = api.schema::<Item<Model>>();
    let array = |item: Value| json!({ "type": "array", "items": item });
    api.add(
        api.op("get", "/discovery/proposals", "listDiscoveryProposals", "models", "What model discovery found")
            .query("status", "open (default), accepted, dismissed, resolved or all.")
            .query("provider_id", "Only this provider's proposals.")
            .query("source", "Only those this source found: provider or broker.")
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
        .created(model.clone()),
    );
    api.add(
        api.op(
            "post",
            "/discovery/proposals/{id}/apply",
            "applyDiscoveryProposal",
            "models",
            "Apply an update proposal to its model",
        )
        .body(apply_in)
        .ok(model),
    );
    api.add(
        api.op("post", "/discovery/proposals/{id}/dismiss", "dismissDiscoveryProposal", "models", "Dismiss a proposal")
            .no_content(),
    );
    router
        .route("/discovery/proposals", get(proposals))
        .route("/discovery/runs", get(runs).post(run_now))
        .route("/discovery/proposals/{id}/accept", post(accept))
        .route("/discovery/proposals/{id}/apply", post(apply))
        .route("/discovery/proposals/{id}/dismiss", post(dismiss))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn stored(preset: &str, upstream_id: &str, json_mode: bool) -> Model {
        let mut caps = capability_presets().into_iter().find(|p| p.name == preset).unwrap().capabilities;
        caps.json_mode = json_mode;
        serde_json::from_value(json!({
            "name": format!("p/{upstream_id}"), "provider_id": "prv", "upstream_id": upstream_id, "capabilities": caps
        }))
        .unwrap()
    }

    fn proposed(family: Family, model: &Model) -> Vec<String> {
        let mut changes = BTreeMap::new();
        preset_changes(family, model, &Reported::default(), &mut changes);
        changes.into_keys().collect()
    }

    #[test]
    fn json_mode_is_proposed_where_it_lets_the_gateway_emulate_a_schema() {
        // DeepSeek and GLM stored before their presets had JSON mode.
        assert_eq!(proposed(Family::Openai, &stored("deepseek", "deepseek-v4-pro", false)), ["capabilities.json_mode"]);
        assert_eq!(proposed(Family::Openai, &stored("glm-5", "glm-5.3", false)), ["capabilities.json_mode"]);
        // A model with JSON-schema output of its own gains nothing a caller sees.
        assert!(proposed(Family::Openai, &stored("openai-chat", "gpt-4.1-nano", false)).is_empty());
        assert!(proposed(Family::Gemini, &stored("gemini-3-flash", "gemini-3.8-flash", false)).is_empty());
        // Nor when it already has it.
        assert!(proposed(Family::Openai, &stored("deepseek", "deepseek-v4-pro", true)).is_empty());
    }
}
