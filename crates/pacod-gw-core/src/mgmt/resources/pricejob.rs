//! The price job (concept section 13.3): its configuration, its runs, the proposals of earlier
//! releases, price imports, and the internal job API the analytics service uses.
//!
//! Model data comes from the sync (`mgmt/catalog.rs`). The core decides what happens with each
//! price a source states, and is the only component that writes prices.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use pacod_gw_events::ingest::INTERNAL_TOKEN_HEADER;
use pacod_gw_events::jobs::{
    CandidateRates, JOB_PRICES, JOB_RETENTION, JOB_ROLLUPS, JobResult, LeaseReply, LeaseRequest, PriceCandidate,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::repo::{PriceInput, PriceView, add_price_version};
use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::{Doc, Model, Obj, PRICE_JOB_ID, Price, PriceJob, PriceSource, PriceStatus, Rates, new_id};
use crate::snapshot::Snapshot;
use crate::store::jobs::JobRun;
use crate::store::{AuditEntry, ParentFilter, Tx};

// --- configuration -----------------------------------------------------------------------------

/// The price job's configuration together with what the core knows about the sync.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PriceJobView {
    #[serde(flatten)]
    pub config: PriceJob,
    /// The sync: pacod-aggregator, the models file, what the environment sets, and when it runs.
    pub sync: crate::mgmt::catalog::SyncView,
    /// What keeps the sync from doing its work, in sentences.
    pub problems: Vec<String>,
    /// Send it back with an update. Absent until the job is first saved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
}

async fn view(st: &AdminState, config: PriceJob) -> ApiResult<PriceJobView> {
    Ok(PriceJobView {
        sync: crate::mgmt::catalog::view(st, &config).await?,
        problems: crate::mgmt::catalog::problems(st, &config),
        version: st.store.get(PRICE_JOB_ID).await?.map(|r| r.version),
        config,
    })
}

async fn get_job(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<PriceJobView>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    Ok(Json(view(&st, st.config.snapshot().price_job.clone()).await?))
}

fn check_job(job: &PriceJob) -> ApiResult<()> {
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
    Ok(())
}

async fn put_job(
    State(st): State<AdminState>,
    p: Principal,
    Json(mut body): Json<Value>,
) -> ApiResult<Json<PriceJobView>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let version = crate::mgmt::crud::take_version(&mut body);
    // What the view adds is accepted back and ignored, as with every other object.
    let mut input: PriceJob = serde_json::from_value(body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    // Saved here, the values are the administrator's: the environment no longer sets them.
    input.origin = crate::model::Origin::Console;
    check_sync(&st, &mut input)?;
    check_job(&input)?;
    // What an earlier release configured is not edited any more, and stays for such an instance.
    if let Some(row) = st.store.get(PRICE_JOB_ID).await? {
        let stored: PriceJob = serde_json::from_value(row.doc).map_err(ApiError::internal)?;
        (input.sources, input.extraction) = (stored.sources, stored.extraction);
    }
    let mut tx = st.store.begin().await?;
    let value = serde_json::to_value(&input).map_err(ApiError::internal)?;
    crate::mgmt::crud::save_singleton(&mut tx, PriceJob::KIND, PRICE_JOB_ID, version, &value, p.audit(None, None))
        .await?;
    st.commit(tx).await?;
    Ok(Json(view(&st, input).await?))
}

/// The sync's settings: addresses an administrator names go through the egress policy, as a
/// provider's do, and the vendors are the catalogue's ids.
fn check_sync(st: &AdminState, job: &mut PriceJob) -> ApiResult<()> {
    for (field, url) in [("openrouter_url", &mut job.openrouter_url), ("litellm_url", &mut job.litellm_url)] {
        *url = url.trim().trim_end_matches('/').to_owned();
        st.egress.check_url(url).map_err(|e| ApiError::invalid(format!("{field}: {e}")))?;
    }
    for v in &mut job.openrouter_price_vendors {
        *v = v.trim().to_ascii_lowercase();
        if v.is_empty() || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(ApiError::invalid(format!("openrouter_price_vendors: \"{v}\" is not a vendor id")));
        }
    }
    if job.source == crate::model::CatalogSource::File && st.boot.catalog.models_file.is_none() {
        return Err(ApiError::invalid("source: the models file is not set (PGW_MODELS_FILE)"));
    }
    Ok(())
}

/// Asks for a sync of the model data now. It starts at the next round of upkeep, within a
/// minute.
async fn run_now(State(st): State<AdminState>, p: Principal) -> ApiResult<StatusCode> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    st.store.job_request_run(crate::mgmt::catalog::JOB).await?;
    audit_run(&st, &p, "run_price_job").await?;
    Ok(StatusCode::ACCEPTED)
}

async fn audit_run(st: &AdminState, p: &Principal, action: &str) -> ApiResult<()> {
    let mut entry = p.audit(None, None);
    entry.action = action.into();
    entry.kind = PriceJob::KIND.into();
    entry.object_id = PRICE_JOB_ID.into();
    Ok(st.store.audit(&entry).await?)
}

/// Prices one model at once: a model that has just become active, or a draft accepted from
/// model discovery, which cannot be activated until it has a price (concept section 13.3). What
/// the sources said at their last reading prices it now; where they named no price for it, a
/// sync covering it is asked for. A failure is logged and does not undo what asked for the price.
pub async fn request_run_for_model(st: &AdminState, _snap: &Snapshot, model_id: &str, model: &Model) {
    if model.free {
        return;
    }
    match crate::mgmt::catalog::price_from_seen(st, model_id).await {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e.message, model = %model.name, "pricing a model from the model data failed"),
    }
    // Read after the change that asked: a snapshot from before it may not hold the model yet.
    if st.boot.scheduled_catalog
        && st.config.snapshot().price_job.enabled
        && let Err(e) = st.store.job_request_run_for(crate::mgmt::catalog::JOB, &[model_id.to_owned()]).await
    {
        tracing::warn!(error = %e, model = %model.name, "asking the model-data sync to price a model failed");
    }
}

#[derive(Debug, Default, Deserialize)]
struct RunsQuery {
    limit: Option<i64>,
    job: Option<String>,
}

/// Runs of the model data (`sync`: the sync, the models file and uploads), or of the job of
/// earlier releases and the imports (`prices`); all of them together by default.
async fn runs(State(st): State<AdminState>, p: Principal, Query(q): Query<RunsQuery>) -> ApiResult<Json<Vec<JobRun>>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    use crate::mgmt::catalog::{JOB as SYNC, JOB_FILE as FILE, JOB_UPLOAD as UPLOAD};
    let jobs: &[&str] = match q.job.as_deref() {
        Some("prices") => &[JOB_PRICES],
        Some("sync") => &[SYNC, FILE, UPLOAD],
        None | Some("all") => &[JOB_PRICES, SYNC, FILE, UPLOAD],
        Some(other) => return Err(ApiError::invalid(format!("job \"{other}\": prices, sync or all"))),
    };
    Ok(Json(st.store.job_runs_of(jobs, q.limit.unwrap_or(30).clamp(1, 200)).await?))
}

// --- deciding what happens with a price that was found -----------------------------------------

/// What the core did with the price found for one model.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ModelResult {
    /// The model's id.
    pub model: String,
    /// `applied`, `unchanged`, `pinned`, `invalid`, `no_price` (no page prices it), `free`, or
    /// `not_priced` (a draft that has a price). Runs of earlier releases also have `proposed`
    /// and `skipped_manual`.
    pub outcome: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The price version that was created, for `applied` and `proposed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// The largest relative change among the rates, where there was a price before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<f64>,
    /// The page the price comes from.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub page: String,
    /// The model's properties a page changed, as `model_fact` names them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<String>,
}

pub(crate) fn result(model: &str, outcome: &str, detail: impl Into<String>) -> ModelResult {
    ModelResult {
        model: model.to_owned(),
        outcome: outcome.to_owned(),
        detail: detail.into(),
        version: None,
        change: None,
        page: String::new(),
        properties: Vec::new(),
    }
}

/// A candidate's rates as a stored price holds them.
fn rates_of(c: &CandidateRates) -> Rates {
    Rates {
        input: c.input,
        output: c.output,
        cache_read: c.cache_read,
        cache_write_5m: c.cache_write_5m,
        cache_write_1h: c.cache_write_1h,
        reasoning: c.reasoning,
        per_character: c.per_character,
        per_request: c.per_request,
        per_server_tool_call: c.per_server_tool_call,
        input_audio: c.input_audio,
        output_audio: c.output_audio,
        input_image: c.input_image,
        output_image: c.output_image,
        per_image: c.per_image,
        per_audio_second: c.per_audio_second,
        per_search_unit: c.per_search_unit,
    }
}

/// The candidate's tiers, in threshold order.
fn tiers_of(c: &PriceCandidate) -> Vec<crate::model::PriceTier> {
    pacod_gw_events::hours::canonical_tiers(&c.tiers)
        .iter()
        .map(|t| crate::model::PriceTier {
            above_input_tokens: t.above_input_tokens,
            rates: rates_of(&t.rates),
            variants: t.variants.iter().map(|(k, r)| (k.clone(), rates_of(r))).collect(),
        })
        .collect()
}

/// The candidate's windows, in their canonical form.
fn windows_of(c: &PriceCandidate) -> Vec<crate::model::PriceWindow> {
    pacod_gw_events::hours::canonical_windows(&c.windows)
        .iter()
        .map(|w| crate::model::PriceWindow {
            variant: w.variant.clone(),
            weekdays: w.weekdays.clone(),
            hours_utc: w.hours_utc.clone(),
        })
        .collect()
}

fn candidate_tiers_of(tiers: &[crate::model::PriceTier]) -> Vec<pacod_gw_events::jobs::CandidateTier> {
    tiers
        .iter()
        .map(|t| pacod_gw_events::jobs::CandidateTier {
            above_input_tokens: t.above_input_tokens,
            rates: candidate_rates_of(&t.rates),
            variants: t.variants.iter().map(|(k, r)| (k.clone(), candidate_rates_of(r))).collect(),
        })
        .collect()
}

fn candidate_windows_of(windows: &[crate::model::PriceWindow]) -> Vec<pacod_gw_events::jobs::CandidateWindow> {
    windows
        .iter()
        .map(|w| pacod_gw_events::jobs::CandidateWindow {
            variant: w.variant.clone(),
            weekdays: w.weekdays.clone(),
            hours_utc: w.hours_utc.clone(),
        })
        .collect()
}

fn candidate_rates_of(r: &Rates) -> CandidateRates {
    CandidateRates {
        input: r.input,
        output: r.output,
        cache_read: r.cache_read,
        cache_write_5m: r.cache_write_5m,
        cache_write_1h: r.cache_write_1h,
        reasoning: r.reasoning,
        per_character: r.per_character,
        per_request: r.per_request,
        per_server_tool_call: r.per_server_tool_call,
        input_audio: r.input_audio,
        output_audio: r.output_audio,
        input_image: r.input_image,
        output_image: r.output_image,
        per_image: r.per_image,
        per_audio_second: r.per_audio_second,
        per_search_unit: r.per_search_unit,
    }
}

/// The highest plausible value of a rate. Token rates are per million tokens: one above
/// 10 000 is a rate per token mistaken for one per million, or worse. The others are per
/// character, request, tool call, image, second of audio or search unit.
fn plausible_bound(name: &str) -> (f64, &'static str) {
    match name {
        "per_character" => (0.01, "per character"),
        "per_request" => (10.0, "per request"),
        "per_server_tool_call" => (10.0, "per tool call"),
        "per_image" => (10.0, "per image"),
        "per_audio_second" => (0.1, "per second of audio"),
        "per_search_unit" => (1.0, "per search unit"),
        _ => (10_000.0, "per million tokens"),
    }
}

/// Positive values within their units' bounds and plausible ratios, for one set of rates.
/// `prefix` names the variant in messages, for example `batch `.
fn check_rates(r: &CandidateRates, prefix: &str) -> Result<(), String> {
    for (name, v) in r.named() {
        let (bound, unit) = plausible_bound(name);
        if v.is_some_and(|x| !x.is_finite() || x <= 0.0 || x > bound) {
            return Err(format!("the {prefix}{name} rate is not a plausible price {unit}"));
        }
    }
    if let (Some(input), Some(output)) = (r.input, r.output)
        && output < input * 0.5
    {
        return Err(format!("the {prefix}output rate is far below the {prefix}input rate"));
    }
    if let (Some(input), Some(read)) = (r.input, r.cache_read)
        && read > input
    {
        return Err(format!("reading from the cache costs more than fresh input ({prefix}rates)"));
    }
    Ok(())
}

/// Units, positive values and plausible ratios (concept section 13.3, step 3), for the
/// standard rates and every variant's.
pub fn validate(c: &PriceCandidate) -> Result<(), String> {
    if c.currency.len() != 3 || !c.currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(format!("\"{}\" is not a currency code", c.currency));
    }
    if c.rates.is_empty() {
        return Err("a price needs at least one standard rate".into());
    }
    check_rates(&c.rates, "")?;
    for (name, r) in &c.variants {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit()) {
            return Err(format!("\"{name}\" is not a variant name"));
        }
        check_rates(r, &format!("{name} "))?;
    }
    for t in &c.tiers {
        check_rates(&t.rates, &format!("above-{} ", t.above_input_tokens))?;
        for (name, r) in &t.variants {
            check_rates(r, &format!("{name} above-{} ", t.above_input_tokens))?;
        }
    }
    let variants: BTreeMap<String, Rates> = c.variants.iter().map(|(k, r)| (k.clone(), rates_of(r))).collect();
    // Checked as sent: the canonical form would leave out what is wrong instead of refusing it.
    let tiers: Vec<crate::model::PriceTier> = c
        .tiers
        .iter()
        .map(|t| crate::model::PriceTier {
            above_input_tokens: t.above_input_tokens,
            rates: rates_of(&t.rates),
            variants: t.variants.iter().map(|(k, r)| (k.clone(), rates_of(r))).collect(),
        })
        .collect();
    let windows: Vec<crate::model::PriceWindow> = c
        .windows
        .iter()
        .map(|w| crate::model::PriceWindow {
            variant: w.variant.clone(),
            weekdays: w.weekdays.clone(),
            hours_utc: w.hours_utc.clone(),
        })
        .collect();
    super::repo::validate_tiers_and_windows(&variants, &tiers, &windows)?;
    Ok(())
}

/// The largest relative change among the rates the candidate names, its variants' included.
/// `None` when a rate appears that the current price does not have: that cannot be measured
/// as a change.
pub fn relative_change(current: &Price, c: &PriceCandidate) -> Option<f64> {
    // New or changed tiers or windows are a change of structure, not of size. They are
    // compared in one form, so that the same hours or tiers written otherwise are no change.
    use pacod_gw_events::hours::{canonical_tiers, canonical_windows};
    if (!c.tiers.is_empty() && canonical_tiers(&c.tiers) != canonical_tiers(&candidate_tiers_of(&current.tiers)))
        || (!c.windows.is_empty()
            && canonical_windows(&c.windows) != canonical_windows(&candidate_windows_of(&current.windows)))
    {
        return None;
    }
    let none = Rates::default();
    let sets = std::iter::once((&current.rates, &c.rates))
        .chain(c.variants.iter().map(|(name, r)| (current.variants.get(name).unwrap_or(&none), r)));
    let mut largest = 0.0f64;
    for (old, new) in sets {
        for ((_, old), (_, new)) in old.named().into_iter().zip(new.named()) {
            match (old, new) {
                (_, None) => {}
                (Some(o), Some(n)) if o > 0.0 => largest = largest.max((n - o).abs() / o),
                (Some(o), Some(n)) if o == n => {}
                _ => return None,
            }
        }
    }
    Some(largest)
}

/// The current rates with the candidate's laid over them, variant by variant. Rates and
/// variants the candidate does not name are kept.
fn merged(current: Option<&Price>, c: &PriceCandidate) -> PriceInput {
    let mut rates = current.map(|p| p.rates.clone()).unwrap_or_default();
    rates.overlay(&rates_of(&c.rates));
    let mut variants = current.map(|p| p.variants.clone()).unwrap_or_default();
    for (name, r) in &c.variants {
        variants.entry(name.clone()).or_default().overlay(&rates_of(r));
    }
    // Tiers and windows come whole from the candidate when it names any.
    let tiers = if c.tiers.is_empty() { current.map(|p| p.tiers.clone()).unwrap_or_default() } else { tiers_of(c) };
    let windows =
        if c.windows.is_empty() { current.map(|p| p.windows.clone()).unwrap_or_default() } else { windows_of(c) };
    PriceInput {
        currency: c.currency.clone(),
        rates,
        variants,
        tiers,
        windows,
        effective_from: None,
        evidence_url: c.evidence_url.clone(),
        evidence_text: c.evidence_text.clone(),
        pinned: false,
    }
}

fn source_of(c: &PriceCandidate) -> PriceSource {
    match c.source.as_str() {
        "extracted" => PriceSource::Extracted,
        "import" => PriceSource::Import,
        _ => PriceSource::Feed,
    }
}

/// Decides and stores what happens with one candidate. Every valid price applies at once,
/// whatever the size of the change and whoever set the price before: the sources state what the
/// vendors charge. Only a price an administrator
/// pinned is left alone, and the plausibility checks of [`validate`] keep misreadings out.
pub(crate) async fn decide(
    tx: &mut Tx,
    snap: &Snapshot,
    model: &Obj<Model>,
    c: &PriceCandidate,
    audit: &AuditEntry,
) -> ApiResult<ModelResult> {
    let name = &model.doc.name;
    if let Err(why) = validate(c) {
        return Ok(result(name, "invalid", why));
    }
    let mut versions: Vec<Obj<Price>> = Vec::new();
    for row in tx.list(Price::KIND, Some(&model.id)).await? {
        versions.push(row.into_obj()?);
    }
    // A new version would supersede every version in effect, so one pin holds them all.
    if versions.iter().any(|p| p.doc.status == PriceStatus::Active && p.doc.pinned) {
        return Ok(result(name, "pinned", "an administrator pinned the price"));
    }
    let current = snap.price_at(&model.id, Utc::now()).map(|p| p.as_ref());
    let change = current.and_then(|p| if p.currency == c.currency { relative_change(p, c) } else { None });
    if change == Some(0.0) {
        let every = confirm_every_hours(snap.price_job.stale_days);
        confirm(tx, model, current, c.checked_at, every, audit).await?;
        return Ok(result(name, "unchanged", ""));
    }
    let mut entry = audit.clone();
    entry.action = "price_applied".into();
    let price = add_price_version(tx, model, merged(current, c), source_of(c), PriceStatus::Active, entry).await?;
    // A proposal left by an earlier release is settled by the price that now applies.
    for mut open in versions.into_iter().filter(|p| p.doc.status == PriceStatus::Proposed) {
        open.doc.status = PriceStatus::Superseded;
        let mut a = audit.clone();
        a.action = "supersede".into();
        let doc = serde_json::to_value(&open.doc).map_err(ApiError::internal)?;
        tx.update(&open.id, None, Some(&open.doc.version.to_string()), &doc, a).await?;
    }
    let detail = match (change, current) {
        (Some(x), _) => format!("a change of {:.0} %", x * 100.0),
        (None, Some(p)) if p.currency != c.currency => {
            format!("the currency changed from {} to {}", p.currency, c.currency)
        }
        (None, Some(_)) => "new rates, tiers or hours, which are not measured as one change".into(),
        (None, None) => "the model's first price".into(),
    };
    let mut r = result(name, "applied", detail);
    (r.version, r.change) = (Some(price.doc.version), change);
    Ok(r)
}

/// The least time a confirmation stands before the next one is written. Running the job several
/// times a day does not write the same fact several times.
const CONFIRM_EVERY_HOURS: i64 = 12;

/// How long a confirmation stands. A confirmation is a change of the configuration, and a daily
/// sync reaches every model, so they are spaced by what the stale threshold can bear: ten
/// confirmations within it.
fn confirm_every_hours(stale_days: u32) -> i64 {
    CONFIRM_EVERY_HOURS.max(i64::from(stale_days) * 24 / 10)
}

/// The time to record as a price's confirmation, if one is due: when the source says it last
/// read the figures, or now for a source that gives no date. A catalogue that has not looked for
/// months says so, and the price it repeats goes stale, as it should; its date never moves a
/// confirmation back, and never lies in the future.
fn confirmed_at(
    last_checked: DateTime<Utc>,
    stated: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    every_hours: i64,
) -> Option<DateTime<Utc>> {
    let at = stated.map_or(now, |t| t.min(now));
    ((at - last_checked).num_hours() >= every_hours).then_some(at)
}

/// Notes on the price in effect that its source still shows the same figures. The stale
/// threshold counts from that time.
async fn confirm(
    tx: &mut Tx,
    model: &Obj<Model>,
    current: Option<&Price>,
    stated: Option<DateTime<Utc>>,
    every_hours: i64,
    audit: &AuditEntry,
) -> ApiResult<()> {
    let Some((current, at)) =
        current.and_then(|p| Some((p, confirmed_at(p.checked_at(), stated, Utc::now(), every_hours)?)))
    else {
        return Ok(());
    };
    for row in tx.list(Price::KIND, Some(&model.id)).await? {
        let mut price: Obj<Price> = row.into_obj()?;
        if price.doc.version != current.version {
            continue;
        }
        price.doc.confirmed_at = Some(at);
        let mut entry = audit.clone();
        entry.action = "price_confirmed".into();
        let doc = serde_json::to_value(&price.doc).map_err(ApiError::internal)?;
        tx.update(&price.id, None, Some(&price.doc.version.to_string()), &doc, entry).await?;
        break;
    }
    Ok(())
}

/// Applies the decisions for the prices of an import and returns the result per model.
async fn settle(st: &AdminState, candidates: &[PriceCandidate], audit: AuditEntry) -> ApiResult<Vec<ModelResult>> {
    let snap = st.config.snapshot();
    let mut out = Vec::new();
    let mut tx = st.store.begin().await?;
    let mut seen = std::collections::HashSet::new();
    for c in candidates {
        // An import names the model by object id, by its id in the repository, or by the
        // provider's id when that is unambiguous.
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
        out.push(decide(&mut tx, &snap, &model, c, &audit).await?);
    }
    st.commit(tx).await?;
    out.sort_by(|a, b| a.model.cmp(&b.model));
    Ok(out)
}

pub(crate) fn counts(results: &[ModelResult]) -> Value {
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
    #[serde(flatten)]
    pub rates: Rates,
    /// Rates of variants the provider bills apart, by the service tier its usage reports.
    #[serde(default)]
    pub variants: BTreeMap<String, Rates>,
    /// Rates for long prompts.
    #[serde(default)]
    pub tiers: Vec<crate::model::PriceTier>,
    /// Hours in which a variant applies by the clock.
    #[serde(default)]
    pub windows: Vec<crate::model::PriceWindow>,
}

/// A price file, for installations without outbound access. Token rates are per million
/// tokens, the others per character, request or tool call.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct PriceImport {
    /// Three-letter currency code of every price in the file.
    pub currency: String,
    pub prices: Vec<ImportedPrice>,
    /// Where the figures come from, for the record.
    #[serde(default)]
    pub evidence_url: String,
}

/// The result of an import or of an upload.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PriceRunResult {
    pub run_id: String,
    pub models: Vec<ModelResult>,
    /// How many models ended with each outcome.
    pub counts: Value,
}

/// Imports a price file. Its prices apply as the sync's do: whatever the size of the change,
/// over prices entered by hand, and not over a pinned price.
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
            rates: candidate_rates_of(&i.rates),
            variants: i.variants.iter().map(|(k, r)| (k.clone(), candidate_rates_of(r))).collect(),
            tiers: candidate_tiers_of(&i.tiers),
            windows: candidate_windows_of(&i.windows),
            source: "import".into(),
            evidence_url: input.evidence_url.clone(),
            evidence_text: String::new(),
            checked_at: None,
        })
        .collect();
    let run_id = new_id("run");
    st.store.job_start_run(JOB_PRICES, &run_id, "import", None).await?;
    let models = settle(&st, &candidates, p.audit(None, None)).await?;
    let counts = counts(&models);
    st.store.job_finish_run(&run_id, true, "", &json!({ "models": models, "counts": counts })).await?;
    Ok(Json(PriceRunResult { run_id, models, counts }))
}

// --- the internal API of the analytics service ---------------------------------------------------

/// The analytics service authenticates with the secret it shares with the core.
pub(crate) fn internal(st: &AdminState, headers: &HeaderMap) -> ApiResult<()> {
    let given = headers.get(INTERNAL_TOKEN_HEADER).map(HeaderValue::as_bytes).unwrap_or_default();
    let expected = st.boot.internal_token.as_bytes();
    if expected.is_empty() || !crate::crypto::ct_eq(given, expected) {
        return Err(ApiError::unauthorized("this endpoint is for the gateway's analytics service"));
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
    if req.job == JOB_RETENTION {
        return lease_retention(&st, &req).await;
    }
    if req.job == JOB_ROLLUPS {
        // Every five minutes, by one analytics instance at a time; no run is recorded.
        if !st.store.job_lease(&req.job, &req.holder, req.ttl_seconds.clamp(30, 3600)).await? {
            return Ok(nothing(false));
        }
        let state = st.store.job_state(&req.job).await?;
        let due = state.last_started_at.is_none_or(|t| Utc::now() - t >= chrono::Duration::minutes(5));
        if due {
            st.store.job_touch(&req.job).await?;
        }
        return Ok(Json(LeaseReply { granted: true, run_id: None, due, work: Value::Null }));
    }
    // The price job of earlier releases (`prices`) is the sync's now: an analytics service of
    // such a release is given no work, and idles.
    Ok(nothing(false))
}

/// What the retention job deletes: the installation's retention in days, and every workspace
/// whose own retention (inherited from its organisation, if it sets none) differs from it.
/// `None` when nothing is to be deleted anywhere.
fn retention_work(snap: &Snapshot) -> Option<Value> {
    let global = snap.settings.usage_retention_days.filter(|d| *d > 0);
    let mut workspaces: Vec<Value> = snap
        .workspaces
        .values()
        .filter(|w| w.settings.usage_retention_days != global)
        .map(|w| json!({ "workspace_id": w.id, "days": w.settings.usage_retention_days }))
        .collect();
    workspaces.sort_by(|a, b| a["workspace_id"].as_str().cmp(&b["workspace_id"].as_str()));
    let any = global.is_some() || workspaces.iter().any(|w| w["days"].is_number());
    any.then(|| json!({ "global_days": global, "workspaces": workspaces }))
}

/// The retention job: once a day, when any retention is set (concept section 14.4).
async fn lease_retention(st: &AdminState, req: &LeaseRequest) -> ApiResult<Json<LeaseReply>> {
    let nothing = |granted| Json(LeaseReply { granted, run_id: None, due: false, work: Value::Null });
    if !st.store.job_lease(&req.job, &req.holder, req.ttl_seconds.clamp(30, 3600)).await? {
        return Ok(nothing(false));
    }
    let Some(work) = retention_work(&st.config.snapshot()) else { return Ok(nothing(true)) };
    let state = st.store.job_state(&req.job).await?;
    let run_id = match state.current_run {
        Some(open) => open,
        None => {
            let due = state.run_requested
                || state.last_started_at.is_none_or(|t| Utc::now() - t >= chrono::Duration::hours(24));
            if !due {
                return Ok(nothing(true));
            }
            let id = new_id("run");
            st.store.job_start_run(&req.job, &id, "schedule", None).await?;
            st.announce("job", json!({ "job": req.job, "run_id": id, "status": "started", "trigger": "schedule" }));
            id
        }
    };
    Ok(Json(LeaseReply { granted: true, run_id: Some(run_id), due: true, work }))
}

/// Takes the result of a run.
async fn job_result(
    State(st): State<AdminState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Json(res): Json<JobResult>,
) -> ApiResult<Json<Value>> {
    internal(&st, &headers)?;
    let run = st.store.job_run(&run_id).await?.ok_or_else(|| ApiError::not_found("run"))?;
    if run.finished_at.is_some() {
        return Err(ApiError::conflict("this run is already finished"));
    }
    if run.job != JOB_PRICES {
        // Retention: what was deleted is the record.
        st.store.job_finish_run(&run_id, res.ok, &res.message, &res.result).await?;
        st.announce(
            "job",
            json!({ "job": run.job, "run_id": run_id, "status": if res.ok { "finished" } else { "failed" } }),
        );
        return Ok(Json(json!({ "run_id": run_id })));
    }
    // A run of the job of earlier releases, left open by its analytics service across the
    // upgrade: closed, and what it found is not applied; model data comes from the sync now.
    st.store.job_finish_run(&run_id, false, EARLIER_JOB, &json!({ "sources": res.result.get("sources") })).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

/// What a run of the price job of earlier releases is closed with.
pub const EARLIER_JOB: &str = "the analytics service no longer runs the price tracker; model data comes from the sync";

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
        api.op("get", "/price-job", "getPriceJob", "prices", "The sync of model data: its configuration and state")
            .ok(job_view.clone()),
    );
    api.add(
        api.op("put", "/price-job", "setPriceJob", "prices", "Configure the sync of model data").body(job).ok(job_view),
    );
    api.add(
        api.op("post", "/price-job/run", "runPriceJob", "prices", "Sync the model data now")
            .response("202", "Accepted", None),
    );
    api.add(
        api.op("get", "/price-job/runs", "listPriceJobRuns", "prices", "Runs of the model data, newest first")
            .query_int("limit", "How many runs, default 30.")
            .query(
                "job",
                "sync (the sync, the models file and uploads), prices (earlier releases and imports) or all (the \
                 default).",
            )
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
            .ok(run_result.clone()),
    );
    api.add(
        api.op("post", "/price-job/upload", "uploadModelData", "prices", "Apply a file of models, parameters and prices")
            .query("format", "auto (the default), catalog (pacod.catalog/v1), openrouter, litellm or pacod.")
            .body(json!({ "type": "object", "description": "The file: a catalogue (pacod.catalog/v1), OpenRouter's or LiteLLM's list, or a pacod price feed. At most 16 MiB." }))
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
        .route(
            "/price-job/upload",
            post(crate::mgmt::catalog::upload)
                .layer(axum::extract::DefaultBodyLimit::max(crate::mgmt::catalog::MAX_UPLOAD_BYTES)),
        )
        // Not part of the OpenAPI document: these are for the analytics service.
        .route("/internal/jobs/lease", post(lease))
        .route("/internal/jobs/{run_id}/result", post(job_result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(input: f64, output: f64) -> PriceCandidate {
        PriceCandidate {
            model_id: "m".into(),
            currency: "USD".into(),
            rates: CandidateRates { input: Some(input), output: Some(output), ..Default::default() },
            variants: BTreeMap::new(),
            tiers: Vec::new(),
            windows: Vec::new(),
            source: "feed".into(),
            evidence_url: String::new(),
            evidence_text: String::new(),
            checked_at: None,
        }
    }

    fn with(mut c: PriceCandidate, f: impl FnOnce(&mut CandidateRates)) -> PriceCandidate {
        f(&mut c.rates);
        c
    }

    fn variant(mut c: PriceCandidate, name: &str, input: f64, output: f64) -> PriceCandidate {
        c.variants
            .insert(name.into(), CandidateRates { input: Some(input), output: Some(output), ..Default::default() });
        c
    }

    #[test]
    fn a_confirmation_keeps_the_sources_date_and_its_spacing() {
        use chrono::{Duration, TimeZone};
        let now = Utc.with_ymd_and_hms(2026, 11, 6, 12, 0, 0).unwrap();
        let hours_ago = |h| now - Duration::hours(h);
        // Spaced by the stale threshold: ten within it, and never closer than twelve hours.
        assert_eq!((confirm_every_hours(30), confirm_every_hours(1)), (72, 12));
        // No date from the source: read now.
        assert_eq!(confirmed_at(hours_ago(100), None, now, 72), Some(now));
        assert_eq!(confirmed_at(hours_ago(10), None, now, 72), None);
        // The source's own date stands, so that a catalogue that stopped reading goes stale.
        assert_eq!(confirmed_at(hours_ago(800), Some(hours_ago(700)), now, 72), Some(hours_ago(700)));
        // Not due from that date, however long ago the last confirmation is from now.
        assert_eq!(confirmed_at(hours_ago(800), Some(hours_ago(790)), now, 72), None);
        // Never back, and never later than now.
        assert_eq!(confirmed_at(hours_ago(100), Some(hours_ago(200)), now, 72), None);
        assert_eq!(confirmed_at(hours_ago(100), Some(now + Duration::hours(5)), now, 72), Some(now));
    }

    #[test]
    fn tiers_and_windows_are_checked_and_never_applied_unseen() {
        let mut c = variant(candidate(0.3, 1.2), "off_peak", 0.15, 0.6);
        c.windows.push(pacod_gw_events::jobs::CandidateWindow {
            variant: "off_peak".into(),
            weekdays: vec![6, 7],
            hours_utc: vec!["00:00-00:00".into()],
        });
        assert!(validate(&c).is_ok());
        let mut wrong = c.clone();
        wrong.windows[0].variant = "nightly".into();
        assert!(validate(&wrong).unwrap_err().contains("nightly"));
        let mut wrong = c.clone();
        wrong.windows[0].weekdays = vec![0];
        assert!(validate(&wrong).is_err());
        // The same rates with new hours: a change of structure, which is not one figure.
        let current = price(rates_of(&c.rates), [("off_peak".to_owned(), rates_of(&c.variants["off_peak"]))].into());
        assert_eq!(relative_change(&current, &c), None);
        let applied = merged(Some(&current), &c);
        assert_eq!(applied.windows.len(), 1);
        let mut tiered = candidate(2.0, 12.0);
        tiered.tiers.push(pacod_gw_events::jobs::CandidateTier {
            above_input_tokens: 200_000,
            rates: CandidateRates { input: Some(4.0), output: Some(18.0), ..Default::default() },
            variants: BTreeMap::new(),
        });
        assert!(validate(&tiered).is_ok());
        tiered.tiers[0].rates.input = Some(-1.0);
        assert!(validate(&tiered).is_err());
    }

    #[test]
    fn the_same_hours_written_otherwise_are_no_change() {
        let window = |weekdays: Vec<u8>, hours: &[&str]| pacod_gw_events::jobs::CandidateWindow {
            variant: "off_peak".into(),
            weekdays,
            hours_utc: hours.iter().map(|h| (*h).to_owned()).collect(),
        };
        let mut c = variant(candidate(0.3, 1.2), "off_peak", 0.15, 0.6);
        c.windows = vec![
            window(vec![1, 2, 3, 4, 5], &["00:00-01:00", "04:00-06:00", "10:00-00:00"]),
            window(vec![6, 7], &["00:00-00:00"]),
        ];
        let mut current =
            price(rates_of(&c.rates), [("off_peak".to_owned(), rates_of(&c.variants["off_peak"]))].into());
        current.windows = windows_of(&c);
        // Another order, `24:00`, one window per weekend day: the same hours.
        let mut again = c.clone();
        again.windows = vec![
            window(vec![7], &["00:00-24:00"]),
            window(vec![6], &["00:00-24:00"]),
            window(vec![5, 4, 3, 2, 1], &["10:00-24:00", "04:00-06:00", "00:00-01:00"]),
        ];
        assert_eq!(relative_change(&current, &again), Some(0.0));
        assert_eq!(merged(Some(&current), &again).windows, current.windows, "stored in one form");
        // Other hours are a change.
        again.windows[0] = window(vec![7], &["00:00-12:00"]);
        assert_eq!(relative_change(&current, &again), None);
        // `24:00` is midnight in a stored window too.
        let stored = crate::model::PriceWindow {
            variant: "off_peak".into(),
            weekdays: vec![1],
            hours_utc: vec!["22:00-24:00".into()],
        };
        assert_eq!(stored.spans(), Some(vec![(22 * 60, 24 * 60)]));
    }

    fn price(rates: Rates, variants: BTreeMap<String, Rates>) -> Price {
        Price {
            model_id: "m".into(),
            version: 1,
            currency: "USD".into(),
            rates,
            variants,
            tiers: Vec::new(),
            windows: Vec::new(),
            effective_from: Utc::now(),
            source: PriceSource::Feed,
            status: PriceStatus::Active,
            evidence_url: String::new(),
            evidence_text: String::new(),
            confirmed_at: None,
            pinned: false,
        }
    }

    #[test]
    fn candidates_are_checked_for_units_and_ratios() {
        assert!(validate(&candidate(2.0, 10.0)).is_ok());
        assert!(validate(&with(candidate(2.0, 10.0), |r| r.cache_read = Some(0.2))).is_ok());
        assert!(validate(&variant(candidate(2.0, 10.0), "batch", 1.0, 5.0)).is_ok());
        // Speech has no token rates; a web search is priced per call.
        let speech = CandidateRates { per_character: Some(0.000015), ..Default::default() };
        assert!(validate(&PriceCandidate { rates: speech, ..candidate(1.0, 1.0) }).is_ok());
        assert!(validate(&with(candidate(3.0, 15.0), |r| r.per_server_tool_call = Some(0.01))).is_ok());
        for (bad, why) in [
            (candidate(0.0, 10.0), "input"),
            (candidate(-1.0, 10.0), "input"),
            (candidate(2.0, f64::NAN), "output"),
            (candidate(20_000.0, 60_000.0), "plausible"),
            (candidate(10.0, 2.0), "far below"),
            (with(candidate(2.0, 10.0), |r| r.cache_read = Some(3.0)), "cache"),
            (with(candidate(2.0, 10.0), |r| r.per_character = Some(15.0)), "per character"),
            (with(candidate(2.0, 10.0), |r| r.per_request = Some(1000.0)), "per request"),
            (PriceCandidate { currency: "usd".into(), ..candidate(2.0, 10.0) }, "currency"),
            (PriceCandidate { rates: CandidateRates::default(), ..candidate(2.0, 10.0) }, "at least one"),
            (variant(candidate(2.0, 10.0), "batch", 10.0, 1.0), "batch output"),
            (variant(candidate(2.0, 10.0), "Batch!", 1.0, 5.0), "variant name"),
        ] {
            let e = validate(&bad).unwrap_err();
            assert!(e.contains(why), "{why}: {e}");
        }
    }

    #[test]
    fn a_change_is_the_largest_relative_difference() {
        let rates = Rates {
            input: Some(2.0),
            output: Some(10.0),
            cache_read: Some(0.2),
            per_character: Some(0.1),
            ..Default::default()
        };
        let batch = Rates { input: Some(1.0), output: Some(5.0), ..Default::default() };
        let current = price(rates, BTreeMap::from([("batch".to_owned(), batch)]));
        assert_eq!(relative_change(&current, &candidate(2.0, 10.0)), Some(0.0));
        assert_eq!(relative_change(&current, &candidate(2.5, 10.0)), Some(0.25));
        assert_eq!(relative_change(&current, &candidate(1.0, 11.0)), Some(0.5));
        // Variants and rates per unit count too.
        assert_eq!(relative_change(&current, &variant(candidate(2.0, 10.0), "batch", 1.0, 7.5)), Some(0.5));
        let per_char = relative_change(&current, &with(candidate(2.0, 10.0), |r| r.per_character = Some(0.12)));
        assert!(per_char.is_some_and(|x| (x - 0.2).abs() < 1e-9), "{per_char:?}");
        // A rate or a variant the current price does not have cannot be measured as one figure.
        assert_eq!(relative_change(&current, &with(candidate(2.0, 10.0), |r| r.cache_write_5m = Some(2.5))), None);
        assert_eq!(relative_change(&current, &variant(candidate(2.0, 10.0), "priority", 3.0, 15.0)), None);
        // Rates and variants the candidate does not name are kept when it is applied.
        let next = merged(Some(&current), &variant(candidate(3.0, 15.0), "flex", 1.5, 7.5));
        assert_eq!(
            (next.rates.input, next.rates.cache_read, next.rates.per_character),
            (Some(3.0), Some(0.2), Some(0.1))
        );
        assert_eq!(next.variants["batch"].output, Some(5.0));
        assert_eq!(next.variants["flex"].input, Some(1.5));
    }
}
