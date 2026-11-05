//! A run of the weekly scan. It needs the browser, which comes in the next stage; until then a
//! scan fails with that reason, so that the console says so.

use std::sync::atomic::AtomicBool;

use pacod_gw_events::jobs::ScanWork;
use serde_json::Value;

use crate::runner::Runner;

pub async fn run(_runner: &Runner, _run_id: &str, _work: &ScanWork, _keep: &AtomicBool) -> anyhow::Result<Value> {
    anyhow::bail!("this scraper cannot scan yet")
}
