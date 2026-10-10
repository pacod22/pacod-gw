//! The model repository: models and their life cycle, capability presets, grants and prices
//! (concept sections 6 and 13.1).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{BuildCtx, Item, Ops, Page, Params, Resource, ScopeIds, ScopeKind, register};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::{OpenApi, page_schema};
use crate::model::Modalities;
use crate::model::repo::{CapabilityPreset, capability_presets, is_model_name};
use crate::model::{
    AuthScheme, Capabilities, Doc, Family, Model, ModelKind, ModelState, Obj, OrgGrant, Price, PriceSource,
    PriceStatus, Rates, Status, WorkspaceGrant, new_id,
};
use crate::snapshot::Snapshot;
use crate::store::{ParentFilter, Tx};

// --- models ----------------------------------------------------------------------------------

/// What can be set on a model. Its state changes through the transition endpoint.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ModelInput {
    /// The id applications use, for example `anthropic/claude-sonnet-5-5`.
    pub name: String,
    pub provider_id: String,
    /// The id the provider knows the model by.
    pub upstream_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub kind: ModelKind,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// Start the capability profile from this preset (see `GET /capability-presets`).
    #[serde(default)]
    pub capability_preset: Option<String>,
    /// The capability profile. When both this and a preset are given, this wins.
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
    /// The model that replaces this one when it is retired (object id or model id).
    #[serde(default)]
    pub successor_id: Option<String>,
    #[serde(default)]
    pub sunset_at: Option<DateTime<Utc>>,
    /// Retire the model by itself when `sunset_at` has passed.
    #[serde(default)]
    pub retire_at_sunset: bool,
    /// What the model takes and gives; left empty, they follow from its kind and capabilities.
    #[serde(default)]
    pub modalities: Modalities,
    /// Explicitly without cost; allows activation without a price.
    #[serde(default)]
    pub free: bool,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ModelView {
    #[serde(flatten)]
    pub model: Model,
    /// The provider's name.
    pub provider: String,
    /// Whether a price is in effect.
    pub priced: bool,
    /// The successor's model id, when one is set.
    pub successor: Option<String>,
    /// The modalities in effect: as set, or as the kind and capabilities imply.
    pub effective_modalities: Modalities,
}

fn resolve_model_ref(snap: &Snapshot, reference: &str) -> Option<String> {
    snap.models.get(reference).map(|m| m.id.clone()).or_else(|| snap.model_names.get(reference).cloned())
}

impl Resource for Model {
    const PLURAL: &'static str = "models";
    const LABEL: &'static str = "model";
    const SCOPE: ScopeKind = ScopeKind::Global;
    type Input = ModelInput;
    type View = ModelView;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if !is_model_name(&input.name) {
            return Err(ApiError::invalid(
                "name may contain letters, digits and / - _ . : (at most 128 characters) and must not start with @",
            ));
        }
        let provider = ctx
            .snap
            .providers
            .get(&input.provider_id)
            .ok_or_else(|| ApiError::invalid("provider_id does not name a provider"))?;
        if input.upstream_id.trim().is_empty() {
            return Err(ApiError::invalid("upstream_id must not be empty"));
        }
        let capabilities = match (input.capabilities, &input.capability_preset) {
            (Some(c), _) => c,
            (None, Some(name)) => capability_presets()
                .into_iter()
                .find(|p| p.name == name)
                .map(|p| p.capabilities)
                .ok_or_else(|| ApiError::invalid(format!("unknown capability preset \"{name}\"")))?,
            (None, None) => existing.map(|e| e.doc.capabilities.clone()).unwrap_or_else(|| match provider.doc.family {
                Family::Anthropic => preset("anthropic-adaptive"),
                _ => preset("openai-compatible"),
            }),
        };
        let successor_id = match input.successor_id.as_deref().filter(|s| !s.is_empty()) {
            None => None,
            Some(r) => {
                let id = resolve_model_ref(ctx.snap, r)
                    .ok_or_else(|| ApiError::invalid("successor_id does not name a model"))?;
                // A successor chain that leads back here would never resolve.
                let mut seen = vec![ctx.id.to_owned()];
                let mut at = Some(id.clone());
                while let Some(cur) = at {
                    if seen.contains(&cur) {
                        return Err(ApiError::invalid("the successor chain leads back to this model"));
                    }
                    seen.push(cur.clone());
                    at = ctx.snap.models.get(&cur).and_then(|m| m.doc.successor_id.clone());
                }
                Some(id)
            }
        };
        Ok(Model {
            name: input.name,
            provider_id: input.provider_id,
            upstream_id: input.upstream_id.trim().to_owned(),
            display_name: input.display_name,
            kind: input.kind,
            context_window: input.context_window,
            max_output_tokens: input.max_output_tokens,
            capabilities,
            // The life-cycle fields are only changed by transitions.
            state: existing.map_or(ModelState::Draft, |e| e.doc.state),
            previous_state: existing.and_then(|e| e.doc.previous_state),
            deprecated_at: existing.and_then(|e| e.doc.deprecated_at),
            successor_id,
            sunset_at: input.sunset_at,
            retire_at_sunset: input.retire_at_sunset,
            modalities: input.modalities,
            free: input.free,
            notes: input.notes,
        })
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        // The handlers below fill in the names from the snapshot; this is the bare form.
        ModelView {
            model: obj.doc.clone(),
            provider: String::new(),
            priced: obj.doc.free,
            successor: None,
            effective_modalities: obj.doc.modalities(),
        }
    }

    async fn before_delete(_tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        if obj.doc.state != ModelState::Draft {
            return Err(ApiError::conflict(
                "only draft models can be deleted; retire the model instead, so that its usage history keeps resolving",
            ));
        }
        Ok(())
    }
}

fn preset(name: &str) -> Capabilities {
    capability_presets().into_iter().find(|p| p.name == name).map(|p| p.capabilities).unwrap_or_default()
}

fn model_view(obj: &Obj<Model>, snap: &Snapshot) -> Item<ModelView> {
    Item {
        meta: obj.meta(),
        data: ModelView {
            model: obj.doc.clone(),
            provider: snap.providers.get(&obj.doc.provider_id).map(|p| p.doc.name.clone()).unwrap_or_default(),
            priced: obj.doc.free || snap.price_at(&obj.id, Utc::now()).is_some(),
            successor: obj.doc.successor_id.as_ref().and_then(|s| snap.models.get(s)).map(|m| m.doc.name.clone()),
            effective_modalities: obj.doc.modalities(),
        },
    }
}

async fn list_models(State(st): State<AdminState>, _p: Principal) -> ApiResult<Json<Page<Item<ModelView>>>> {
    let snap = st.config.snapshot();
    let rows = st.store.list(Model::KIND, ParentFilter::Any, None, 5000).await?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(model_view(&row.into_obj()?, &snap));
    }
    items.sort_by(|a, b| a.data.model.name.cmp(&b.data.model.name));
    Ok(Json(Page { items, next_cursor: None }))
}

async fn get_model(
    State(st): State<AdminState>,
    _p: Principal,
    Params(params): Params,
) -> ApiResult<Json<Item<ModelView>>> {
    let snap = st.config.snapshot();
    let id = params.get("id").ok_or_else(|| ApiError::not_found("model"))?;
    let obj = st.store.get_obj::<Model>(id).await.map_err(|_| ApiError::not_found("model"))?;
    Ok(Json(model_view(&obj, &snap)))
}

async fn presets(_p: Principal) -> Json<Vec<CapabilityPreset>> {
    Json(capability_presets())
}

/// What a life-cycle change would affect.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct Impact {
    /// Organisations the model is granted to.
    pub organisations: Vec<String>,
    /// Workspaces that can use the model.
    pub workspaces: Vec<String>,
    /// Templates whose current version uses the model, as `workspace/@template`.
    pub templates: Vec<String>,
    /// API keys whose allow-list names the model.
    pub keys: Vec<String>,
    /// Models that name this one as their successor.
    pub successor_of: Vec<String>,
    /// Calls in the last 30 days. Absent when the analytics service cannot be asked.
    pub requests_last_30d: Option<u64>,
    /// The same calls by organisation, workspace (`organisation/workspace`), template
    /// (`@name`) and key (concept section 6.3). Empty when the analytics service cannot be asked.
    pub requests_by: RequestsBy,
}

/// Calls of the last 30 days, counted per item.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct RequestsBy {
    pub organisations: BTreeMap<String, u64>,
    pub workspaces: BTreeMap<String, u64>,
    pub templates: BTreeMap<String, u64>,
    pub keys: BTreeMap<String, u64>,
}

async fn impact_of(st: &AdminState, snap: &Snapshot, model: &Obj<Model>) -> Impact {
    let mut out = Impact::default();
    for org in snap.organisations.values() {
        // The gateway's own organisation may use every model; it depends on one only through
        // a template, which is listed below.
        if org.grants.contains_key(&model.id) && org.id != crate::model::SYSTEM_ORG_ID {
            out.organisations.push(org.doc.name.clone());
        }
    }
    for ws in snap.workspaces.values() {
        let label = || {
            let org = snap.organisations.get(&ws.organisation_id).map(|o| o.doc.name.as_str()).unwrap_or_default();
            format!("{org}/{}", ws.doc.name)
        };
        if snap.grant_terms(ws, &model.id).is_some() && ws.organisation_id != crate::model::SYSTEM_ORG_ID {
            out.workspaces.push(label());
        }
        for t in ws.templates.values() {
            if t.pool.iter().any(|m| m.model.id == model.id) {
                out.templates.push(format!("{}/@{}", label(), t.name));
            }
        }
    }
    for key in snap.keys.values() {
        if key.valid_until.is_none() && key.doc.allowed_models.iter().any(|m| *m == model.doc.name || *m == model.id) {
            out.keys.push(key.doc.name.clone());
        }
    }
    for m in snap.models.values() {
        if m.doc.successor_id.as_deref() == Some(&model.id) {
            out.successor_of.push(m.doc.name.clone());
        }
    }
    out.organisations.sort();
    out.workspaces.sort();
    out.templates.sort();
    out.keys.sort();
    out.successor_of.sort();

    // The request counts are helpful, not essential: without analytics the rest still answers.
    if let Ok(base) = st.analytics_url() {
        let from = (Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        let ask = |by: &'static str| {
            let req = st
                .http
                .get(format!("{base}/reports/breakdown"))
                .query(&[("by", by), ("from", from.as_str()), ("model", model.doc.name.as_str()), ("limit", "500")])
                .header(pacod_gw_events::ingest::INTERNAL_TOKEN_HEADER, &st.boot.internal_token)
                .timeout(Duration::from_secs(5));
            async move {
                let r = req.send().await.ok().filter(|r| r.status().is_success())?;
                let v = r.json::<serde_json::Value>().await.ok()?;
                Some(
                    v["rows"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|r| Some((r["group"].as_str()?.to_owned(), r["requests"].as_u64()?)))
                        .collect::<Vec<(String, u64)>>(),
                )
            }
        };
        let (orgs, workspaces, templates, keys) =
            tokio::join!(ask("organisation_id"), ask("workspace_id"), ask("template"), ask("key_id"));
        if let Some(orgs) = orgs {
            out.requests_last_30d = Some(orgs.iter().map(|(_, n)| n).sum());
            for (id, n) in orgs {
                let name = snap.organisations.get(&id).map_or(id, |o| o.doc.name.clone());
                out.requests_by.organisations.insert(name, n);
            }
        }
        for (id, n) in workspaces.unwrap_or_default() {
            let name = snap.workspaces.get(&id).map_or(id, |ws| {
                let org = snap.organisations.get(&ws.organisation_id).map(|o| o.doc.name.as_str()).unwrap_or_default();
                format!("{org}/{}", ws.doc.name)
            });
            out.requests_by.workspaces.insert(name, n);
        }
        for (name, n) in templates.unwrap_or_default().into_iter().filter(|(t, _)| !t.is_empty()) {
            out.requests_by.templates.insert(format!("@{name}"), n);
        }
        for (id, n) in keys.unwrap_or_default().into_iter().filter(|(k, _)| !k.is_empty()) {
            let name = snap.keys.values().find(|k| k.id == id).map_or(id, |k| k.doc.name.clone());
            out.requests_by.keys.insert(name, n);
        }
    }
    out
}

async fn impact(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<Impact>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let id = params.get("id").ok_or_else(|| ApiError::not_found("model"))?;
    let obj = st.store.get_obj::<Model>(id).await.map_err(|_| ApiError::not_found("model"))?;
    Ok(Json(impact_of(&st, &snap, &obj).await))
}

/// The result of calling the provider to check a model.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ProbeResult {
    pub ok: bool,
    pub detail: String,
    pub latency_ms: u64,
    /// The provider's HTTP status, when it answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}

/// Retires the deprecated models whose sunset has passed and that are marked to retire by
/// themselves (concept section 6.3). Returns how many it retired.
pub async fn retire_at_sunset(st: &AdminState) -> anyhow::Result<usize> {
    let now = Utc::now();
    let due: Vec<String> = st
        .config
        .snapshot()
        .models
        .values()
        .filter(|m| {
            m.doc.state == ModelState::Deprecated && m.doc.retire_at_sunset && m.doc.sunset_at.is_some_and(|s| s <= now)
        })
        .map(|m| m.id.clone())
        .collect();
    let mut retired = 0;
    for id in due {
        let mut obj = st.store.get_obj::<Model>(&id).await?;
        if obj.doc.state != ModelState::Deprecated {
            continue;
        }
        obj.doc.state = ModelState::Retired;
        let audit = crate::store::AuditEntry {
            actor: "sunset".into(),
            actor_kind: "system".into(),
            action: "transition:deprecated->retired".into(),
            detail: serde_json::json!({ "sunset_at": obj.doc.sunset_at }),
            ..Default::default()
        };
        let mut tx = st.store.begin().await?;
        tx.update(&obj.id, Some(obj.version), Some(&obj.doc.name), &serde_json::to_value(&obj.doc)?, audit).await?;
        st.commit(tx).await.map_err(|e| anyhow::anyhow!(e.message))?;
        tracing::info!(model = %obj.doc.name, "retired at its sunset");
        retired += 1;
    }
    Ok(retired)
}

/// Why a model's provider cannot be called: missing, disabled, or without any credential.
fn credential_problem(snap: &Snapshot, model: &Model) -> Option<String> {
    let Some(provider) = snap.providers.get(&model.provider_id) else {
        return Some("the model's provider does not exist".into());
    };
    if provider.doc.status != Status::Active {
        return Some("the model's provider is disabled".into());
    }
    let c = &provider.credentials;
    let usable = provider.doc.auth_scheme() == AuthScheme::None
        || c.global.is_some()
        || !c.organisations.is_empty()
        || !c.workspaces.is_empty();
    (!usable).then(|| format!("the provider \"{}\" has no credential", provider.doc.name))
}

/// Asks the provider whether it knows the model, with the shared credential. The probe is
/// recorded like a call of the built-in `_system` workspace (concept section 5.1).
pub async fn probe_model(st: &AdminState, snap: &Snapshot, model: &Model) -> ProbeResult {
    let mut rec =
        crate::relay::CallRecorder::new(st.gw.clone(), crate::telemetry::trace::TraceContext::from_traceparent(None));
    let e = &mut rec.event;
    e.organisation_id = crate::model::SYSTEM_ORG_ID.into();
    e.workspace_id = crate::model::SYSTEM_WS_ID.into();
    e.key_id = "probe".into();
    e.mode = "managed".into();
    e.interface = "probe".into();
    e.model_requested = model.name.clone();
    e.model_served = model.name.clone();
    e.provider = snap.providers.get(&model.provider_id).map(|p| p.doc.name.clone()).unwrap_or_default();
    rec.usage_policy = crate::model::UsagePolicy::Ignore;
    let result = probe_once(st, snap, model).await;
    rec.event.attempts = u8::from(result.http_status.is_some());
    match result.http_status {
        _ if result.ok => rec.outcome("ok", result.http_status.unwrap_or(200), "", ""),
        status => rec.outcome("error", status.unwrap_or(0), "probe_failed", &result.detail),
    }
    rec.finish();
    result
}

async fn probe_once(st: &AdminState, snap: &Snapshot, model: &Model) -> ProbeResult {
    let started = Instant::now();
    let done = |ok: bool, detail: String| ProbeResult {
        ok,
        detail,
        latency_ms: started.elapsed().as_millis() as u64,
        http_status: None,
    };
    let Some(provider) = snap.providers.get(&model.provider_id) else {
        return done(false, "the model's provider does not exist".into());
    };
    if provider.doc.status != Status::Active {
        return done(false, "the provider is disabled".into());
    }
    let secret = provider.credentials.global.clone();
    if secret.is_none() && provider.doc.auth_scheme() != AuthScheme::None {
        return done(false, "the provider has no shared credential".into());
    }
    // Each family has a cheap, free way to ask about one model.
    let base = provider.doc.base_url.trim_end_matches('/');
    let url = match provider.doc.family {
        Family::Openai => format!("{base}/models/{}", model.upstream_id),
        Family::Anthropic => format!("{base}/v1/models/{}", model.upstream_id),
        Family::Gemini => format!("{base}/models/{}", model.upstream_id),
        Family::Ollama => format!("{}/api/tags", provider.proxy_origin),
    };
    let mut req = match http::Request::builder().method("GET").uri(&url).body(axum::body::Body::empty()) {
        Ok(r) => r,
        Err(e) => return done(false, format!("the probe URL is not valid: {e}")),
    };
    if let Some(s) = &secret {
        let (name, value) = match provider.doc.auth_scheme() {
            AuthScheme::Bearer => ("authorization", format!("Bearer {}", s.value.as_str())),
            AuthScheme::XApiKey => ("x-api-key", s.value.to_string()),
            AuthScheme::XGoogApiKey => ("x-goog-api-key", s.value.to_string()),
            AuthScheme::None => ("x-unused", String::new()),
        };
        if let Ok(mut v) = http::HeaderValue::from_str(&value) {
            v.set_sensitive(true);
            req.headers_mut().insert(http::HeaderName::from_static(name), v);
        }
    }
    if provider.doc.family == Family::Anthropic {
        req.headers_mut().insert("anthropic-version", http::HeaderValue::from_static("2023-06-01"));
    }
    match st.upstream.send(req, Duration::from_secs(15)).await {
        Ok(sent) => {
            let status = sent.response.status();
            let result = if status.is_success() {
                done(true, format!("the provider answered {status}"))
            } else if status.as_u16() == 404 {
                done(false, format!("the provider does not know \"{}\" (404)", model.upstream_id))
            } else if matches!(status.as_u16(), 401 | 403) {
                done(false, format!("the provider refused the credential ({status})"))
            } else {
                done(false, format!("the provider answered {status}"))
            };
            ProbeResult { http_status: Some(status.as_u16()), ..result }
        }
        Err(e) => done(false, e.to_string()),
    }
}

async fn probe(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<ProbeResult>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let id = params.get("id").ok_or_else(|| ApiError::not_found("model"))?;
    let obj = st.store.get_obj::<Model>(id).await.map_err(|_| ApiError::not_found("model"))?;
    Ok(Json(probe_model(&st, &snap, &obj.doc).await))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransitionInput {
    /// The state to move to.
    pub to: ModelState,
    /// Deprecation and retirement: the model that replaces this one.
    #[serde(default)]
    pub successor_id: Option<String>,
    /// Deprecation: when the model is to be retired.
    #[serde(default)]
    pub sunset_at: Option<DateTime<Utc>>,
    /// Deprecation: retire the model by itself once `sunset_at` has passed.
    #[serde(default)]
    pub retire_at_sunset: Option<bool>,
    /// Activation: do not call the provider first. For installations without outbound access.
    #[serde(default)]
    pub skip_probe: bool,
}

#[derive(Debug, Default, Deserialize)]
struct DryRun {
    dry_run: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TransitionResult {
    /// False for a dry run.
    pub applied: bool,
    pub from: ModelState,
    pub to: ModelState,
    pub impact: Impact,
    /// The model after the transition; for a dry run, as it is now.
    pub model: Item<ModelView>,
}

async fn transition(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Query(q): Query<DryRun>,
    Json(input): Json<TransitionInput>,
) -> ApiResult<Json<TransitionResult>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let id = params.get("id").ok_or_else(|| ApiError::not_found("model"))?;
    let mut obj = st.store.get_obj::<Model>(id).await.map_err(|_| ApiError::not_found("model"))?;
    let from = obj.doc.state;
    if !from.can_become(input.to, obj.doc.previous_state) {
        return Err(ApiError::conflict(format!(
            "a model in state {} cannot move to {}",
            from.as_str(),
            input.to.as_str()
        )));
    }

    // Gates for activation (concept section 6.3).
    if input.to == ModelState::Active && from == ModelState::Draft {
        if !obj.doc.free && snap.price_at(&obj.id, Utc::now()).is_none() {
            return Err(ApiError::invalid("the model has no price; set one, or mark the model as free"));
        }
        // A usable credential is a gate of its own, also when the probe is skipped.
        if let Some(why) = credential_problem(&snap, &obj.doc) {
            return Err(ApiError::invalid(why));
        }
        if !input.skip_probe && q.dry_run != Some(true) {
            let result = probe_model(&st, &snap, &obj.doc).await;
            if !result.ok {
                return Err(ApiError::invalid(format!("the probe failed: {}", result.detail))
                    .with_details(serde_json::to_value(&result).unwrap_or_default()));
            }
        }
    }

    let successor_id = match input.successor_id.as_deref().filter(|s| !s.is_empty()) {
        Some(r) => {
            let sid =
                resolve_model_ref(&snap, r).ok_or_else(|| ApiError::invalid("successor_id does not name a model"))?;
            if sid == obj.id {
                return Err(ApiError::invalid("a model cannot be its own successor"));
            }
            Some(sid)
        }
        None => obj.doc.successor_id.clone(),
    };

    let impact = impact_of(&st, &snap, &obj).await;
    if q.dry_run == Some(true) {
        return Ok(Json(TransitionResult {
            applied: false,
            from,
            to: input.to,
            impact,
            model: model_view(&obj, &snap),
        }));
    }

    match input.to {
        ModelState::Disabled => obj.doc.previous_state = Some(from),
        _ if from == ModelState::Disabled => obj.doc.previous_state = None,
        _ => {}
    }
    match input.to {
        ModelState::Deprecated => {
            obj.doc.deprecated_at = Some(obj.doc.deprecated_at.unwrap_or_else(Utc::now));
            obj.doc.successor_id = successor_id;
            if input.sunset_at.is_some() {
                obj.doc.sunset_at = input.sunset_at;
            }
            if let Some(retire) = input.retire_at_sunset {
                obj.doc.retire_at_sunset = retire;
            }
        }
        ModelState::Retired => obj.doc.successor_id = successor_id,
        ModelState::Active if from != ModelState::Disabled => {
            // Taking back a deprecation or retirement.
            obj.doc.deprecated_at = None;
            obj.doc.sunset_at = None;
        }
        _ => {}
    }
    obj.doc.state = input.to;

    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(None, None);
    audit.action = format!("transition:{}->{}", from.as_str(), input.to.as_str());
    audit.detail = serde_json::json!({ "skip_probe": input.skip_probe });
    let row = tx
        .update(
            &obj.id,
            Some(obj.version),
            Some(&obj.doc.name),
            &serde_json::to_value(&obj.doc).map_err(ApiError::internal)?,
            audit,
        )
        .await?;
    st.commit(tx).await?;
    let snap = st.config.snapshot();
    Ok(Json(TransitionResult { applied: true, from, to: input.to, impact, model: model_view(&row.into_obj()?, &snap) }))
}

// --- grants ----------------------------------------------------------------------------------

#[derive(Debug, Serialize, JsonSchema)]
pub struct GrantView {
    /// The model's object id.
    pub model_id: String,
    pub follow_successor: bool,
    pub max_output_tokens: Option<u32>,
}

fn check_grant_model(snap: &Snapshot, model_ref: &str, existing: bool) -> ApiResult<String> {
    let id = resolve_model_ref(snap, model_ref).ok_or_else(|| ApiError::invalid("model_id does not name a model"))?;
    let m = &snap.models[&id];
    // A new grant needs an active model; an existing grant may stay on a deprecated one.
    if !existing && !m.doc.state.assignable() {
        return Err(ApiError::invalid(format!(
            "the model \"{}\" is {} and cannot be granted",
            m.doc.name,
            m.doc.state.as_str()
        )));
    }
    Ok(id)
}

impl Resource for OrgGrant {
    const PLURAL: &'static str = "grants";
    const LABEL: &'static str = "grant";
    const SCOPE: ScopeKind = ScopeKind::Organisation;
    type Input = OrgGrant;
    type View = GrantView;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.model_id.clone())
    }

    fn build(mut input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        let same = existing.is_some_and(|e| e.doc.model_id == input.model_id);
        input.model_id = check_grant_model(ctx.snap, &input.model_id, same)?;
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        GrantView {
            model_id: obj.doc.model_id.clone(),
            follow_successor: obj.doc.follow_successor,
            max_output_tokens: obj.doc.max_output_tokens,
        }
    }

    /// Everyone who works in the organisation, a workspace member too, may see which models
    /// it has: they choose from them.
    fn can_read(p: &Principal, scope: &ScopeIds, snap: &Snapshot) -> bool {
        p.sees_org(scope.org(), snap)
    }

    /// Which models an organisation may use is the platform's decision.
    fn can_write(p: &Principal, _scope: &ScopeIds, _snap: &Snapshot) -> bool {
        p.is_platform_admin()
    }
}

impl Resource for WorkspaceGrant {
    const PLURAL: &'static str = "grants";
    const LABEL: &'static str = "grant";
    const SCOPE: ScopeKind = ScopeKind::Workspace;
    type Input = WorkspaceGrant;
    type View = GrantView;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.model_id.clone())
    }

    fn build(mut input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        let same = existing.is_some_and(|e| e.doc.model_id == input.model_id);
        input.model_id = check_grant_model(ctx.snap, &input.model_id, same)?;
        let org = ctx.snap.organisations.get(ctx.scope.org()).ok_or_else(|| ApiError::not_found("organisation"))?;
        if !org.grants.contains_key(&input.model_id) {
            return Err(ApiError::invalid("the organisation has not been granted this model"));
        }
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        GrantView {
            model_id: obj.doc.model_id.clone(),
            follow_successor: obj.doc.follow_successor,
            max_output_tokens: obj.doc.max_output_tokens,
        }
    }

    /// Which of the organisation's models a workspace may use is the organisation's decision.
    fn can_write(p: &Principal, scope: &ScopeIds, _snap: &Snapshot) -> bool {
        p.can_admin_org(scope.org())
    }
}

// --- prices ----------------------------------------------------------------------------------

/// A new manual price for a model.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PriceInput {
    /// Three-letter currency code.
    pub currency: String,
    #[serde(flatten)]
    pub rates: Rates,
    #[serde(default)]
    pub variants: BTreeMap<String, Rates>,
    /// When the price starts to apply. Default now. Prices never apply backwards.
    #[serde(default)]
    pub effective_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub evidence_url: String,
    #[serde(default)]
    pub evidence_text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PriceView {
    #[serde(flatten)]
    pub price: Price,
    /// The model's id.
    pub model: String,
    /// True for the version currently in effect.
    pub in_effect: bool,
}

fn validate_rates(r: &Rates) -> ApiResult<()> {
    for (name, v) in [
        ("input", r.input),
        ("output", r.output),
        ("cache_read", r.cache_read),
        ("cache_write_5m", r.cache_write_5m),
        ("cache_write_1h", r.cache_write_1h),
        ("reasoning", r.reasoning),
        ("per_character", r.per_character),
        ("per_request", r.per_request),
        ("per_server_tool_call", r.per_server_tool_call),
    ] {
        if v.is_some_and(|x| !x.is_finite() || x < 0.0) {
            return Err(ApiError::invalid(format!("{name} must be a non-negative number")));
        }
    }
    Ok(())
}

/// Stores a new price version for a model and supersedes the versions it replaces.
pub async fn add_price_version(
    tx: &mut Tx,
    model: &Obj<Model>,
    input: PriceInput,
    source: PriceSource,
    status: PriceStatus,
    audit: crate::store::AuditEntry,
) -> ApiResult<Obj<Price>> {
    if input.currency.len() != 3 || !input.currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(ApiError::invalid("currency must be a three-letter code such as USD"));
    }
    validate_rates(&input.rates)?;
    for r in input.variants.values() {
        validate_rates(r)?;
    }
    let now = Utc::now();
    let effective_from = input.effective_from.unwrap_or(now);
    if effective_from < now - chrono::Duration::minutes(5) {
        return Err(ApiError::invalid("effective_from lies in the past; prices never apply backwards"));
    }
    let existing: Vec<Obj<Price>> = {
        let mut v = Vec::new();
        for row in tx.list(Price::KIND, Some(&model.id)).await? {
            v.push(row.into_obj()?);
        }
        v
    };
    let version = existing.iter().map(|p| p.doc.version).max().unwrap_or(0) + 1;
    let price = Price {
        model_id: model.id.clone(),
        version,
        currency: input.currency,
        rates: input.rates,
        variants: input.variants,
        effective_from,
        source,
        status,
        evidence_url: input.evidence_url,
        evidence_text: input.evidence_text.chars().take(2000).collect(),
        confirmed_at: None,
    };
    let id = new_id(Price::PREFIX);
    let row = tx
        .create(
            Price::KIND,
            &id,
            Some(&model.id),
            Some(&version.to_string()),
            &serde_json::to_value(&price).map_err(ApiError::internal)?,
            audit.clone(),
        )
        .await?;
    // A price in effect now replaces the active versions before it. They stay as history.
    if status == PriceStatus::Active && effective_from <= now + chrono::Duration::seconds(1) {
        for mut old in existing.into_iter().filter(|p| p.doc.status == PriceStatus::Active) {
            old.doc.status = PriceStatus::Superseded;
            let mut a = audit.clone();
            a.action = "supersede".into();
            tx.update(
                &old.id,
                None,
                Some(&old.doc.version.to_string()),
                &serde_json::to_value(&old.doc).map_err(ApiError::internal)?,
                a,
            )
            .await?;
        }
    }
    Ok(row.into_obj()?)
}

async fn put_price(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<PriceInput>,
) -> ApiResult<(StatusCode, Json<PriceView>)> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("model"))?;
    let model = st.store.get_obj::<Model>(id).await.map_err(|_| ApiError::not_found("model"))?;
    let mut tx = st.store.begin().await?;
    let price =
        add_price_version(&mut tx, &model, input, PriceSource::Manual, PriceStatus::Active, p.audit(None, None))
            .await?;
    st.commit(tx).await?;
    Ok((StatusCode::CREATED, Json(PriceView { price: price.doc, model: model.doc.name, in_effect: true })))
}

async fn model_prices(
    State(st): State<AdminState>,
    _p: Principal,
    Params(params): Params,
) -> ApiResult<Json<Vec<PriceView>>> {
    let snap = st.config.snapshot();
    let id = params.get("id").ok_or_else(|| ApiError::not_found("model"))?;
    let model = st.store.get_obj::<Model>(id).await.map_err(|_| ApiError::not_found("model"))?;
    let current = snap.price_at(&model.id, Utc::now()).map(|p| p.version);
    let mut out = Vec::new();
    for row in st.store.list(Price::KIND, ParentFilter::Is(&model.id), None, 1000).await? {
        let price: Obj<Price> = row.into_obj()?;
        out.push(PriceView {
            in_effect: Some(price.doc.version) == current,
            price: price.doc,
            model: model.doc.name.clone(),
        });
    }
    out.sort_by_key(|v| std::cmp::Reverse(v.price.version));
    Ok(Json(out))
}

/// The price in effect for every model that has one.
async fn current_prices(State(st): State<AdminState>, _p: Principal) -> Json<Vec<PriceView>> {
    let snap = st.config.snapshot();
    let now = Utc::now();
    let mut out: Vec<PriceView> = snap
        .models
        .values()
        .filter_map(|m| {
            snap.price_at(&m.id, now).map(|p| PriceView {
                price: (**p).clone(),
                model: m.doc.name.clone(),
                in_effect: true,
            })
        })
        .collect();
    out.sort_by(|a, b| a.model.cmp(&b.model));
    Json(out)
}

pub fn routes(mut router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    // Models: the generic create, replace and delete, with listing and reading done here so
    // that the provider name and price state are filled in.
    router = register::<Model>(router, api, Ops { list: false, read: false, ..Ops::ALL });
    let mv = api.schema::<Item<ModelView>>();
    let impact_schema = api.schema::<Impact>();
    let probe_schema = api.schema::<ProbeResult>();
    let transition_in = api.schema::<TransitionInput>();
    let transition_out = api.schema::<TransitionResult>();
    let preset_schema = api.schema::<CapabilityPreset>();
    let price_in = api.schema::<PriceInput>();
    let price_view = api.schema::<PriceView>();
    let array = |item: serde_json::Value| serde_json::json!({ "type": "array", "items": item });

    api.add(api.op("get", "/models", "listModels", "models", "List the model repository").ok(page_schema(mv.clone())));
    api.add(api.op("get", "/models/{id}", "getModel", "models", "Get a model").ok(mv));
    api.add(
        api.op(
            "post",
            "/models/{id}/transition",
            "transitionModel",
            "models",
            "Move a model to another life-cycle state",
        )
        .query("dry_run", "Set to true to see the impact without changing anything.")
        .body(transition_in)
        .ok(transition_out),
    );
    api.add(api.op("get", "/models/{id}/impact", "modelImpact", "models", "What uses this model").ok(impact_schema));
    api.add(
        api.op("post", "/models/{id}/probe", "probeModel", "models", "Ask the provider whether it knows the model")
            .ok(probe_schema),
    );
    api.add(
        api.op("get", "/capability-presets", "capabilityPresets", "models", "Starting points for capability profiles")
            .ok(array(preset_schema)),
    );
    api.add(
        api.op("get", "/prices", "currentPrices", "prices", "The price in effect for each model")
            .ok(array(price_view.clone())),
    );
    api.add(
        api.op("get", "/models/{id}/prices", "modelPrices", "prices", "All price versions of a model, newest first")
            .ok(array(price_view.clone())),
    );
    api.add(
        api.op("put", "/models/{id}/price", "setModelPrice", "prices", "Set a manual price: creates a new version")
            .body(price_in)
            .created(price_view),
    );

    router = register::<OrgGrant>(router, api, Ops::ALL);
    router = register::<WorkspaceGrant>(router, api, Ops::ALL);

    router
        .route("/models", get(list_models))
        .route("/models/{id}", get(get_model))
        .route("/models/{id}/transition", post(transition))
        .route("/models/{id}/impact", get(impact))
        .route("/models/{id}/probe", post(probe))
        .route("/models/{id}/prices", get(model_prices))
        .route("/models/{id}/price", put(put_price))
        .route("/capability-presets", get(presets))
        .route("/prices", get(current_prices))
}
