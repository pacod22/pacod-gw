//! Model data from outside the configuration: the daily sync from pacod-aggregator, OpenRouter
//! or LiteLLM, the models file of the configuration, and uploaded files. Each is read into a
//! catalogue (`pacod.catalog/v1`) and applied alike: prices through the price job's decision
//! (every valid price applies unless pinned), properties a source changed through
//! `write_model_fields`, and models the repository lacks as proposals of model discovery.
//!
//! Three jobs keep their runs apart: `catalog_sync` on its schedule or when asked for,
//! `catalog_file` when the models file is read (at start and after a provider changed), and
//! `catalog_upload`. One waits for another: an upload is refused while a run is open, and a
//! round of upkeep leaves the first two for the next round while an upload is open.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use chrono::Utc;
use pacod_agg_catalog::{self as cat, Catalog, Format, Found};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::AdminState;
use super::auth::Principal;
use super::error::{ApiError, ApiResult};
use super::resources::pricejob::{ModelResult, PriceRunResult, counts, decide, result};
use super::resources::repo::write_model_fields;
use crate::catalog::convert;
use crate::catalog::fetch::Fetcher;
use crate::model::repo::model_fact;
use crate::model::{
    CatalogSource, Doc, Model, ModelKind, ModelState, Obj, Price, Provider, Status, new_id, preset_for_base_url,
};
use crate::store::AuditEntry;
use crate::store::catalog::Seen;
use crate::store::jobs::JobRun;

/// The sync on its schedule, or when asked for.
pub const JOB: &str = "catalog_sync";
/// The models file read.
pub const JOB_FILE: &str = "catalog_file";
/// A file an administrator uploaded.
pub const JOB_UPLOAD: &str = "catalog_upload";

/// What a run left open is closed with.
const LEFT_OPEN: &str = "did not finish: the instance running it stopped, or could not record its end";

/// The sync's state in this process.
pub struct SyncState {
    pub fetcher: Fetcher,
    /// The models file is to be read at the next upkeep round: at start, and after a provider
    /// was created or changed, whose models the file may price.
    file_due: AtomicBool,
}

impl SyncState {
    pub fn new(egress: std::sync::Arc<crate::upstream::EgressPolicy>) -> anyhow::Result<Self> {
        Ok(Self { fetcher: Fetcher::new(egress)?, file_due: AtomicBool::new(true) })
    }

    /// Has the models file read again at the next upkeep round.
    pub fn file_changed(&self) {
        self.file_due.store(true, Ordering::Relaxed);
    }
}

/// The vendor whose catalogue entries a provider's models are: the provider's own setting, or
/// its preset's by its address. `None` (a provider set to `none`) keeps its models out of every
/// sync.
pub fn vendor_of(provider: &Provider) -> Option<String> {
    match provider.catalog_vendor.as_deref().map(str::trim) {
        Some("none") => None,
        Some(v) if !v.is_empty() => Some(v.to_ascii_lowercase()),
        _ => preset_for_base_url(&provider.base_url).and_then(|p| p.catalog_vendor).map(str::to_owned),
    }
}

/// How one catalogue is applied.
struct Reading<'a> {
    catalog: &'a Catalog,
    /// The name its rows in `catalog_seen` and its proposals carry: a source's name, or `upload`.
    source: &'static str,
    /// Where it was read from, for the evidence of its prices.
    url: String,
    /// The price candidate's source: `feed` for a sync and the file, `import` for an upload.
    candidate_source: &'static str,
    /// The models file: a value applies only when the file's own value changed since it was
    /// last read, so that a restart does not undo what the daily sync applied since.
    changes_only: bool,
    /// The discovery source of its proposals; `None`: it proposes nothing.
    propose: Option<&'static str>,
    /// The vendors whose prices apply (OpenRouter); `None`: every vendor's.
    price_vendors: Option<Vec<String>>,
    properties: bool,
    /// The models a run is limited to.
    only: Option<HashSet<String>>,
    run_id: &'a str,
}

/// Whether an entry is a gateway model's: by its id or an alias, its slug, or the id without a
/// snapshot's date.
fn names(entry: &cat::Model, upstream_id: &str) -> bool {
    let catalog = Catalog { models: vec![entry.clone()], ..Default::default() };
    let vendor = entry.vendor.clone().unwrap_or_default();
    catalog.index().find(&vendor, upstream_id).one().is_some()
}

fn active(p: &Provider) -> bool {
    p.status == Status::Active
}

/// Applies a catalogue, and returns the outcome per model and what the run is to record.
async fn apply(st: &AdminState, r: &Reading<'_>) -> ApiResult<(Vec<ModelResult>, Value)> {
    let run_id = r.run_id;
    let snap = st.config.snapshot();
    let index = r.catalog.index();
    let before: HashMap<(String, String), cat::Model> = st
        .store
        .catalog_seen(r.source)
        .await?
        .into_iter()
        .filter_map(|(k, v)| Some((k, serde_json::from_value(v).ok()?)))
        .collect();
    let read_before = st.store.catalog_read().await?;
    let mut read_now = Vec::new();
    let vendors: HashMap<String, String> = snap
        .providers
        .values()
        .filter(|p| active(&p.doc))
        .filter_map(|p| vendor_of(&p.doc).map(|v| (p.id.clone(), v)))
        .collect();
    let mapped: HashSet<&str> = vendors.values().map(String::as_str).collect();
    // A vendor whose list shrank to less than half of what this source listed before is
    // left alone: a source that lost most of a vendor's models is more likely broken than
    // the vendor gone.
    let listed: BTreeMap<String, usize> = mapped.iter().map(|v| ((*v).to_owned(), index.models_of(v).len())).collect();
    let listed_before = previous_counts(st, r.source).await;
    let shrunk: HashSet<&str> = listed
        .iter()
        .filter(|(v, now)| listed_before.get(*v).is_some_and(|b| *b >= 10 && **now * 2 < *b))
        .map(|(v, _)| v.as_str())
        .collect();

    let mut audit = AuditEntry::system("model-data");
    audit.kind = Price::KIND.into();
    audit.detail = json!({ "run_id": run_id, "source": r.source });
    let owner = format!("catalog:{}", r.source);
    let mut models: Vec<_> = snap
        .models
        .values()
        .filter(|m| vendors.contains_key(&m.doc.provider_id))
        .filter(|m| !matches!(m.doc.state, ModelState::Retired | ModelState::Disabled))
        .filter(|m| r.only.as_ref().is_none_or(|only| only.contains(&m.id)))
        .collect();
    models.sort_by(|a, b| a.doc.name.cmp(&b.doc.name));
    let mut matched: HashMap<(String, String), String> = HashMap::new();
    let mut out = Vec::new();
    let mut tx = st.store.begin().await?;
    for m in models {
        let vendor = &vendors[&m.doc.provider_id];
        let name = &m.doc.name;
        if shrunk.contains(vendor.as_str()) {
            out.push(result(
                name,
                "skipped",
                format!("the source lists less than half the {vendor} models it listed before"),
            ));
            continue;
        }
        let entry = match index.find(vendor, &m.doc.upstream_id) {
            Found::One(e) => e,
            Found::None => {
                out.push(result(
                    name,
                    "not_listed",
                    format!("the source lists no {vendor} model {}", m.doc.upstream_id),
                ));
                continue;
            }
            Found::Ambiguous(ids) => {
                out.push(result(name, "ambiguous", format!("the source lists several: {}", ids.join(", "))));
                continue;
            }
        };
        let key = (vendor.clone(), cat::ids::normalise_model_id(&entry.api_id));
        matched.insert(key.clone(), m.id.clone());
        let seen = before.get(&key);
        let Some(row) = tx.get(&m.id).await? else { continue };
        let model: Obj<Model> = row.into_obj()?;
        let price_taken = r.price_vendors.as_ref().is_none_or(|vs| vs.iter().any(|v| v == vendor));
        let mut res = match &entry.price {
            _ if m.doc.free => result(name, "free", ""),
            Some(p) if !price_taken => {
                let _ = p;
                result(name, "price_not_taken", format!("{vendor} prices are not taken from this source"))
            }
            Some(p) if r.changes_only && seen.and_then(|s| s.price.as_ref()) == Some(p) => {
                result(name, "kept", "the file's price did not change since it was last read")
            }
            Some(p) => {
                let c = convert::candidate(&m.id, p, r.candidate_source, &r.url);
                let mut res = decide(&mut tx, &snap, &model, &c, &audit).await?;
                res.page = c.evidence_url;
                res
            }
            None => result(name, "no_price", "the source states no price for it"),
        };
        if r.properties {
            // What a source said before says nothing to a model that was not there to hear it: a
            // model no source has read takes it all, and from then on only what changes. Its
            // price needs none of this: a model is priced when it is made and when it goes live.
            let stated_before = seen.filter(|_| read_before.contains(&m.id)).map(convert::facts).unwrap_or_default();
            let mut window = model.doc.context_window.map(u64::from);
            let mut writes: Vec<(String, Value)> = Vec::new();
            for (field, value) in convert::facts(entry) {
                if field == "context_window" {
                    window = value.as_u64().or(window);
                }
                let unchanged_at_source = stated_before.iter().any(|(f, v)| *f == field && *v == value);
                if unchanged_at_source
                    || model_fact(&model.doc, field).as_ref() == Some(&value)
                    || !convert::plausible(field, &value, window)
                {
                    continue;
                }
                writes.push((field.to_owned(), value));
            }
            if !writes.is_empty() {
                let mut entry_audit = audit.clone();
                entry_audit.action = "update_from_model_data".into();
                entry_audit.kind = Model::KIND.into();
                entry_audit.detail = json!({ "run_id": run_id, "source": r.source,
                                             "fields": writes.iter().map(|(f, v)| json!({ "field": f, "value": v })).collect::<Vec<_>>() });
                match write_model_fields(st, &mut tx, &model, &writes, "model-data", Some(&owner), entry_audit).await {
                    Ok(_) => res.properties = writes.into_iter().map(|(f, _)| f).collect(),
                    Err(e) => {
                        res.detail =
                            format!("{} (properties not applied: {})", res.detail, e.message).trim().to_owned();
                    }
                }
            }
        }
        if r.properties {
            read_now.push(m.id.clone());
        }
        out.push(res);
    }
    st.commit(tx).await?;
    st.store.catalog_read_record(r.source, &read_now).await?;

    // What the source listed of the vendors the gateway serves, for the next reading.
    let mut rows: BTreeMap<(String, String), Seen> = BTreeMap::new();
    for e in &r.catalog.models {
        let Some(vendor) = e.vendor.as_deref().filter(|v| mapped.contains(v)) else { continue };
        let key = (vendor.to_owned(), cat::ids::normalise_model_id(&e.api_id));
        let entry = serde_json::to_value(e).map_err(ApiError::internal)?;
        rows.entry(key.clone()).or_insert(Seen {
            vendor: key.0.clone(),
            api_id: key.1.clone(),
            entry,
            model_id: matched.get(&key).cloned(),
        });
    }
    st.store.catalog_seen_record(r.source, &rows.into_values().collect::<Vec<_>>()).await?;

    let proposed = match r.propose {
        Some(source) => propose(st, &snap, r, &index, &vendors, &shrunk, source).await?,
        None => 0,
    };
    out.sort_by(|a, b| a.model.cmp(&b.model));
    let summary = json!({ "source": r.source, "url": r.url, "listed": listed, "proposed": proposed });
    Ok((out, summary))
}

/// Models a catalogue lists that the repository lacks become proposals of model discovery,
/// for each active provider of their vendor. The aggregator proposes only ids a vendor states
/// itself; the models file is the operator's own and proposes what it lists. Open proposals the
/// source no longer lists are resolved where it lists the vendor completely.
async fn propose(
    st: &AdminState,
    snap: &crate::snapshot::Snapshot,
    r: &Reading<'_>,
    index: &cat::Index<'_>,
    vendors: &HashMap<String, String>,
    shrunk: &HashSet<&str>,
    source: &'static str,
) -> ApiResult<usize> {
    // The run's start as the database stamped it: `last_seen` is the database's time too, and
    // the two clocks need not agree.
    let started = st.store.job_run(r.run_id).await?.map(|r| r.started_at).unwrap_or_else(Utc::now);
    let mut proposed = 0;
    for (provider_id, vendor) in vendors {
        if shrunk.contains(vendor.as_str()) {
            continue;
        }
        let repository: Vec<&str> = snap
            .models
            .values()
            .filter(|m| &m.doc.provider_id == provider_id)
            .map(|m| m.doc.upstream_id.as_str())
            .collect();
        for e in index.models_of(vendor) {
            if (source == "catalog" && !e.api_id_confirmed) || repository.iter().any(|u| names(e, u)) {
                continue;
            }
            let p = &e.properties;
            // The keys a proposal from the provider's own list has, so that accepting it works alike.
            let details = json!({
                "display_name": e.display_name.clone().unwrap_or_default(),
                "context_window": p.context_window, "max_output_tokens": p.max_output_tokens,
                "vision": p.vision, "reasons": p.reasoning, "structured_output": p.structured_output,
                "tools": p.tools, "embedding": convert::kind(e) == Some(ModelKind::Embedding),
                "catalog": { "source": r.source, "url": r.url },
                "price": e.price,
            });
            st.store.catalog_proposal_seen(&new_id("dsp"), provider_id, &e.api_id, &details, source).await?;
            proposed += 1;
        }
        let complete = source == "file" || r.catalog.vendor(vendor).is_some_and(|v| v.complete);
        if complete {
            st.store.catalog_proposals_resolve_unseen(provider_id, source, started).await?;
        }
    }
    Ok(proposed)
}

/// How many models of each vendor `source` listed at its last successful run.
async fn previous_counts(st: &AdminState, source: &str) -> HashMap<String, usize> {
    let runs = st.store.job_runs_of(&[JOB, JOB_FILE], 20).await.unwrap_or_default();
    runs.into_iter()
        .filter(|run| run.ok == Some(true) && run.result["source"] == source)
        .find_map(|run| serde_json::from_value(run.result["listed"].clone()).ok())
        .unwrap_or_default()
}

/// Where a source is read from, as the evidence of a price names it.
fn address_of(st: &AdminState, job: &crate::model::PriceJob, source: CatalogSource) -> String {
    let boot = &st.boot.catalog;
    match source {
        CatalogSource::PacodAggregator => {
            boot.aggregator.as_ref().map_or_else(|| source.name().to_owned(), |a| format!("{}/v1/catalog", a.url))
        }
        CatalogSource::Openrouter => job.openrouter_url.clone(),
        CatalogSource::Litellm => job.litellm_url.clone(),
        CatalogSource::File => {
            boot.models_file.as_ref().map_or_else(|| source.name().to_owned(), |p| p.display().to_string())
        }
    }
}

/// A run that had nothing to read, with the reason.
fn skipped(reason: &str) -> (Vec<ModelResult>, Value, String) {
    tracing::warn!(reason, "the sync of model data read nothing");
    (Vec::new(), json!({ "skipped": reason }), reason.to_owned())
}

/// Reads the job's source and applies it.
async fn sync_once(
    st: &AdminState,
    run_id: &str,
    only: Option<HashSet<String>>,
) -> anyhow::Result<(Vec<ModelResult>, Value, String)> {
    let job = st.config.snapshot().price_job.clone();
    let source = job.source;
    let fetcher = &st.catalog.fetcher;
    let (body, url, format) = match source {
        CatalogSource::PacodAggregator => {
            let Some(link) = &st.boot.catalog.aggregator else {
                return Ok(skipped("the source is pacod-aggregator, but PGW_AGGREGATOR_URL is not set"));
            };
            let url = format!("{}/v1/catalog", link.url);
            (fetcher.get_aggregator(&url, link.token.as_deref()).await?, url, Some(Format::Catalog))
        }
        CatalogSource::Openrouter => {
            (fetcher.get_public(&job.openrouter_url).await?, job.openrouter_url.clone(), Some(Format::Openrouter))
        }
        CatalogSource::Litellm => {
            (fetcher.get_public(&job.litellm_url).await?, job.litellm_url.clone(), Some(Format::Litellm))
        }
        CatalogSource::File => {
            let Some(path) = &st.boot.catalog.models_file else {
                return Ok(skipped("the source is the models file, but PGW_MODELS_FILE is not set"));
            };
            (tokio::fs::read(path).await?, path.display().to_string(), None)
        }
    };
    let catalog = cat::parse(&body, format).map_err(|e| anyhow::anyhow!("{url}: {e}"))?;
    let reading = Reading {
        catalog: &catalog,
        source: source.name(),
        url,
        candidate_source: "feed",
        changes_only: source == CatalogSource::File,
        propose: match source {
            CatalogSource::PacodAggregator => Some("catalog"),
            CatalogSource::File => Some("file"),
            _ => None,
        },
        price_vendors: (source == CatalogSource::Openrouter).then(|| job.openrouter_price_vendors.clone()),
        properties: job.apply_properties,
        only,
        run_id,
    };
    let (models, summary) = apply(st, &reading).await.map_err(|e| anyhow::anyhow!(e.message))?;
    Ok((models, summary, String::new()))
}

/// Records a run's end and announces it.
async fn finish(
    st: &AdminState,
    job: &str,
    run_id: &str,
    outcome: anyhow::Result<(Vec<ModelResult>, Value, String)>,
) -> anyhow::Result<()> {
    let (ok, message, result) = match outcome {
        Ok((models, mut summary, message)) => {
            summary["counts"] = counts(&models);
            summary["models"] = json!(models);
            (true, message, summary)
        }
        Err(e) => (false, format!("{e:#}"), json!({})),
    };
    if st.store.job_finish_run(run_id, ok, &message, &result).await? {
        st.announce(
            "job",
            json!({ "job": job, "run_id": run_id, "status": if ok { "finished" } else { "failed" }, "message": message }),
        );
    }
    if !ok {
        tracing::warn!(job, run_id, error = %message, "model data: the run failed");
    }
    Ok(())
}

/// The sync, on its schedule or when asked for.
pub async fn run(st: &AdminState, trigger: &str) -> anyhow::Result<()> {
    let state = st.store.job_state(JOB).await?;
    let only = state.run_models.clone();
    let run_id = new_id("run");
    st.store.job_start_run(JOB, &run_id, trigger, only.as_deref()).await?;
    st.announce("job", json!({ "job": JOB, "run_id": run_id, "status": "started", "trigger": trigger }));
    let outcome = sync_once(st, &run_id, only.map(|m| m.into_iter().collect())).await;
    finish(st, JOB, &run_id, outcome).await
}

/// Reads the models file, when one is configured and it is due.
pub async fn read_file_when_due(st: &AdminState) -> anyhow::Result<()> {
    let Some(path) = st.boot.catalog.models_file.clone() else { return Ok(()) };
    if !st.catalog.file_due.swap(false, Ordering::Relaxed) {
        return Ok(());
    }
    let run_id = new_id("run");
    st.store.job_start_run(JOB_FILE, &run_id, "file", None).await?;
    let outcome = async {
        let body = tokio::fs::read(&path).await.map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        let catalog = cat::parse(&body, None).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        let job = st.config.snapshot().price_job.clone();
        let reading = Reading {
            catalog: &catalog,
            source: CatalogSource::File.name(),
            url: path.display().to_string(),
            candidate_source: "feed",
            changes_only: true,
            propose: Some("file"),
            price_vendors: None,
            properties: job.apply_properties,
            only: None,
            run_id: &run_id,
        };
        let (models, summary) = apply(st, &reading).await.map_err(|e| anyhow::anyhow!(e.message))?;
        anyhow::Ok((models, summary, String::new()))
    }
    .await;
    finish(st, JOB_FILE, &run_id, outcome).await
}

/// Whether an upload is being applied. It runs in a request, not in a round of upkeep, on any
/// instance: an open run counts as in progress until it is older than any request can be.
async fn upload_in_progress(st: &AdminState) -> anyhow::Result<bool> {
    let Some(open) = st.store.job_state(JOB_UPLOAD).await?.current_run else { return Ok(false) };
    Ok(st.store.job_run(&open).await?.is_some_and(|r| Utc::now() - r.started_at < chrono::Duration::minutes(10)))
}

/// Closes runs still open when an upkeep round begins: their instance stopped during them.
pub async fn close_left_open(st: &AdminState) -> anyhow::Result<()> {
    for job in [JOB, JOB_FILE, JOB_UPLOAD] {
        let Some(open) = st.store.job_state(job).await?.current_run else { continue };
        if job == JOB_UPLOAD && upload_in_progress(st).await? {
            continue;
        }
        if st.store.job_finish_run(&open, false, LEFT_OPEN, &json!({})).await? {
            st.announce("job", json!({ "job": job, "run_id": open, "status": "failed", "message": LEFT_OPEN }));
        }
    }
    Ok(())
}

/// Whether a sync is due: asked for, or by the job's schedule since the last sync started.
pub async fn due(st: &AdminState) -> anyhow::Result<bool> {
    let state = st.store.job_state(JOB).await?;
    if state.current_run.is_some() {
        return Ok(false);
    }
    if state.run_requested {
        return Ok(true);
    }
    Ok(st.boot.scheduled_catalog && st.config.snapshot().price_job.due(state.last_started_at, Utc::now()))
}

/// One upkeep round's share: the models file when due, then the sync when due.
pub async fn upkeep(st: &AdminState) {
    if let Err(e) = close_left_open(st).await {
        tracing::warn!(error = %e, "upkeep: closing a model-data run left open failed");
    }
    // What is due stays due: the next round does it.
    match upload_in_progress(st).await {
        Ok(false) => {}
        Ok(true) => return,
        Err(e) => {
            tracing::warn!(error = %e, "upkeep: the model data could not be read");
            return;
        }
    }
    if let Err(e) = read_file_when_due(st).await {
        tracing::warn!(error = %e, "upkeep: reading the models file failed");
    }
    match due(st).await {
        Ok(true) => {
            let state = st.store.job_state(JOB).await.unwrap_or_default();
            let trigger = match (state.run_requested, state.run_models.is_some()) {
                (true, true) => "activation",
                (true, false) => "manual",
                (false, _) => "schedule",
            };
            if let Err(e) = run(st, trigger).await {
                tracing::warn!(error = %e, "upkeep: the model-data sync failed");
            }
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "upkeep: the model-data sync could not be scheduled"),
    }
}

/// Prices a model at once from what the sources last said: a draft accepted from discovery, or
/// a model that has just become active. The job's source comes first, then the models file.
/// `false` when neither prices it.
pub async fn price_from_seen(st: &AdminState, model_id: &str) -> ApiResult<bool> {
    let snap = st.config.snapshot();
    let Some(m) = snap.models.get(model_id) else { return Ok(false) };
    let Some(vendor) = snap.providers.get(&m.doc.provider_id).and_then(|p| vendor_of(&p.doc)) else {
        return Ok(false);
    };
    let job = &snap.price_job;
    let mut sources = vec![job.source];
    if st.boot.catalog.models_file.is_some() && !sources.contains(&CatalogSource::File) {
        sources.push(CatalogSource::File);
    }
    for source in sources {
        let entries: Vec<cat::Model> = st
            .store
            .catalog_seen(source.name())
            .await?
            .into_iter()
            .filter(|((v, _), _)| *v == vendor)
            .filter_map(|(_, e)| serde_json::from_value(e).ok())
            .collect();
        let catalog = Catalog { models: entries, ..Default::default() };
        let Some(entry) = catalog.index().find(&vendor, &m.doc.upstream_id).one() else { continue };
        let taken = source != CatalogSource::Openrouter || job.openrouter_price_vendors.contains(&vendor);
        let Some(price) = entry.price.as_ref().filter(|_| taken) else { continue };
        let mut tx = st.store.begin().await?;
        let Some(row) = tx.get(model_id).await? else { return Ok(false) };
        let model: Obj<Model> = row.into_obj()?;
        let mut audit = AuditEntry::system("model-data");
        audit.kind = Price::KIND.into();
        audit.detail = json!({ "source": source.name(), "for": "activation" });
        let c = convert::candidate(model_id, price, "feed", &address_of(st, job, source));
        let r = decide(&mut tx, &snap, &model, &c, &audit).await?;
        st.commit(tx).await?;
        if matches!(r.outcome.as_str(), "applied" | "unchanged" | "pinned") {
            return Ok(true);
        }
    }
    Ok(false)
}

// --- the API -------------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct UploadQuery {
    /// `auto` (the default), `catalog`, `openrouter`, `litellm` or `pacod`.
    pub format: Option<String>,
}

/// The most an upload may hold: LiteLLM's list is about 3.5 MB today.
pub const MAX_UPLOAD_BYTES: usize = 16 << 20;

/// Applies a file of models, parameters and prices: a catalogue, or OpenRouter's or LiteLLM's
/// list, or a pacod price feed. Its prices apply as the sync's do, its properties where they
/// differ from the model's; it proposes no models.
pub async fn upload(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Json<PriceRunResult>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let format = match q.format.as_deref().map(str::trim) {
        None | Some("") | Some("auto") => None,
        Some(name) => Some(Format::from_name(name).ok_or_else(|| {
            ApiError::invalid(format!("format \"{name}\": auto, catalog, openrouter, litellm or pacod"))
        })?),
    };
    for job in [JOB, JOB_FILE, JOB_UPLOAD] {
        if st.store.job_state(job).await?.current_run.is_some() {
            return Err(ApiError::conflict("model data is being applied now; upload again when the run has finished"));
        }
    }
    let catalog = cat::parse(&body, format).map_err(|e| ApiError::invalid(e.to_string()))?;
    if catalog.models.is_empty() {
        return Err(ApiError::invalid("the file lists no models"));
    }
    let run_id = new_id("run");
    st.store.job_start_run(JOB_UPLOAD, &run_id, "upload", None).await?;
    let job = st.config.snapshot().price_job.clone();
    let reading = Reading {
        catalog: &catalog,
        source: "upload",
        url: "upload".into(),
        candidate_source: "import",
        changes_only: false,
        propose: None,
        price_vendors: (catalog.source.kind == cat::SourceKind::Openrouter)
            .then(|| job.openrouter_price_vendors.clone()),
        properties: job.apply_properties,
        only: None,
        run_id: &run_id,
    };
    let applied = apply(&st, &reading).await;
    let mut entry = p.audit(None, None);
    entry.action = "upload_model_data".into();
    entry.kind = crate::model::PriceJob::KIND.into();
    entry.object_id = crate::model::PRICE_JOB_ID.into();
    entry.detail = json!({ "run_id": run_id, "models": catalog.models.len() });
    st.store.audit(&entry).await?;
    match applied {
        Ok((models, summary)) => {
            let counts = counts(&models);
            finish(&st, JOB_UPLOAD, &run_id, Ok((models.clone(), summary, String::new())))
                .await
                .map_err(ApiError::internal)?;
            Ok(Json(PriceRunResult { run_id, models, counts }))
        }
        Err(e) => {
            finish(&st, JOB_UPLOAD, &run_id, Err(anyhow::anyhow!(e.message.clone())))
                .await
                .map_err(ApiError::internal)?;
            Err(e)
        }
    }
}

/// What the console shows of the sync.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SyncView {
    /// pacod-aggregator as the operator configured it (`PGW_AGGREGATOR_URL`).
    pub aggregator: AggregatorView,
    /// The models file (`PGW_MODELS_FILE`), and its last reading.
    pub models_file: Option<ModelsFileView>,
    /// The job's fields the environment sets at every start (`PGW_CATALOG_*`), until the job is
    /// saved in the console.
    pub seeded: Vec<String>,
    /// The active providers the sync covers: those with a vendor a catalogue lists.
    pub providers: Vec<String>,
    pub last_started_at: Option<chrono::DateTime<Utc>>,
    /// The next scheduled sync. Absent while it is switched off.
    pub next_run_at: Option<chrono::DateTime<Utc>>,
    pub run_requested: bool,
    pub running: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AggregatorView {
    pub configured: bool,
    pub url: Option<String>,
    /// Whether a token is sent; never the token.
    pub token: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ModelsFileView {
    pub path: String,
    pub last_read: Option<JobRun>,
}

pub async fn view(st: &AdminState, job: &crate::model::PriceJob) -> ApiResult<SyncView> {
    let state = st.store.job_state(JOB).await?;
    let next_run_at = job
        .enabled
        .then(|| {
            let from = state.last_started_at.unwrap_or_else(Utc::now);
            crate::model::pricejob::next_after(&job.schedule, &job.time_zone, from)
        })
        .flatten();
    let models_file = match &st.boot.catalog.models_file {
        Some(path) => Some(ModelsFileView {
            path: path.display().to_string(),
            last_read: st.store.job_last_finished(JOB_FILE).await?,
        }),
        None => None,
    };
    let seed = &st.boot.catalog.seed;
    let seeded = [
        ("enabled", seed.enabled.is_some()),
        ("source", seed.source.is_some()),
        ("schedule", seed.schedule.is_some()),
        ("time_zone", seed.time_zone.is_some()),
        ("openrouter_url", seed.openrouter_url.is_some()),
        ("litellm_url", seed.litellm_url.is_some()),
    ]
    .into_iter()
    .filter(|(_, set)| *set && job.origin == crate::model::Origin::Env)
    .map(|(f, _)| f.to_owned())
    .collect();
    let mut providers: Vec<String> = st
        .config
        .snapshot()
        .providers
        .values()
        .filter(|p| active(&p.doc) && vendor_of(&p.doc).is_some())
        .map(|p| p.id.clone())
        .collect();
    providers.sort();
    Ok(SyncView {
        providers,
        aggregator: AggregatorView {
            configured: st.boot.catalog.aggregator.is_some(),
            url: st.boot.catalog.aggregator.as_ref().map(|a| a.url.clone()),
            token: st.boot.catalog.aggregator.as_ref().is_some_and(|a| a.token.is_some()),
        },
        models_file,
        seeded,
        last_started_at: state.last_started_at,
        next_run_at,
        run_requested: state.run_requested,
        running: state.current_run.is_some(),
    })
}

/// What keeps the sync from doing its work, in sentences. An installation that switched the
/// sync off has no such problem.
pub fn problems(st: &AdminState, job: &crate::model::PriceJob) -> Vec<String> {
    let mut out = Vec::new();
    if !job.enabled {
        return out;
    }
    match job.source {
        CatalogSource::PacodAggregator if st.boot.catalog.aggregator.is_none() => out.push(
            "The model data comes from pacod-aggregator, but PGW_AGGREGATOR_URL is not set: nothing is synced.".into(),
        ),
        CatalogSource::File if st.boot.catalog.models_file.is_none() => {
            out.push("The model data comes from the models file, but PGW_MODELS_FILE is not set.".into())
        }
        _ => {}
    }
    out
}

/// Sets the job's values from the environment (`PGW_CATALOG_*`) at start, as long as no
/// administrator saved the job: a job saved in the console, or saved by an earlier release,
/// keeps its values.
pub async fn ensure_price_job(store: &crate::store::Store, seed: &crate::bootstrap::CatalogSeed) -> anyhow::Result<()> {
    use crate::model::{Origin, PRICE_JOB_ID, PriceJob};
    if seed.is_empty() {
        return Ok(());
    }
    let mut tx = store.begin().await?;
    let stored = tx.get(PRICE_JOB_ID).await?;
    let mut job: PriceJob = match &stored {
        Some(row) => serde_json::from_value(row.doc.clone())?,
        None => PriceJob { origin: Origin::Env, ..PriceJob::default() },
    };
    if job.origin == Origin::Console && stored.is_some() {
        return Ok(());
    }
    let before = job.clone();
    seed.apply(&mut job);
    job.origin = Origin::Env;
    if stored.is_some() && job == before {
        return Ok(());
    }
    let audit = AuditEntry::system("environment");
    let doc = serde_json::to_value(&job)?;
    match &stored {
        Some(row) => {
            tx.update(PRICE_JOB_ID, Some(row.version), None, &doc, audit).await?;
        }
        None => {
            tx.create(PriceJob::KIND, PRICE_JOB_ID, None, None, &doc, audit).await?;
        }
    }
    tx.commit().await?;
    tracing::info!("the model-data sync takes its settings from the environment (PGW_CATALOG_*)");
    Ok(())
}
