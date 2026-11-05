//! Readings of price pages. The scraper reports each page of a check run; when a page changed,
//! the core has its text read by the extraction model through its own data plane and keeps what
//! the page says. Once every page of a provider in the run was checked, it applies what the
//! pages say about the provider's models: their prices, and the properties a page newly states.

use std::collections::HashSet;
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use chrono::Utc;
use pacod_gw_events::extraction::{
    ModelProperties, PageExtraction, PageModel, PageReading, model_slug, models_from_answer, normalise_model_id,
};
use pacod_gw_events::jobs::{JOB_PRICE_CHECK, PriceCandidate};
use serde_json::{Value, json};

use super::AdminState;
use super::error::{ApiError, ApiResult};
use super::resources::pricejob::{ModelResult, decide, internal, result};
use super::resources::repo::write_model_fields;
use super::scraper::can_extract;
use crate::model::repo::model_fact;
use crate::model::{Doc, Model, ModelState, Obj, PageKind, PageStatus, Price, PricePage, SYSTEM_WS_ID};
use crate::snapshot::Snapshot;
use crate::store::AuditEntry;
use crate::store::jobs::JobRun;
use crate::store::pricepages::{PageState, Recording};

/// The largest part of a page the extraction model is given at once, in characters.
const MAX_CHUNK_CHARS: usize = 120_000;
/// The smallest, whatever the model's context window.
const MIN_CHUNK_CHARS: usize = 8_000;
/// The output an extraction may take: a model page lists many models with many fields.
const EXTRACTION_OUTPUT_TOKENS: u32 = 16_000;

/// The modalities a page may give a model.
const MODALITIES: &[&str] = &["text", "image", "document", "audio", "video", "embedding", "ranking"];

/// Takes the scraper's account of one page of a check run.
pub async fn readings(
    State(st): State<AdminState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Json(reading): Json<PageReading>,
) -> ApiResult<Json<Value>> {
    internal(&st, &headers)?;
    let run = st.store.job_run(&run_id).await?.ok_or_else(|| ApiError::not_found("run"))?;
    if run.finished_at.is_some() {
        return Err(ApiError::conflict("this run is already finished"));
    }
    if run.job != JOB_PRICE_CHECK || !run.models.as_ref().is_some_and(|m| m.contains(&reading.page_id)) {
        return Err(ApiError::conflict("the page is not one this run checks"));
    }
    let snap = st.config.snapshot();
    let Some(page) =
        snap.price_pages.iter().find(|p| p.id == reading.page_id && p.doc.status == PageStatus::Active).cloned()
    else {
        let entry =
            json!({ "page_id": reading.page_id, "outcome": "not_read", "message": "the page is no longer confirmed" });
        st.store.job_run_append(&run_id, "pages", &json!([entry])).await?;
        return Ok(Json(entry));
    };
    let states = st.store.price_page_states().await?;
    let state = states.get(&page.id);
    let force = state.is_none_or(|s| s.force_read || s.last_read_at.is_none());
    let mut rec = Recording {
        http_status: reading.http_status.map(i32::from),
        final_url: reading.final_url.clone(),
        ..Default::default()
    };
    let read = |rec: &mut Recording, ex: &PageExtraction| -> ApiResult<()> {
        rec.outcome = "read".into();
        rec.content_hash = Some(reading.content_hash.clone());
        rec.text_hash = Some(reading.text_hash.clone());
        rec.extraction = Some(serde_json::to_value(ex).map_err(ApiError::internal)?);
        if !ex.notes.is_empty() {
            rec.message = ex.notes.join("; ");
        }
        Ok(())
    };
    let mut models = 0;
    match (reading.outcome.as_str(), page.doc.kind) {
        ("failed", _) => {
            rec.outcome = "failed".into();
            rec.message = if reading.message.is_empty() {
                "the page could not be fetched".into()
            } else {
                reading.message.clone()
            };
        }
        (_, PageKind::Feed) => match &reading.extraction {
            None => (rec.outcome, rec.message) = ("failed".into(), "the scraper sent no models for the feed".into()),
            Some(_) if !force && state.is_some_and(|s| s.content_hash == reading.content_hash) => {
                rec.outcome = "unchanged".into();
            }
            Some(ex) => {
                models = ex.models.len();
                read(&mut rec, ex)?;
            }
        },
        ("unchanged", PageKind::Page) => rec.outcome = "unchanged".into(),
        (_, PageKind::Page) => match reading.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            None => (rec.outcome, rec.message) = ("failed".into(), "the page has no text".into()),
            Some(_) if !can_extract(&snap.price_job) => {
                (rec.outcome, rec.message) = ("not_read".into(), "no extraction model is set".into());
            }
            Some(text) => match extract_page(&st, &snap, &run, &page, text).await {
                Ok(ex) => {
                    models = ex.models.len();
                    read(&mut rec, &ex)?;
                }
                Err(Unread::Budget(why)) => (rec.outcome, rec.message) = ("not_read".into(), why),
                Err(Unread::Failed(why)) => (rec.outcome, rec.message) = ("failed".into(), why),
            },
        },
    }
    if rec.outcome == "failed" {
        tracing::warn!(url = %page.doc.url, message = %rec.message, "a price page could not be read");
    }
    st.store.price_page_record(&page.id, &page.doc.provider_id, &rec).await?;
    let entry = json!({ "page_id": page.id, "url": page.doc.url, "provider_id": page.doc.provider_id,
                        "outcome": rec.outcome, "message": rec.message, "models": models,
                        "http_status": reading.http_status, "elapsed_ms": reading.elapsed_ms });
    st.store.job_run_append(&run_id, "pages", &json!([entry])).await?;
    let settled = settle_when_checked(&st, &run_id, &page.doc.provider_id).await?;
    Ok(Json(json!({ "page_id": page.id, "outcome": rec.outcome, "message": rec.message, "settled": settled })))
}

/// Applies what the provider's pages say, once the run has checked every one of its pages:
/// settling after each page would apply a mix of new and old readings when two pages of a
/// provider changed at once.
async fn settle_when_checked(st: &AdminState, run_id: &str, provider_id: &str) -> ApiResult<Option<Vec<ModelResult>>> {
    let Some(run) = st.store.job_run(run_id).await? else { return Ok(None) };
    if run.result["settled"].as_array().is_some_and(|s| s.iter().any(|p| p == provider_id)) {
        return Ok(None);
    }
    let snap = st.config.snapshot();
    let covered: HashSet<&String> = run.models.iter().flatten().collect();
    let states = st.store.price_page_states().await?;
    let pages: Vec<&Obj<PricePage>> = snap
        .price_pages
        .iter()
        .filter(|p| p.doc.provider_id == provider_id && p.doc.status == PageStatus::Active && covered.contains(&p.id))
        .collect();
    let since = |s: Option<chrono::DateTime<Utc>>| s.is_some_and(|t| t >= run.started_at);
    if !pages.iter().all(|p| since(states.get(&p.id).and_then(|s| s.last_checked_at))) {
        return Ok(None);
    }
    let read_now: HashSet<String> =
        pages.iter().filter(|p| since(states.get(&p.id).and_then(|s| s.last_read_at))).map(|p| p.id.clone()).collect();
    st.store.job_run_append(run_id, "settled", &json!([provider_id])).await?;
    let results = settle_provider(st, &snap, provider_id, &states, &read_now, None, Some(&run)).await?;
    st.store.job_run_append(run_id, "models", &serde_json::to_value(&results).map_err(ApiError::internal)?).await?;
    Ok(Some(results))
}

/// What one page says about one repository model, and what it said at its reading before.
struct Stated<'a> {
    page: &'a Obj<PricePage>,
    now: PageModel,
    before: Option<PageModel>,
}

/// Whether a page's naming of a model is the repository model: by the provider's id, or by the
/// name written as a slug (`GPT-5 mini` for `gpt-5-mini`).
fn names(m: &PageModel, model: &Model) -> bool {
    let id = normalise_model_id(&model.upstream_id);
    let slug = model_slug(&id);
    normalise_model_id(&m.model) == id
        || model_slug(&m.model) == slug
        || m.display_name.as_deref().is_some_and(|d| {
            model_slug(d) == slug
                || (!model.display_name.is_empty() && model_slug(d) == model_slug(&model.display_name))
        })
}

fn extraction_of(v: &Value) -> PageExtraction {
    serde_json::from_value(v.clone()).unwrap_or_default()
}

/// The models the tracker prices: active and deprecated ones, and drafts without a price.
fn provisioned(snap: &Snapshot, model: &crate::snapshot::ModelRt) -> bool {
    match model.doc.state {
        ModelState::Active | ModelState::Deprecated => !model.doc.free,
        ModelState::Draft => !model.doc.free && snap.price_at(&model.id, Utc::now()).is_none(),
        ModelState::Retired | ModelState::Disabled => false,
    }
}

/// A fact a page states, by the field name `model_fact` takes, with its value for the model.
fn facts_of(p: &ModelProperties) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    if let Some(n) = p.context_window {
        out.push(("context_window", json!(n)));
    }
    if let Some(n) = p.max_output_tokens {
        out.push(("max_output_tokens", json!(n)));
    }
    for (field, v) in [
        ("capabilities.vision", p.vision),
        ("capabilities.tools", p.tools),
        ("capabilities.structured_output", p.structured_output),
    ] {
        if let Some(b) = v {
            out.push((field, json!(b)));
        }
    }
    for (field, v) in [("modalities.input", &p.input_modalities), ("modalities.output", &p.output_modalities)] {
        if let Some(list) = v {
            out.push((field, json!(list)));
        }
    }
    out
}

/// Whether a stated fact can be true of a model at all. A misreading (a price read as a
/// context window, a modality the gateway does not know) is left out.
fn plausible(field: &str, value: &Value, context_window: Option<u64>) -> bool {
    match field {
        "context_window" => value.as_u64().is_some_and(|n| (1_000..=100_000_000).contains(&n)),
        "max_output_tokens" => {
            value.as_u64().is_some_and(|n| (1..=10_000_000).contains(&n) && context_window.is_none_or(|w| n <= w))
        }
        "modalities.input" | "modalities.output" => value
            .as_array()
            .is_some_and(|l| !l.is_empty() && l.iter().all(|m| m.as_str().is_some_and(|m| MODALITIES.contains(&m)))),
        _ => value.is_boolean(),
    }
}

/// Applies what the active pages of a provider say about its models. Prices come from every
/// page, the first page by priority that prices a model winning; an unchanged price confirms
/// the price in effect. A property is applied only from a page read now (`read_now`), where the
/// page's value differs from its reading before, and from the first page by priority that states
/// it: a page that did not change does not undo an administrator's edit every day. `only`
/// limits the work to one model.
pub async fn settle_provider(
    st: &AdminState,
    snap: &Snapshot,
    provider_id: &str,
    states: &std::collections::HashMap<String, PageState>,
    read_now: &HashSet<String>,
    only: Option<&str>,
    run: Option<&JobRun>,
) -> ApiResult<Vec<ModelResult>> {
    let pages: Vec<(&Obj<PricePage>, PageExtraction, PageExtraction)> = snap
        .price_pages
        .iter()
        .filter(|p| p.doc.provider_id == provider_id && p.doc.status == PageStatus::Active)
        .filter_map(|p| {
            let s = states.get(&p.id)?;
            Some((p, extraction_of(&s.extraction), extraction_of(&s.previous_extraction)))
        })
        .collect();
    let mut models: Vec<_> = snap
        .models
        .values()
        .filter(|m| m.doc.provider_id == provider_id && only.is_none_or(|id| id == m.id))
        .filter(|m| !matches!(m.doc.state, ModelState::Retired | ModelState::Disabled))
        .collect();
    models.sort_by(|a, b| a.doc.name.cmp(&b.doc.name));
    let mut audit = AuditEntry::system("price-tracker");
    audit.kind = Price::KIND.into();
    audit.detail = json!({ "run_id": run.map(|r| r.id.as_str()) });
    let mut out = Vec::new();
    let mut tx = st.store.begin().await?;
    for m in models {
        let stated: Vec<Stated> = pages
            .iter()
            .filter_map(|(page, now, before)| {
                let now = now.models.iter().find(|x| names(x, &m.doc))?.clone();
                let before = before.models.iter().find(|x| names(x, &m.doc)).cloned();
                Some(Stated { page, now, before })
            })
            .collect();
        let Some(row) = tx.get(&m.id).await? else { continue };
        let model: Obj<Model> = row.into_obj()?;
        // The price, from the first page that has one.
        let priced = stated.iter().find_map(|s| s.now.price.clone().map(|p| (s.page, p)));
        let mut r = match priced {
            _ if m.doc.free => result(&m.doc.name, "free", ""),
            Some((page, c)) => {
                let c = PriceCandidate { model_id: m.id.clone(), evidence_url: page.doc.url.clone(), ..c };
                let mut r = decide(&mut tx, snap, &model, &c, &audit).await?;
                r.page = page.doc.url.clone();
                r
            }
            None if provisioned(snap, m) => result(&m.doc.name, "no_price", "no page of the provider prices it"),
            None => result(&m.doc.name, "not_priced", ""),
        };
        // The properties a page read now changed, each from the first page that states it.
        let mut writes: Vec<(String, Value, &str)> = Vec::new();
        let current_window = model.doc.context_window.map(u64::from);
        let mut fields_seen = HashSet::new();
        for s in &stated {
            for (field, value) in facts_of(&s.now.properties) {
                if !fields_seen.insert(field) || !read_now.contains(&s.page.id) {
                    continue;
                }
                let before: Vec<(&str, Value)> = s.before.as_ref().map(|b| facts_of(&b.properties)).unwrap_or_default();
                let unchanged_on_page = before.iter().any(|(f, v)| *f == field && *v == value);
                let window = writes
                    .iter()
                    .find(|(f, ..)| f == "context_window")
                    .and_then(|(_, v, _)| v.as_u64())
                    .or(current_window);
                if unchanged_on_page
                    || model_fact(&model.doc, field).as_ref() == Some(&value)
                    || !plausible(field, &value, window)
                {
                    continue;
                }
                writes.push((field.to_owned(), value, s.page.id.as_str()));
            }
        }
        if !writes.is_empty() {
            let mut entry = audit.clone();
            entry.action = "update_from_price_page".into();
            entry.kind = Model::KIND.into();
            entry.detail = json!({ "run_id": run.map(|r| r.id.as_str()),
                                   "fields": writes.iter().map(|(f, v, p)| json!({ "field": f, "value": v, "page": p })).collect::<Vec<_>>() });
            // One page owns each field; the fields of one page are written together.
            let mut fresh = model;
            let mut by_page: Vec<&str> = writes.iter().map(|(.., p)| *p).collect();
            by_page.dedup();
            for page in by_page {
                let values: Vec<(String, Value)> =
                    writes.iter().filter(|(.., p)| *p == page).map(|(f, v, _)| (f.clone(), v.clone())).collect();
                match write_model_fields(st, &mut tx, &fresh, &values, "price-tracker", Some(page), entry.clone()).await
                {
                    Ok(updated) => fresh = updated,
                    Err(e) => {
                        r.detail = format!("{} (properties not applied: {})", r.detail, e.message).trim().to_owned();
                        break;
                    }
                }
                r.properties.extend(values.into_iter().map(|(f, _)| f));
            }
        }
        out.push(r);
    }
    st.commit(tx).await?;
    Ok(out)
}

/// Prices a model at once from what its provider's pages said at their last reading: a model
/// that was just activated, or a draft accepted from discovery. `false` when no page prices it.
pub async fn price_from_pages(st: &AdminState, snap: &Snapshot, model_id: &str, provider_id: &str) -> ApiResult<bool> {
    let states = st.store.price_page_states().await?;
    let results = settle_provider(st, snap, provider_id, &states, &HashSet::new(), Some(model_id), None).await?;
    Ok(results.iter().any(|r| matches!(r.outcome.as_str(), "applied" | "unchanged" | "pinned")))
}

/// Why a page was not read.
enum Unread {
    /// The run used up the calls of the extraction model it may make.
    Budget(String),
    Failed(String),
}

/// The context window of the model that reads pages, for the size of the parts of a page.
fn extraction_window(snap: &Snapshot) -> Option<u32> {
    let job = &snap.price_job;
    let model = match (&job.extraction_model, &job.extraction) {
        (Some(name), _) => snap.model_by_name(name),
        (None, Some(x)) => {
            let ws = snap.workspaces.get(&x.workspace_id)?;
            snap.models.get(&ws.templates.get(x.template.trim_start_matches('@'))?.model_id)
        }
        (None, None) => None,
    }?;
    model.doc.context_window
}

/// Has the extraction model read a page's text, in parts where it is long, within the run's
/// budget of calls.
async fn extract_page(
    st: &AdminState,
    snap: &std::sync::Arc<Snapshot>,
    run: &JobRun,
    page: &Obj<PricePage>,
    text: &str,
) -> Result<PageExtraction, Unread> {
    // About three characters a token, with room for the prompt and the answer.
    let window = extraction_window(snap).unwrap_or(32_000) as usize;
    let size = (window.saturating_sub(24_000) * 3).clamp(MIN_CHUNK_CHARS, MAX_CHUNK_CHARS);
    let chunks = chunk_text(text, size);
    let budget = i64::from(snap.price_job.max_extractions_per_run);
    let used = run.result["extractions"].as_i64().unwrap_or(0);
    let fresh = st.store.job_run(&run.id).await.ok().flatten().and_then(|r| r.result["extractions"].as_i64());
    if fresh.unwrap_or(used) + chunks.len() as i64 > budget {
        return Err(Unread::Budget(format!(
            "the run's budget of {budget} extractions is used up; the page is read at the next run"
        )));
    }
    let _ = st.store.job_run_count(&run.id, "extractions", chunks.len() as i64).await;
    let provider = snap.providers.get(&page.doc.provider_id).map(|p| p.doc.name.clone()).unwrap_or_default();
    let known = {
        let mut ids: Vec<&str> = snap
            .models
            .values()
            .filter(|m| m.doc.provider_id == page.doc.provider_id && m.doc.state != ModelState::Retired)
            .map(|m| m.doc.upstream_id.as_str())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.join(", ")
    };
    let mut all = PageExtraction { chunks: chunks.len() as u32, ..Default::default() };
    for (i, chunk) in chunks.iter().enumerate() {
        let answer = call_extraction(st, snap, &run.id, &provider, &known, chunk, (i + 1, chunks.len()))
            .await
            .map_err(Unread::Failed)?;
        let found = models_from_answer(&answer, &page.doc.url);
        all.notes.extend(found.notes);
        for m in found.models {
            match all.models.iter_mut().find(|x| normalise_model_id(&x.model) == normalise_model_id(&m.model)) {
                Some(first) => {
                    first.properties.fill_from(&m.properties);
                    if first.price.is_none() {
                        first.price = m.price;
                    }
                }
                None => all.models.push(m),
            }
        }
    }
    Ok(all)
}

/// Splits a page's text into parts of at most `max` characters, at line ends. A table that is
/// split carries its first row (its header) into the next part, so that its columns keep their
/// names. A line longer than a part is cut.
pub fn chunk_text(text: &str, max: usize) -> Vec<String> {
    if text.chars().count() <= max {
        return vec![text.to_owned()];
    }
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut header: Option<String> = None;
    for line in text.lines() {
        let row = line.contains(" | ");
        if !row {
            header = None;
        }
        let pieces: Vec<String> = if line.chars().count() > max {
            line.chars().collect::<Vec<_>>().chunks(max).map(|c| c.iter().collect()).collect()
        } else {
            vec![line.to_owned()]
        };
        for piece in pieces {
            if !part.is_empty() && part.chars().count() + piece.chars().count() + 1 > max {
                parts.push(std::mem::take(&mut part));
                if let Some(h) = header.as_deref().filter(|h| *h != piece && h.len() + piece.len() + 2 <= max) {
                    part.push_str(h);
                    part.push('\n');
                }
            }
            part.push_str(&piece);
            part.push('\n');
        }
        if row && header.is_none() {
            header = Some(line.to_owned());
        }
    }
    if !part.trim().is_empty() {
        parts.push(part);
    }
    parts
}

/// The JSON schema the extraction model answers in. Strict: every member present, null where the
/// page says nothing, nothing else. Without it a model may answer in a shape of its own (prices
/// keyed by model, seen live); with it every provider with structured output holds to the shape.
fn extraction_schema() -> Value {
    let rate = json!({ "type": ["number", "null"] });
    let object = |properties: Value| {
        let required: Vec<String> = properties.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
    };
    let nullable = |schema: Value| json!({ "anyOf": [schema, { "type": "null" }] });
    let token_rates = json!({
        "input": rate, "output": rate, "cache_read": rate, "cache_write_5m": rate, "cache_write_1h": rate,
    });
    // The variants are fixed properties, not an open map, which structured output may refuse.
    let variant = nullable(object(token_rates.clone()));
    // Hours as the page states them; they are converted to UTC by code, not by the model.
    let hours = nullable(object(json!({
        "weekdays": { "type": "array", "items": { "type": "integer" } },
        "hours": { "type": "array", "items": { "type": "string" } },
        "utc_offset": { "type": "string" }
    })));
    let flag = json!({ "type": ["boolean", "null"] });
    let count = json!({ "type": ["integer", "null"] });
    let words = nullable(json!({ "type": "array", "items": { "type": "string" } }));
    let mut properties = json!({
        "model": { "type": "string" }, "display_name": { "type": ["string", "null"] },
        "api_id_stated": { "type": "boolean" }, "currency": { "type": ["string", "null"] },
        "reasoning": rate, "per_character": rate, "per_request": rate, "per_server_tool_call": rate,
        "input_audio": rate, "output_audio": rate, "input_image": rate, "output_image": rate,
        "per_image": rate, "per_audio_second": rate, "per_search_unit": rate,
        "variants": object(json!({ "batch": variant, "priority": variant, "flex": variant })),
        "tiers": { "type": "array", "items": object(json!({
            "above_input_tokens": { "type": "integer" }, "input": rate, "output": rate, "cache_read": rate
        })) },
        "off_peak": nullable(object(json!({
            "input": rate, "output": rate, "cache_read": rate, "peak": hours, "off_peak": hours
        }))),
        "context_window": count, "max_output_tokens": count,
        "input_modalities": words, "output_modalities": words,
        "vision": flag, "tools": flag, "structured_output": flag, "reasons": flag,
        "evidence": { "type": "string" }
    });
    for (k, v) in token_rates.as_object().into_iter().flatten() {
        properties[k] = v.clone();
    }
    object(json!({ "models": { "type": "array", "items": object(properties) } }))
}

fn extraction_prompt(provider: &str, known: &str, text: &str, (part, parts): (usize, usize)) -> String {
    let part = if parts > 1 { format!(" This is part {part} of {parts} of the page.") } else { String::new() };
    format!(
        "The text below is a page of the provider \"{provider}\" that describes models, what they can do, and \
         their prices.{part} List every model the page describes.\n\
         For \"model\" give the id the provider's API takes for the model, where the page states it, and set \
         api_id_stated to true; otherwise give the model's name as the page writes it and set api_id_stated to \
         false. These are the provider's model ids the gateway knows; where the page describes one of them, give \
         that id exactly: {known}.\n\
         Prices: give token rates (input, output, cache_read, cache_write_5m, cache_write_1h, and reasoning only \
         where reasoning tokens are priced apart from output) in the page's currency per one million tokens, and the \
         currency as a three-letter code. Give per_character for speech per character of input text, per_request per \
         call, and per_server_tool_call per call of a tool the provider runs, such as a web search. Give input_audio, \
         output_audio, input_image and output_image per one million audio or image tokens only where the page prices \
         them apart from text tokens. Give per_image per image made (at the model's standard size and quality), \
         per_audio_second per second of transcribed audio (a price per minute divided by 60), and per_search_unit per \
         search unit of a rerank model. Under \"variants\" give the rates of the batch, priority and flex tiers where \
         the page prices them apart. Under \"tiers\" give the rates that apply instead when a prompt is longer than a \
         threshold (for example above 200,000 input tokens), with the threshold in tokens as above_input_tokens. \
         Under \"off_peak\" give discounted rates that apply at certain hours, and the hours exactly as the page \
         states them: \"peak\" for the hours the normal rates apply or \"off_peak\" for the discounted ones, each \
         with weekdays (1 is Monday, 7 is Sunday), spans \"HH:MM-HH:MM\" in the page's time zone, and that zone's \
         offset from UTC with its sign as utc_offset (\"+08:00\" for Beijing time, \"+00:00\" for UTC). Do not \
         convert the hours. Use an empty list for tiers and null for off_peak when the page has none.\n\
         Properties: context_window is the most tokens the model takes in one request, max_output_tokens the most it \
         writes in one answer; input_modalities and output_modalities use the words text, image, document, audio, \
         video, embedding and ranking; vision is whether it takes images, tools whether it calls functions, \
         structured_output whether its answers can be held to a JSON schema, reasons whether it thinks before it \
         answers.\n\
         Use null for anything the page does not state; do not guess. In \"evidence\" quote the words of the page \
         the prices come from.\n\n\
         --- page ---\n{text}"
    )
}

/// One call of the extraction model, through the gateway's own data plane as the built-in
/// `_system` workspace, so that its cost is accounted like any other.
async fn call_extraction(
    st: &AdminState,
    snap: &std::sync::Arc<Snapshot>,
    run_id: &str,
    provider: &str,
    known: &str,
    text: &str,
    part: (usize, usize),
) -> Result<Value, String> {
    let job = &snap.price_job;
    let workspace = snap
        .workspaces
        .get(SYSTEM_WS_ID)
        .filter(|w| w.enabled)
        .cloned()
        .ok_or("the _system workspace is missing or disabled; restart the management instance")?;
    let mut caller = super::playground::caller_for(snap.clone(), workspace, "price-tracker");
    // The gateway's own call: plugins of the workspace leave it alone.
    caller.internal = Some(crate::dataplane::auth::Internal::Job);
    let mut request = json!({
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": extraction_prompt(provider, known, text, part) }] }],
        "response_format": { "type": "json_schema", "name": "price_page", "schema": extraction_schema(), "strict": true },
        "gateway": { "trace_id": run_id, "tags": { "purpose": "price_fetch" } }
    });
    match (&job.extraction_model, &job.extraction) {
        (Some(name), _) => {
            request["model"] = json!(name);
            // No template sets a ceiling, so the call names its own.
            let cap =
                snap.model_by_name(name).and_then(|m| m.doc.max_output_tokens).unwrap_or(EXTRACTION_OUTPUT_TOKENS);
            request["max_output_tokens"] = json!(cap.min(EXTRACTION_OUTPUT_TOKENS));
        }
        (None, Some(x)) => request["model"] = json!(format!("@{}", x.template.trim_start_matches('@'))),
        (None, None) => return Err("no extraction model is set".into()),
    }
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    let response = crate::dataplane::managed::serve(
        st.gw.clone(),
        caller,
        Instant::now(),
        crate::telemetry::trace::TraceContext::from_traceparent(None),
        headers,
        axum::body::Body::from(request.to_string()),
        crate::dataplane::managed::Interface::Native,
        None,
    )
    .await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 8 << 20).await.map_err(|e| e.to_string())?;
    let answer: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if status != StatusCode::OK {
        let said = answer["error"]["message"].as_str().unwrap_or("the extraction call failed");
        return Err(format!("the extraction model answered {status}: {said}"));
    }
    // The model's answer is the text of its first text block.
    let text = answer["content"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["type"] == "text")
        .and_then(|b| b["text"].as_str())
        .unwrap_or_default();
    json_in(text)
        .map(models_as_list)
        .filter(|v| v["models"].is_array())
        .ok_or_else(|| "the extraction model did not answer with a JSON object holding \"models\"".into())
}

/// `models` as a list: a model that ignores the schema may key them by model instead.
fn models_as_list(mut v: Value) -> Value {
    if let Some(by_model) = v["models"].as_object().cloned() {
        let list: Vec<Value> = by_model
            .into_iter()
            .map(|(model, mut p)| {
                if let Some(o) = p.as_object_mut() {
                    o.entry("model").or_insert(json!(model));
                }
                p
            })
            .collect();
        v["models"] = json!(list);
    }
    v
}

/// The JSON object in a model's answer: the whole text, the inside of a code fence, or the
/// span from the first `{` to the last `}`.
fn json_in(text: &str) -> Option<Value> {
    let parse = |t: &str| serde_json::from_str::<Value>(t.trim()).ok().filter(Value::is_object);
    if let Some(v) = parse(text) {
        return Some(v);
    }
    if let Some(start) = text.find("```") {
        let inner = &text[start + 3..];
        let inner = inner.strip_prefix("json").unwrap_or(inner);
        if let Some(end) = inner.find("```")
            && let Some(v) = parse(&inner[..end])
        {
            return Some(v);
        }
    }
    let (start, end) = (text.find('{')?, text.rfind('}')?);
    (start < end).then(|| parse(&text[start..=end])).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_read_with_or_without_a_fence() {
        assert_eq!(json_in(r#"{"models":[]}"#), Some(json!({ "models": [] })));
        assert_eq!(json_in("Here you go:\n```json\n{\"models\": [1]}\n```\nDone."), Some(json!({ "models": [1] })));
        assert_eq!(json_in("The models are {\"models\": []} as asked."), Some(json!({ "models": [] })));
        assert_eq!(json_in("I could not find any prices."), None);
        assert_eq!(json_in("[1, 2]"), None);
        // Models keyed by id, as a model that ignores the schema answers.
        let keyed = models_as_list(json!({ "models": { "deepseek-flash": { "currency": "USD", "input": 0.3 } } }));
        assert_eq!(keyed["models"][0]["model"], "deepseek-flash");
    }

    #[test]
    fn a_long_page_is_read_in_parts_that_keep_a_tables_header() {
        assert_eq!(chunk_text("short", 100), ["short"]);
        let mut text = String::from("Pricing\nModel | Input | Output\n");
        for i in 0..40 {
            text.push_str(&format!("model-{i:02} | ${i}.00 | ${i}.50\n"));
        }
        text.push_str("Footnotes follow.\n");
        let parts = chunk_text(&text, 200);
        assert!(parts.len() > 3, "{parts:?}");
        assert!(parts.iter().all(|p| p.chars().count() <= 200), "{parts:?}");
        for p in &parts[1..parts.len() - 1] {
            assert!(p.starts_with("Model | Input | Output\n"), "the header comes along: {p}");
        }
        // Every row is in exactly one part.
        let rows: usize = parts.iter().map(|p| p.lines().filter(|l| l.starts_with("model-")).count()).sum();
        assert_eq!(rows, 40);
        // A line longer than a part is cut.
        assert_eq!(chunk_text(&"x".repeat(450), 200).len(), 3);
    }

    #[test]
    fn facts_are_left_out_where_they_cannot_be_true() {
        assert!(plausible("context_window", &json!(200_000), None));
        assert!(!plausible("context_window", &json!(15), None), "a price read as a window");
        assert!(plausible("max_output_tokens", &json!(8192), Some(128_000)));
        assert!(!plausible("max_output_tokens", &json!(256_000), Some(128_000)));
        assert!(plausible("modalities.input", &json!(["text", "image"]), None));
        assert!(!plausible("modalities.input", &json!(["text", "smell"]), None));
        assert!(!plausible("modalities.output", &json!([]), None));
        assert!(plausible("capabilities.vision", &json!(true), None));
    }

    #[test]
    fn a_page_names_a_model_by_id_or_by_name() {
        let model: Model = serde_json::from_value(json!({
            "name": "openai/gpt-5-mini", "provider_id": "prv", "upstream_id": "gpt-5-mini", "display_name": "GPT-5 mini"
        }))
        .unwrap();
        let page = |model: &str, display: Option<&str>| PageModel {
            model: model.into(),
            display_name: display.map(str::to_owned),
            ..Default::default()
        };
        assert!(names(&page("GPT-5-Mini", None), &model));
        assert!(names(&page("GPT-5 mini", None), &model));
        assert!(names(&page("mini", Some("GPT-5 mini")), &model));
        assert!(!names(&page("gpt-5", None), &model));
        assert!(!names(&page("gpt-5-mini-2025", None), &model));
    }
}
