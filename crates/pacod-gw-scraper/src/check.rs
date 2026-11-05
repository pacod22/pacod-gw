//! A run of the daily check: each page is fetched (a feed) or rendered (a page), fingerprinted,
//! and reported to the core, with its text when it changed or is to be read anyway. The core
//! has the text read and applies what the pages say; this side never sees a provider key.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use pacod_gw_events::extraction::PageReading;
use pacod_gw_events::jobs::{CheckWork, PageTask};
use serde_json::{Value, json};

use crate::runner::Runner;
use crate::text::{fingerprint, sha256};

/// The largest feed a check reads.
const MAX_FEED_BYTES: usize = 16 << 20;
/// The most text of a page sent to the core: more than any pricing page holds.
const MAX_TEXT_CHARS: usize = 2_000_000;

/// Checks every page of the run, in the order the core gave them (by provider and priority), as
/// long as the run is this scraper's (`keep`). Returns a summary for the run's record.
pub async fn run(runner: &Runner, run_id: &str, work: &CheckWork, keep: &AtomicBool) -> anyhow::Result<Value> {
    let mut outcomes: BTreeMap<String, u64> = BTreeMap::new();
    let started = Instant::now();
    for task in &work.pages {
        if !keep.load(Ordering::Relaxed) {
            anyhow::bail!("the core gave the run to another scraper");
        }
        let reading = read(runner, task).await;
        if reading.outcome == "failed" {
            tracing::warn!(url = %task.url, message = %reading.message, "a price page could not be fetched");
        }
        match runner.core.reading(run_id, &reading).await {
            Ok(reply) => {
                let outcome = reply["outcome"].as_str().unwrap_or("unknown").to_owned();
                tracing::info!(url = %task.url, outcome = %outcome, "price page checked");
                *outcomes.entry(outcome).or_default() += 1;
            }
            Err(e) => {
                tracing::warn!(url = %task.url, error = %format!("{e:#}"), "reporting a price page to the core failed");
                *outcomes.entry("not_reported".into()).or_default() += 1;
            }
        }
    }
    Ok(json!({ "scraper": { "outcomes": outcomes, "elapsed_ms": started.elapsed().as_millis() as u64 } }))
}

/// What one page is now. A failure is a reading too: the core keeps count of them.
pub async fn read(runner: &Runner, task: &PageTask) -> PageReading {
    let started = Instant::now();
    let mut reading = PageReading { page_id: task.page_id.clone(), ..Default::default() };
    let outcome = if task.kind == "feed" {
        read_feed(runner, task, &mut reading).await
    } else {
        read_page(runner, task, &mut reading).await
    };
    if let Err(e) = outcome {
        reading.outcome = "failed".into();
        reading.message = format!("{e:#}");
        reading.text = None;
        reading.extraction = None;
    }
    reading.elapsed_ms = started.elapsed().as_millis() as u64;
    reading
}

async fn read_feed(runner: &Runner, task: &PageTask, reading: &mut PageReading) -> anyhow::Result<()> {
    let fetched = runner.fetch.get(&task.url, MAX_FEED_BYTES).await?;
    reading.http_status = Some(fetched.status);
    reading.final_url = fetched.final_url;
    reading.bytes = fetched.body.len() as u64;
    if fetched.status >= 400 {
        anyhow::bail!("the feed answered {}", fetched.status);
    }
    let found = crate::feeds::read(task, &fetched.body)?;
    // Over what the feed says of the provider's models, not the whole list: LiteLLM's changes
    // daily for models of other providers.
    let said = serde_json::to_string(&found)?;
    reading.content_hash = sha256(&said);
    reading.text_hash = sha256(&String::from_utf8_lossy(&fetched.body));
    reading.outcome = "changed".into();
    reading.extraction = Some(found);
    Ok(())
}

async fn read_page(runner: &Runner, task: &PageTask, reading: &mut PageReading) -> anyhow::Result<()> {
    let page = runner.render.render(&task.url).await?;
    reading.http_status = page.status;
    reading.final_url = page.final_url;
    reading.title = page.title;
    reading.bytes = page.bytes;
    if let Some(status) = page.status.filter(|s| *s >= 400) {
        anyhow::bail!("the page answered {status}");
    }
    if page.text.trim().is_empty() {
        anyhow::bail!("the page rendered without text");
    }
    reading.content_hash = fingerprint(&page.text, &task.known_models);
    reading.text_hash = sha256(&page.text);
    if !task.force && reading.content_hash == task.previous_hash {
        reading.outcome = "unchanged".into();
    } else {
        reading.outcome = "changed".into();
        reading.text = Some(page.text.chars().take(MAX_TEXT_CHARS).collect());
    }
    Ok(())
}
