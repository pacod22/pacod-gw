//! The price tracker (concept section 13.3): its configuration, its runs, the proposals it
//! leaves for approval, price imports, and the internal API the analytics service uses to run
//! it.
//!
//! The job itself runs in the analytics service, which fetches price lists and pricing pages.
//! The core hands out the lease, decides what happens with each price that was found, and is
//! the only component that writes prices.

use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use pacod_gw_events::ingest::INTERNAL_TOKEN_HEADER;
use pacod_gw_events::jobs::{JOB_PRICES, JobResult, LeaseReply, LeaseRequest, PriceCandidate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::repo::{PriceInput, PriceView, add_price_version};
use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{
    Doc, Model, ModelState, Obj, PRICE_JOB_ID, Price, PriceJob, PriceSource, PriceStatus, Rates, SYSTEM_WS_ID,
    SourceKind, new_id,
};
use crate::snapshot::Snapshot;
use crate::store::jobs::JobRun;
use crate::store::{AuditEntry, ParentFilter, Tx};

// --- configuration -----------------------------------------------------------------------------

/// The price tracker's configuration together with what the core knows about its state.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PriceJobView {
    #[serde(flatten)]
    pub config: PriceJob,
    /// When the job last started.
    pub last_started_at: Option<chrono::DateTime<Utc>>,
    /// The next scheduled start. Absent while the job is switched off.
    pub next_run_at: Option<chrono::DateTime<Utc>>,
    /// A run was asked for and has not started yet.
    pub run_requested: bool,
    /// True while a run is in progress.
    pub running: bool,
}

async fn view(st: &AdminState, config: PriceJob) -> ApiResult<PriceJobView> {
    let state = st.store.job_state(JOB_PRICES).await?;
    let next_run_at = config
        .enabled
        .then(|| {
            let tz = config.time_zone.parse::<chrono_tz::Tz>().ok()?;
            let from = state.last_started_at.unwrap_or_else(Utc::now).with_timezone(&tz);
            config.parsed_schedule().ok()?.after(&from).next().map(|t| t.with_timezone(&Utc))
        })
        .flatten();
    Ok(PriceJobView {
        config,
        last_started_at: state.last_started_at,
        next_run_at,
        run_requested: state.run_requested,
        running: state.current_run.is_some(),
    })
}

async fn get_job(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<PriceJobView>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    Ok(Json(view(&st, st.config.snapshot().price_job.clone()).await?))
}

fn check_job(job: &PriceJob, snap: &Snapshot, egress: &crate::upstream::EgressPolicy) -> ApiResult<()> {
    job.parsed_schedule().map_err(ApiError::invalid)?;
    if job.time_zone.parse::<chrono_tz::Tz>().is_err() {
        return Err(ApiError::invalid(format!("\"{}\" is not an IANA time zone", job.time_zone)));
    }
    if !(0.0..=10.0).contains(&job.auto_apply_bound) {
        return Err(ApiError::invalid("auto_apply_bound is a share: 0.3 means 30 %; it must be between 0 and 10"));
    }
    if !(1..=3650).contains(&job.stale_days) {
        return Err(ApiError::invalid("stale_days must be between 1 and 3650"));
    }
    for s in &job.sources {
        if !snap.providers.contains_key(&s.provider_id) {
            return Err(ApiError::invalid(format!("the source {} names a provider that does not exist", s.url)));
        }
        // The analytics service fetches this URL from inside the network. The same rule as
        // for a provider's base URL applies, and is applied again when the fetch connects.
        egress.check_url(&s.url).map_err(|e| ApiError::invalid(format!("the source \"{}\": {e}", s.url)))?;
        if s.kind == SourceKind::Page && job.extraction.is_none() {
            return Err(ApiError::invalid("a source of kind page needs an extraction template"));
        }
    }
    if let Some(x) = &job.extraction {
        if x.workspace_id != SYSTEM_WS_ID {
            return Err(ApiError::invalid(
                "the extraction template lives in the built-in _system workspace, so that its cost is the gateway's own",
            ));
        }
        let ws = snap
            .workspaces
            .get(&x.workspace_id)
            .ok_or_else(|| ApiError::invalid("the _system workspace is missing; restart the management instance"))?;
        if !ws.templates.contains_key(x.template.trim_start_matches('@')) {
            return Err(ApiError::invalid(format!("the _system workspace has no template \"@{}\"", x.template)));
        }
    }
    Ok(())
}

async fn put_job(
    State(st): State<AdminState>,
    p: Principal,
    Json(mut input): Json<PriceJob>,
) -> ApiResult<Json<PriceJobView>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    if let Some(x) = &mut input.extraction {
        x.template = x.template.trim_start_matches('@').to_owned();
        // Unset means the built-in workspace, the only one allowed.
        if x.workspace_id.is_empty() {
            x.workspace_id = SYSTEM_WS_ID.into();
        }
    }
    check_job(&input, &st.config.snapshot(), &st.egress)?;
    let value = serde_json::to_value(&input).map_err(ApiError::internal)?;
    let mut tx = st.store.begin().await?;
    if tx.get(PRICE_JOB_ID).await?.is_some() {
        tx.update(PRICE_JOB_ID, None, None, &value, p.audit(None, None)).await?;
    } else {
        tx.create(PriceJob::KIND, PRICE_JOB_ID, None, None, &value, p.audit(None, None)).await?;
    }
    st.commit(tx).await?;
    Ok(Json(view(&st, input).await?))
}

/// Asks for a run now. It starts when the analytics service next asks for work, within a minute.
async fn run_now(State(st): State<AdminState>, p: Principal) -> ApiResult<StatusCode> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    st.analytics_url()
        .map_err(|_| ApiError::conflict("the price tracker runs in the analytics service, which is not configured"))?;
    st.store.job_request_run(JOB_PRICES).await?;
    let mut entry = p.audit(None, None);
    entry.action = "run_price_job".into();
    entry.kind = PriceJob::KIND.into();
    entry.object_id = PRICE_JOB_ID.into();
    st.store.audit(&entry).await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, Default, Deserialize)]
struct RunsQuery {
    limit: Option<i64>,
}

async fn runs(State(st): State<AdminState>, p: Principal, Query(q): Query<RunsQuery>) -> ApiResult<Json<Vec<JobRun>>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    Ok(Json(st.store.job_runs(JOB_PRICES, q.limit.unwrap_or(30).clamp(1, 200)).await?))
}

// --- deciding what happens with a price that was found -----------------------------------------

/// What the core did with the price found for one model.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ModelResult {
    /// The model's id.
    pub model: String,
    /// `applied`, `proposed`, `unchanged`, `skipped_manual`, `no_price` or `invalid`.
    pub outcome: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The price version that was created, for `applied` and `proposed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// The largest relative change among the rates, where there was a price before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<f64>,
}

fn result(model: &str, outcome: &str, detail: impl Into<String>) -> ModelResult {
    ModelResult {
        model: model.to_owned(),
        outcome: outcome.to_owned(),
        detail: detail.into(),
        version: None,
        change: None,
    }
}

/// The token rates a candidate names, as `(name, value)`.
fn candidate_rates(c: &PriceCandidate) -> [(&'static str, Option<f64>); 5] {
    [
        ("input", c.input),
        ("output", c.output),
        ("cache_read", c.cache_read),
        ("cache_write_5m", c.cache_write_5m),
        ("cache_write_1h", c.cache_write_1h),
    ]
}

/// Units, positive values and plausible ratios (concept section 13.3, step 3).
pub fn validate(c: &PriceCandidate) -> Result<(), String> {
    if c.currency.len() != 3 || !c.currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(format!("\"{}\" is not a currency code", c.currency));
    }
    let (Some(input), Some(output)) = (c.input, c.output) else {
        return Err("a price needs an input and an output rate".into());
    };
    for (name, v) in candidate_rates(c) {
        // Per million tokens: a rate above 10 000 is a rate per token mistaken for one per million, or worse.
        if v.is_some_and(|x| !x.is_finite() || x <= 0.0 || x > 10_000.0) {
            return Err(format!("the {name} rate is not a plausible price per million tokens"));
        }
    }
    if output < input * 0.5 {
        return Err("the output rate is far below the input rate".into());
    }
    if c.cache_read.is_some_and(|r| r > input) {
        return Err("reading from the cache costs more than fresh input".into());
    }
    Ok(())
}

/// The largest relative change among the rates the candidate names. `None` when a rate
/// appears that the current price does not have: that cannot be measured as a change.
pub fn relative_change(current: &Rates, c: &PriceCandidate) -> Option<f64> {
    let old = [current.input, current.output, current.cache_read, current.cache_write_5m, current.cache_write_1h];
    let mut largest = 0.0f64;
    for ((_, new), old) in candidate_rates(c).into_iter().zip(old) {
        match (old, new) {
            (_, None) => {}
            (Some(o), Some(n)) if o > 0.0 => largest = largest.max((n - o).abs() / o),
            (Some(o), Some(n)) if o == n => {}
            _ => return None,
        }
    }
    Some(largest)
}

/// The current rates with the candidate's laid over them. Rates the candidate does not name
/// (speech, requests, variants) are kept.
fn merged(current: Option<&Price>, c: &PriceCandidate) -> PriceInput {
    let mut rates = current.map(|p| p.rates.clone()).unwrap_or_default();
    rates.input = c.input.or(rates.input);
    rates.output = c.output.or(rates.output);
    rates.cache_read = c.cache_read.or(rates.cache_read);
    rates.cache_write_5m = c.cache_write_5m.or(rates.cache_write_5m);
    rates.cache_write_1h = c.cache_write_1h.or(rates.cache_write_1h);
    PriceInput {
        currency: c.currency.clone(),
        rates,
        variants: current.map(|p| p.variants.clone()).unwrap_or_default(),
        effective_from: None,
        evidence_url: c.evidence_url.clone(),
        evidence_text: c.evidence_text.clone(),
    }
}

fn source_of(c: &PriceCandidate) -> PriceSource {
    match c.source.as_str() {
        "extracted" => PriceSource::Extracted,
        "import" => PriceSource::Import,
        _ => PriceSource::Feed,
    }
}

/// Decides and stores what happens with one candidate. `authoritative` is true for an import
/// by an administrator, which is applied whatever the size of the change.
async fn decide(
    tx: &mut Tx,
    snap: &Snapshot,
    model: &Obj<Model>,
    c: &PriceCandidate,
    bound: f64,
    authoritative: bool,
    audit: &AuditEntry,
) -> ApiResult<ModelResult> {
    let name = &model.doc.name;
    if let Err(why) = validate(c) {
        return Ok(result(name, "invalid", why));
    }
    let current = snap.price_at(&model.id, Utc::now()).map(|p| p.as_ref());
    // A price an administrator entered is never overwritten by the job.
    if !authoritative && current.is_some_and(|p| p.source == PriceSource::Manual) {
        return Ok(result(name, "skipped_manual", "the current price was entered by hand"));
    }
    let change = current.and_then(|p| if p.currency == c.currency { relative_change(&p.rates, c) } else { None });
    if change == Some(0.0) {
        confirm(tx, model, current, audit).await?;
        return Ok(result(name, "unchanged", ""));
    }
    let apply = authoritative || change.is_some_and(|x| x <= bound);
    let status = if apply { PriceStatus::Active } else { PriceStatus::Proposed };
    if !apply {
        // One open proposal per model is enough: the same figures are not proposed twice.
        for row in tx.list(Price::KIND, Some(&model.id)).await? {
            let existing: Obj<Price> = row.into_obj()?;
            if existing.doc.status == PriceStatus::Proposed
                && existing.doc.currency == c.currency
                && relative_change(&existing.doc.rates, c) == Some(0.0)
            {
                let mut r = result(name, "proposed", "already waiting for approval");
                (r.version, r.change) = (Some(existing.doc.version), change);
                return Ok(r);
            }
        }
    }
    let mut entry = audit.clone();
    entry.action = if apply { "price_applied" } else { "price_proposed" }.into();
    let price = add_price_version(tx, model, merged(current, c), source_of(c), status, entry).await?;
    let detail = match (apply, change, current.is_some()) {
        (true, _, _) => String::new(),
        (false, Some(x), _) => format!("a change of {:.0} % is above the bound of {:.0} %", x * 100.0, bound * 100.0),
        (false, None, true) => "the price changes in a way that cannot be compared with the current one".into(),
        (false, None, false) => "the model's first price".into(),
    };
    let mut r = result(name, if apply { "applied" } else { "proposed" }, detail);
    (r.version, r.change) = (Some(price.doc.version), change);
    Ok(r)
}

/// How long a confirmation stands before the next one is written. Running the job several
/// times a day does not write the same fact several times.
const CONFIRM_EVERY_HOURS: i64 = 12;

/// Notes on the price in effect that its source still shows the same figures. The stale
/// threshold counts from this moment.
async fn confirm(tx: &mut Tx, model: &Obj<Model>, current: Option<&Price>, audit: &AuditEntry) -> ApiResult<()> {
    let now = Utc::now();
    let Some(current) = current.filter(|p| (now - p.checked_at()).num_hours() >= CONFIRM_EVERY_HOURS) else {
        return Ok(());
    };
    for row in tx.list(Price::KIND, Some(&model.id)).await? {
        let mut price: Obj<Price> = row.into_obj()?;
        if price.doc.version != current.version {
            continue;
        }
        price.doc.confirmed_at = Some(now);
        let mut entry = audit.clone();
        entry.action = "price_confirmed".into();
        let doc = serde_json::to_value(&price.doc).map_err(ApiError::internal)?;
        tx.update(&price.id, None, Some(&price.doc.version.to_string()), &doc, entry).await?;
        break;
    }
    Ok(())
}

/// The models the tracker looks after: active and deprecated ones, and drafts without a price.
fn provisioned(snap: &Snapshot) -> Vec<&std::sync::Arc<crate::snapshot::ModelRt>> {
    let now = Utc::now();
    let mut models: Vec<_> = snap
        .models
        .values()
        .filter(|m| match m.doc.state {
            ModelState::Active | ModelState::Deprecated => !m.doc.free,
            ModelState::Draft => !m.doc.free && snap.price_at(&m.id, now).is_none(),
            ModelState::Retired | ModelState::Disabled => false,
        })
        .collect();
    models.sort_by(|a, b| a.doc.name.cmp(&b.doc.name));
    models
}

/// Applies the decisions for a set of candidates and returns the result per provisioned model.
async fn settle(
    st: &AdminState,
    candidates: &[PriceCandidate],
    authoritative: bool,
    audit: AuditEntry,
) -> ApiResult<Vec<ModelResult>> {
    let snap = st.config.snapshot();
    let bound = snap.price_job.auto_apply_bound;
    let mut out = Vec::new();
    let mut tx = st.store.begin().await?;
    let mut seen = std::collections::HashSet::new();
    for c in candidates {
        // A candidate names the model by object id, by its id in the repository, or (in an
        // import) by the provider's id when that is unambiguous.
        let by_upstream = || {
            let mut same = snap.models.values().filter(|m| m.doc.upstream_id == c.model_id);
            same.next().filter(|_| same.next().is_none()).map(|m| m.id.clone())
        };
        let id = snap
            .models
            .get(&c.model_id)
            .map(|m| m.id.clone())
            .or_else(|| snap.model_names.get(&c.model_id).cloned())
            .or_else(by_upstream);
        let Some(id) = id.filter(|id| seen.insert(id.clone())) else {
            out.push(result(&c.model_id, "invalid", "no such model, or named twice"));
            continue;
        };
        let model: Obj<Model> = match tx.get(&id).await? {
            Some(row) => row.into_obj()?,
            None => continue,
        };
        out.push(decide(&mut tx, &snap, &model, c, bound, authoritative, &audit).await?);
    }
    st.commit(tx).await?;
    if !authoritative {
        // A scheduled run reports every model it looks after, also those nothing was found for.
        for m in provisioned(&snap) {
            if !seen.contains(&m.id) {
                out.push(result(&m.doc.name, "no_price", "no source had a price for this model"));
            }
        }
    }
    out.sort_by(|a, b| a.model.cmp(&b.model));
    Ok(out)
}

fn counts(results: &[ModelResult]) -> Value {
    let mut c = serde_json::Map::new();
    for r in results {
        let n = c.get(&r.outcome).and_then(Value::as_u64).unwrap_or(0);
        c.insert(r.outcome.clone(), json!(n + 1));
    }
    Value::Object(c)
}

// --- proposals and imports -----------------------------------------------------------------------

async fn proposals(
    State(st): State<AdminState>,
    p: Principal,
) -> ApiResult<Json<Vec<crate::mgmt::crud::Item<PriceView>>>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let mut out = Vec::new();
    for row in st.store.list(Price::KIND, ParentFilter::Any, None, 5000).await? {
        let price: Obj<Price> = row.into_obj()?;
        if price.doc.status == PriceStatus::Proposed {
            let model = snap.models.get(&price.doc.model_id).map(|m| m.doc.name.clone()).unwrap_or_default();
            out.push(crate::mgmt::crud::Item {
                meta: price.meta(),
                data: PriceView { price: price.doc, model, in_effect: false },
            });
        }
    }
    out.sort_by(|a, b| a.data.model.cmp(&b.data.model));
    Ok(Json(out))
}

async fn resolve_proposal(st: &AdminState, p: &Principal, id: &str, approve: bool) -> ApiResult<Json<PriceView>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let mut price = st.store.get_obj::<Price>(id).await.map_err(|_| ApiError::not_found("price proposal"))?;
    if price.doc.status != PriceStatus::Proposed {
        return Err(ApiError::conflict("this price is not waiting for approval"));
    }
    let model = st.store.get_obj::<Model>(&price.doc.model_id).await.map_err(|_| ApiError::not_found("model"))?;
    let mut tx = st.store.begin().await?;
    let mut entry = p.audit(None, None);
    entry.action = if approve { "price_approved" } else { "price_rejected" }.into();
    if approve {
        // The price applies from now on; the versions in effect before it become history.
        for row in tx.list(Price::KIND, Some(&model.id)).await? {
            let mut old: Obj<Price> = row.into_obj()?;
            if old.doc.status == PriceStatus::Active && old.id != price.id {
                old.doc.status = PriceStatus::Superseded;
                let mut a = entry.clone();
                a.action = "supersede".into();
                let v = serde_json::to_value(&old.doc).map_err(ApiError::internal)?;
                tx.update(&old.id, None, Some(&old.doc.version.to_string()), &v, a).await?;
            }
        }
        price.doc.status = PriceStatus::Active;
        price.doc.effective_from = Utc::now();
    } else {
        price.doc.status = PriceStatus::Rejected;
    }
    let v = serde_json::to_value(&price.doc).map_err(ApiError::internal)?;
    tx.update(&price.id, Some(price.version), Some(&price.doc.version.to_string()), &v, entry).await?;
    st.commit(tx).await?;
    Ok(Json(PriceView { price: price.doc, model: model.doc.name, in_effect: approve }))
}

async fn approve(State(st): State<AdminState>, p: Principal, Path(id): Path<String>) -> ApiResult<Json<PriceView>> {
    resolve_proposal(&st, &p, &id, true).await
}

async fn reject(State(st): State<AdminState>, p: Principal, Path(id): Path<String>) -> ApiResult<Json<PriceView>> {
    resolve_proposal(&st, &p, &id, false).await
}

/// One model's price in an import file.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImportedPrice {
    /// The model: its id in the repository, or the provider's own id when only one model has it.
    pub model: String,
    pub input: Option<f64>,
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write_5m: Option<f64>,
    #[serde(default)]
    pub cache_write_1h: Option<f64>,
}

/// A price file, for installations without outbound access. Rates are per million tokens.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PriceImport {
    /// Three-letter currency code of every price in the file.
    pub currency: String,
    pub prices: Vec<ImportedPrice>,
    /// Where the figures come from, for the record.
    #[serde(default)]
    pub evidence_url: String,
}

/// The result of an import or of a tracker run.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PriceRunResult {
    pub run_id: String,
    pub models: Vec<ModelResult>,
    /// How many models ended with each outcome.
    pub counts: Value,
}

/// Imports a price file. An import is an administrator's own statement of the prices: it is
/// applied whatever the size of the change, and it also replaces prices entered by hand.
async fn import(
    State(st): State<AdminState>,
    p: Principal,
    Json(input): Json<PriceImport>,
) -> ApiResult<Json<PriceRunResult>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    if input.prices.is_empty() || input.prices.len() > 5000 {
        return Err(ApiError::invalid("an import holds between 1 and 5000 prices"));
    }
    let candidates: Vec<PriceCandidate> = input
        .prices
        .iter()
        .map(|i| PriceCandidate {
            model_id: i.model.clone(),
            currency: input.currency.clone(),
            input: i.input,
            output: i.output,
            cache_read: i.cache_read,
            cache_write_5m: i.cache_write_5m,
            cache_write_1h: i.cache_write_1h,
            source: "import".into(),
            evidence_url: input.evidence_url.clone(),
            evidence_text: String::new(),
        })
        .collect();
    let run_id = new_id("run");
    st.store.job_start_run(JOB_PRICES, &run_id, "import").await?;
    let models = settle(&st, &candidates, true, p.audit(None, None)).await?;
    let counts = counts(&models);
    st.store.job_finish_run(&run_id, true, "", &json!({ "models": models, "counts": counts })).await?;
    Ok(Json(PriceRunResult { run_id, models, counts }))
}

// --- the internal API of the analytics service ---------------------------------------------------

/// The analytics service authenticates with the secret it shares with the core.
fn internal(st: &AdminState, headers: &HeaderMap) -> ApiResult<()> {
    let given = headers.get(INTERNAL_TOKEN_HEADER).map(HeaderValue::as_bytes).unwrap_or_default();
    let expected = st.boot.internal_token.as_bytes();
    if expected.is_empty() || !crate::crypto::ct_eq(given, expected) {
        return Err(ApiError::unauthorized("this endpoint is for the analytics service"));
    }
    Ok(())
}

/// Gives out the lease of a job and, when the job is due, the work for a run.
async fn lease(
    State(st): State<AdminState>,
    headers: HeaderMap,
    Json(req): Json<LeaseRequest>,
) -> ApiResult<Json<LeaseReply>> {
    internal(&st, &headers)?;
    let nothing = |granted| Json(LeaseReply { granted, run_id: None, due: false, work: Value::Null });
    if req.job != JOB_PRICES {
        return Ok(nothing(false));
    }
    if !st.store.job_lease(&req.job, &req.holder, req.ttl_seconds.clamp(30, 3600)).await? {
        return Ok(nothing(false));
    }
    let snap = st.config.snapshot();
    let state = st.store.job_state(&req.job).await?;
    // A run that is still open belongs to this holder now: it is given out again, so that a
    // restarted analytics instance finishes it.
    let run_id = match state.current_run {
        Some(open) => open,
        None => {
            let trigger = if state.run_requested { "manual" } else { "schedule" };
            if !state.run_requested && !snap.price_job.due(state.last_started_at, Utc::now()) {
                return Ok(nothing(true));
            }
            let id = new_id("run");
            st.store.job_start_run(&req.job, &id, trigger).await?;
            st.announce("job", json!({ "job": req.job, "run_id": id, "status": "started", "trigger": trigger }));
            id
        }
    };
    let now = Utc::now();
    let models: Vec<Value> = provisioned(&snap)
        .into_iter()
        .map(|m| {
            let provider = snap.providers.get(&m.doc.provider_id);
            json!({
                "model_id": m.id,
                "name": m.doc.name,
                "upstream_id": m.doc.upstream_id,
                "provider_id": m.doc.provider_id,
                "provider": provider.map(|p| p.doc.name.as_str()),
                "current": snap.price_at(&m.id, now).map(|p| json!({ "currency": p.currency, "source": p.source, "rates": p.rates })),
            })
        })
        .collect();
    Ok(Json(LeaseReply {
        granted: true,
        run_id: Some(run_id),
        due: true,
        work: json!({ "config": snap.price_job, "models": models }),
    }))
}

/// Takes the result of a run: the prices that were found. The core decides per model.
async fn job_result(
    State(st): State<AdminState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Json(res): Json<JobResult>,
) -> ApiResult<Json<PriceRunResult>> {
    internal(&st, &headers)?;
    let run = st.store.job_run(&run_id).await?.ok_or_else(|| ApiError::not_found("run"))?;
    if run.finished_at.is_some() {
        return Err(ApiError::conflict("this run is already finished"));
    }
    let candidates: Vec<PriceCandidate> =
        serde_json::from_value(res.result.get("candidates").cloned().unwrap_or_else(|| json!([])))
            .map_err(|e| ApiError::invalid(format!("candidates: {e}")))?;
    let mut entry = AuditEntry::system("price-tracker");
    entry.kind = Price::KIND.into();
    entry.detail = json!({ "run_id": run_id });
    let models = settle(&st, &candidates, false, entry).await?;
    let counts = counts(&models);
    let stored = json!({ "models": models, "counts": counts, "sources": res.result.get("sources") });
    st.store.job_finish_run(&run_id, res.ok, &res.message, &stored).await?;
    st.announce(
        "job",
        json!({ "job": JOB_PRICES, "run_id": run_id, "status": if res.ok { "finished" } else { "failed" }, "counts": counts }),
    );
    Ok(Json(PriceRunResult { run_id, models, counts }))
}

/// What the analytics service sends to have rates extracted from a pricing page.
#[derive(Debug, Deserialize)]
struct ExtractRequest {
    /// The page's URL, for the record.
    url: String,
    /// The page as text.
    text: String,
    /// The provider's ids of the models to find.
    models: Vec<String>,
}

/// Has an LLM read a pricing page. The call goes through the gateway's own data plane, as the
/// workspace and template the price job names, so that its cost is accounted like any other.
async fn extract(
    State(st): State<AdminState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Json(req): Json<ExtractRequest>,
) -> ApiResult<Json<Value>> {
    internal(&st, &headers)?;
    let snapshot = st.config.snapshot();
    let x = snapshot
        .price_job
        .extraction
        .clone()
        .ok_or_else(|| ApiError::conflict("no extraction template is configured"))?;
    let workspace = snapshot
        .workspaces
        .get(&x.workspace_id)
        .filter(|w| w.enabled)
        .cloned()
        .ok_or_else(|| ApiError::conflict("the extraction workspace is missing or disabled"))?;
    let caller = super::super::playground::caller_for(snapshot, workspace, "price-tracker");

    let schema = json!({
        "type": "object",
        "properties": { "prices": { "type": "array", "items": {
            "type": "object",
            "properties": {
                "model": { "type": "string" }, "currency": { "type": "string" },
                "input": { "type": "number" }, "output": { "type": "number" },
                "cache_read": { "type": ["number", "null"] }, "cache_write_5m": { "type": ["number", "null"] }, "cache_write_1h": { "type": ["number", "null"] },
                "evidence": { "type": "string" }
            },
            "required": ["model", "currency", "input", "output", "evidence"]
        } } },
        "required": ["prices"]
    });
    let prompt = format!(
        "The text below is a provider's pricing page. Find the prices of these models: {}.\n\
         Give every rate in the page's currency per one million tokens. Use null for a rate the page does not state. \
         In \"evidence\" quote the words of the page the figures come from. Leave out a model the page does not price; do not guess.\n\n\
         --- page ---\n{}",
        req.models.join(", "),
        req.text.chars().take(120_000).collect::<String>()
    );
    let request = json!({
        "model": format!("@{}", x.template),
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": prompt }] }],
        "response_format": { "type": "json_schema", "name": "prices", "schema": schema },
        "gateway": { "trace_id": run_id, "tags": { "purpose": "price_fetch" } }
    });
    let mut call_headers = HeaderMap::new();
    call_headers.insert("content-type", HeaderValue::from_static("application/json"));
    let response = crate::dataplane::managed::serve(
        st.gw.clone(),
        caller,
        Instant::now(),
        crate::telemetry::trace::TraceContext::from_traceparent(None),
        call_headers,
        axum::body::Body::from(request.to_string()),
        crate::dataplane::managed::Interface::Native,
    )
    .await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 8 << 20).await.map_err(ApiError::internal)?;
    let answer: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !status.is_success() {
        let said = answer["error"]["message"].as_str().unwrap_or("the extraction call failed");
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "extraction_failed", said.to_owned()));
    }
    // The model's answer is the text of its first text block.
    let text = answer["content"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["type"] == "text")
        .and_then(|b| b["text"].as_str())
        .unwrap_or_default();
    let found: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({ "prices": [] }));
    Ok(Json(json!({ "url": req.url, "prices": found["prices"] })))
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let job = api.schema::<PriceJob>();
    let job_view = api.schema::<PriceJobView>();
    let run = api.schema::<JobRun>();
    let run_result = api.schema::<PriceRunResult>();
    let price_view = api.schema::<PriceView>();
    let item = api.schema::<crate::mgmt::crud::Item<PriceView>>();
    let import_in = api.schema::<PriceImport>();
    let array = |item: Value| json!({ "type": "array", "items": item });
    api.add(
        api.op("get", "/price-job", "getPriceJob", "prices", "The price tracker's configuration and state")
            .ok(job_view.clone()),
    );
    api.add(api.op("put", "/price-job", "setPriceJob", "prices", "Configure the price tracker").body(job).ok(job_view));
    api.add(
        api.op("post", "/price-job/run", "runPriceJob", "prices", "Run the price tracker now")
            .response("202", "Accepted", None),
    );
    api.add(
        api.op("get", "/price-job/runs", "listPriceJobRuns", "prices", "Runs of the price tracker, newest first")
            .query_int("limit", "How many runs, default 30.")
            .ok(array(run)),
    );
    api.add(
        api.op("get", "/price-proposals", "listPriceProposals", "prices", "Prices waiting for approval")
            .ok(array(item)),
    );
    api.add(
        api.op(
            "post",
            "/price-proposals/{id}/approve",
            "approvePriceProposal",
            "prices",
            "Put a proposed price into effect",
        )
        .ok(price_view.clone()),
    );
    api.add(
        api.op("post", "/price-proposals/{id}/reject", "rejectPriceProposal", "prices", "Reject a proposed price")
            .ok(price_view),
    );
    api.add(
        api.op("post", "/prices/import", "importPrices", "prices", "Import a price file")
            .body(import_in)
            .ok(run_result),
    );
    router
        .route("/price-job", get(get_job).put(put_job))
        .route("/price-job/run", post(run_now))
        .route("/price-job/runs", get(runs))
        .route("/price-proposals", get(proposals))
        .route("/price-proposals/{id}/approve", post(approve))
        .route("/price-proposals/{id}/reject", post(reject))
        .route("/prices/import", post(import))
        // Not part of the OpenAPI document: these are for the analytics service only.
        .route("/internal/jobs/lease", post(lease))
        .route("/internal/jobs/{run_id}/result", post(job_result))
        .route("/internal/jobs/{run_id}/extract", post(extract))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(input: f64, output: f64) -> PriceCandidate {
        PriceCandidate {
            model_id: "m".into(),
            currency: "USD".into(),
            input: Some(input),
            output: Some(output),
            cache_read: None,
            cache_write_5m: None,
            cache_write_1h: None,
            source: "feed".into(),
            evidence_url: String::new(),
            evidence_text: String::new(),
        }
    }

    #[test]
    fn candidates_are_checked_for_units_and_ratios() {
        assert!(validate(&candidate(2.0, 10.0)).is_ok());
        assert!(validate(&PriceCandidate { cache_read: Some(0.2), ..candidate(2.0, 10.0) }).is_ok());
        for (bad, why) in [
            (candidate(0.0, 10.0), "input"),
            (candidate(-1.0, 10.0), "input"),
            (candidate(2.0, f64::NAN), "output"),
            (candidate(20_000.0, 60_000.0), "plausible"),
            (candidate(10.0, 2.0), "far below"),
            (PriceCandidate { cache_read: Some(3.0), ..candidate(2.0, 10.0) }, "cache"),
            (PriceCandidate { currency: "usd".into(), ..candidate(2.0, 10.0) }, "currency"),
            (PriceCandidate { output: None, ..candidate(2.0, 10.0) }, "needs an input and an output"),
        ] {
            let e = validate(&bad).unwrap_err();
            assert!(e.contains(why), "{e}");
        }
    }

    #[test]
    fn a_change_is_the_largest_relative_difference() {
        let current = Rates {
            input: Some(2.0),
            output: Some(10.0),
            cache_read: Some(0.2),
            per_character: Some(0.1),
            ..Default::default()
        };
        assert_eq!(relative_change(&current, &candidate(2.0, 10.0)), Some(0.0));
        assert_eq!(relative_change(&current, &candidate(2.5, 10.0)), Some(0.25));
        assert_eq!(relative_change(&current, &candidate(1.0, 11.0)), Some(0.5));
        // A rate the current price does not have cannot be compared: that needs a person.
        assert_eq!(
            relative_change(&current, &PriceCandidate { cache_write_5m: Some(2.5), ..candidate(2.0, 10.0) }),
            None
        );
        // Rates the candidate does not name are kept when it is applied.
        let old = Price {
            model_id: "m".into(),
            version: 1,
            currency: "USD".into(),
            rates: current,
            variants: Default::default(),
            effective_from: Utc::now(),
            source: PriceSource::Feed,
            status: PriceStatus::Active,
            evidence_url: String::new(),
            evidence_text: String::new(),
            confirmed_at: None,
        };
        let next = merged(Some(&old), &candidate(3.0, 15.0));
        assert_eq!(
            (next.rates.input, next.rates.cache_read, next.rates.per_character),
            (Some(3.0), Some(0.2), Some(0.1))
        );
    }
}
