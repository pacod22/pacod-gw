//! The benchmark report (`pacod-bench-report/1`): what pacod-benchmark publishes to the
//! gateway's management API (`POST /api/v1/benchmark-reports`), or a platform admin uploads in
//! the console, and what the console's Benchmarks page shows.
//!
//! The gateway owns this contract: it validates every document it is given, whoever made it,
//! and pacod-benchmark builds its reports from these types (it vendors this crate, as it
//! vendors the load agent's contract). The fixtures in `tests/fixtures/v1/` are read by the
//! tests of both.
//!
//! Compatibility:
//! - Within a major version, fields are only added, and only as optional ones. A document of a
//!   newer minor version is accepted: fields this version does not know are kept in the stored
//!   bytes and left out of the console's view.
//! - A new major version is accepted next to the previous one for at least one release.
//!
//! What a report says about its figures matters as much as the figures: the environment
//! (observed from the gateway, or declared by the operator), where the agents ran, and the
//! measurement class. Figures of agents in the gateway's stack describe behaviour, not capacity;
//! the console says so on every report that carries them.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The schema name of every version 1 document.
pub const SCHEMA: &str = "pacod-bench-report/1";
/// The minor version this crate writes.
pub const MINOR: u32 = 0;
/// A document is at most this long, as sent.
pub const MAX_BYTES: usize = 1 << 20;
pub const MAX_CELLS: usize = 256;
/// A cell's series holds at most this many points.
pub const MAX_POINTS: usize = 600;
/// The highest bucket index of the agents' histograms (pacod-gw-load's `histogram::index`).
pub const MAX_BUCKET: u32 = 3775;
const MAX_TEXT: usize = 2000;
const MAX_NAME: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    /// `pacod-bench-report/1`.
    pub schema: String,
    #[serde(default)]
    pub minor: u32,
    /// The run's id at the benchmark that made it.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub producer: Producer,
    /// Who ran it, as the benchmark knows them. Who published it is the gateway's to record: a
    /// report published and the same report uploaded from its file are one report.
    #[serde(default)]
    pub run_by: Option<Person>,
    pub target: Target,
    /// What the run knew about the deployment it measured.
    #[serde(default)]
    pub environment: BTreeMap<String, Fact>,
    #[serde(default)]
    pub agents: Vec<Agent>,
    pub measurement: Measurement,
    /// The suite as the run ran it; opaque here.
    #[serde(default)]
    pub suite: Value,
    pub cells: Vec<Cell>,
    /// Data of the producer's that the gateway keeps and does not show.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub extensions: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Producer {
    /// `pacod-benchmark`.
    pub name: String,
    pub version: String,
    /// The pacod-gw commit the producer was built against.
    #[serde(default)]
    pub gw_commit: String,
    /// The load agents' contract (pacod-gw-load's `CONTRACT`).
    #[serde(default)]
    pub load_contract: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Person {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// A pacod-gw whose scene the benchmark set up through its management API.
    Managed,
    /// A URL and a key: pacod-gw-lite, another installation, another product.
    External,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Target {
    pub label: String,
    pub kind: TargetKind,
    /// `pacod_gw`, `pacod_gw_lite`, `third_party`.
    pub product: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The gateway said so.
    Observed,
    /// The operator did.
    Declared,
}

/// One thing the run knew about the deployment: the gateway's version, its instances, the cache
/// store, the layout, the hardware.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Fact {
    pub value: Value,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Agent {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub cpus: u32,
    /// `in_stack`, `same_host`, `remote`, `unknown`: as the operator declared it.
    pub placement: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// How the target behaves; not what it can take.
    Behaviour,
    /// What the target can take: every agent on another machine, none short of CPU, and a mock
    /// of its own.
    Capacity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Measurement {
    pub class: Class,
    /// Why the figures are not capacity.
    #[serde(default)]
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    /// Through the target.
    Target,
    /// The same load straight to the mock, without any gateway.
    Baseline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Cell {
    pub index: u32,
    #[serde(default)]
    pub rep: u32,
    pub kind: CellKind,
    /// `done`, `skipped`, `error`, `cancelled`.
    pub status: String,
    /// What the cell varies, in a few words.
    pub label: String,
    #[serde(default)]
    pub axes: BTreeMap<String, Value>,
    /// Cells with the same key ran the same configuration, in any run: only they are compared.
    pub comparison_key: String,
    /// The comparison key of the baseline cell this cell is measured against.
    #[serde(default)]
    pub baseline_key: String,
    /// The configuration as the producer ran it; opaque here.
    #[serde(default)]
    pub config: Value,
    /// The steady window: from the end of the warm-up to the end of the last stage.
    #[serde(default)]
    pub steady: Option<Summary>,
    /// The whole cell, ramp and drain included.
    #[serde(default)]
    pub whole: Option<Summary>,
    #[serde(default)]
    pub histograms: Option<Histograms>,
    #[serde(default)]
    pub series: Option<Series>,
    #[serde(default)]
    pub flags: Vec<Flag>,
    #[serde(default)]
    pub error: String,
}

/// Milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Pct {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Summary {
    pub duration_s: f64,
    pub calls: u64,
    pub ok: u64,
    #[serde(default)]
    pub failed: u64,
    #[serde(default)]
    pub skipped: u64,
    pub rps_mean: f64,
    #[serde(default)]
    pub rps_peak: f64,
    /// Answers by status code.
    #[serde(default)]
    pub status: BTreeMap<String, u64>,
    pub ttfb: Pct,
    pub total: Pct,
    /// The gateway's own figure (`x-gw-delay-ms`); absent where no answer carried it: another
    /// product, or a baseline. Absent is never shown as 0.
    #[serde(default)]
    pub gw_delay: Option<Pct>,
}

/// Calls counted by bucket, in the agents' wire format: integer log-linear buckets over
/// microseconds, sparse (`b` is `[[index, count], …]`). Histograms add up across cells and runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Histogram {
    pub n: u64,
    pub min: u64,
    pub max: u64,
    pub sum: u64,
    pub b: Vec<(u32, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Histograms {
    pub ttfb: Histogram,
    pub total: Histogram,
    #[serde(default)]
    pub gw_delay: Option<Histogram>,
}

/// A cell's seconds as columns: one entry each `step_s` seconds, `null` where nothing was
/// measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Series {
    pub step_s: u32,
    pub t: Vec<u64>,
    #[serde(default)]
    pub ok: Vec<Option<f64>>,
    #[serde(default)]
    pub failed: Vec<Option<f64>>,
    #[serde(default)]
    pub in_flight: Vec<Option<f64>>,
    #[serde(default)]
    pub total_p50: Vec<Option<f64>>,
    #[serde(default)]
    pub total_p99: Vec<Option<f64>>,
    #[serde(default)]
    pub ttfb_p50: Vec<Option<f64>>,
    #[serde(default)]
    pub gw_delay_p99: Vec<Option<f64>>,
}

/// Something a reader of a cell's figures should know. `code` is from an open set; a reader
/// that does not know one shows the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Flag {
    pub code: String,
    pub message: String,
}

/// Why a document is refused: where, and what is wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Problem {
    pub path: String,
    pub message: String,
}

fn problem(path: impl Into<String>, message: impl Into<String>) -> Problem {
    Problem { path: path.into(), message: message.into() }
}

/// A document as sent: read and checked. The bytes are what the gateway keeps.
pub fn parse(bytes: &[u8]) -> Result<Report, Vec<Problem>> {
    if bytes.len() > MAX_BYTES {
        return Err(vec![problem("", format!("the report is longer than {MAX_BYTES} bytes"))]);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|e| vec![problem("", format!("not JSON: {e}"))])?;
    let schema = value.get("schema").and_then(Value::as_str).unwrap_or("");
    if schema != SCHEMA {
        return Err(vec![problem("schema", format!("{schema:?} is not {SCHEMA}"))]);
    }
    let report: Report =
        serde_json::from_value(value).map_err(|e| vec![problem("", format!("does not read as a report: {e}"))])?;
    validate(&report)?;
    Ok(report)
}

fn text(problems: &mut Vec<Problem>, path: &str, s: &str, max: usize, required: bool) {
    if required && s.trim().is_empty() {
        problems.push(problem(path, "is empty"));
    }
    if s.chars().count() > max {
        problems.push(problem(path, format!("is longer than {max} characters")));
    }
    if s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        problems.push(problem(path, "holds control characters"));
    }
}

fn finite(problems: &mut Vec<Problem>, path: &str, v: f64) {
    if !v.is_finite() || v < 0.0 {
        problems.push(problem(path, "is not a finite number of at least 0"));
    }
}

fn pct(problems: &mut Vec<Problem>, path: &str, p: &Pct) {
    for (name, v) in [("p50", p.p50), ("p95", p.p95), ("p99", p.p99), ("max", p.max)] {
        finite(problems, &format!("{path}.{name}"), v);
    }
}

fn summary(problems: &mut Vec<Problem>, path: &str, s: &Summary) {
    finite(problems, &format!("{path}.duration_s"), s.duration_s);
    finite(problems, &format!("{path}.rps_mean"), s.rps_mean);
    finite(problems, &format!("{path}.rps_peak"), s.rps_peak);
    if s.ok > s.calls {
        problems.push(problem(format!("{path}.ok"), "is more than calls"));
    }
    pct(problems, &format!("{path}.ttfb"), &s.ttfb);
    pct(problems, &format!("{path}.total"), &s.total);
    if let Some(g) = &s.gw_delay {
        pct(problems, &format!("{path}.gw_delay"), g);
    }
}

fn histogram(problems: &mut Vec<Problem>, path: &str, h: &Histogram) {
    let mut count = 0u64;
    let mut last = None;
    for (i, c) in &h.b {
        if *i > MAX_BUCKET {
            problems.push(problem(path, format!("bucket {i} is above {MAX_BUCKET}")));
        }
        if last.is_some_and(|l| *i <= l) {
            problems.push(problem(path, "buckets are not in index order"));
        }
        last = Some(*i);
        count = count.saturating_add(*c);
    }
    if count != h.n {
        problems.push(problem(path, format!("the buckets count {count} calls, n says {}", h.n)));
    }
    if h.n > 0 && h.min > h.max {
        problems.push(problem(path, "min is above max"));
    }
}

fn key(problems: &mut Vec<Problem>, path: &str, k: &str, required: bool) {
    let ok = k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    if (required || !k.is_empty()) && !ok {
        problems.push(problem(path, "is not 64 lower-case hex characters"));
    }
}

/// Every check a report must pass, all at once: a refusal names every problem.
pub fn validate(r: &Report) -> Result<(), Vec<Problem>> {
    let mut p = Vec::new();
    if r.schema != SCHEMA {
        p.push(problem("schema", format!("is not {SCHEMA}")));
    }
    text(&mut p, "id", &r.id, MAX_NAME, true);
    text(&mut p, "name", &r.name, MAX_NAME, true);
    text(&mut p, "description", &r.description, MAX_TEXT, false);
    text(&mut p, "producer.name", &r.producer.name, MAX_NAME, true);
    text(&mut p, "producer.version", &r.producer.version, MAX_NAME, false);
    text(&mut p, "target.label", &r.target.label, MAX_NAME, true);
    text(&mut p, "target.product", &r.target.product, MAX_NAME, true);
    if r.environment.len() > 50 {
        p.push(problem("environment", "holds more than 50 facts"));
    }
    for (k, f) in &r.environment {
        text(&mut p, &format!("environment.{k}"), k, 60, true);
        if serde_json::to_string(&f.value).map_or(true, |s| s.len() > MAX_TEXT) {
            p.push(problem(format!("environment.{k}"), "is too long"));
        }
    }
    if r.agents.len() > 50 {
        p.push(problem("agents", "holds more than 50 agents"));
    }
    for (i, a) in r.agents.iter().enumerate() {
        text(&mut p, &format!("agents[{i}].name"), &a.name, 60, true);
        text(&mut p, &format!("agents[{i}].placement"), &a.placement, 20, true);
    }
    for (i, reason) in r.measurement.reasons.iter().enumerate() {
        text(&mut p, &format!("measurement.reasons[{i}]"), reason, MAX_TEXT, true);
    }
    if r.cells.is_empty() {
        p.push(problem("cells", "is empty"));
    }
    if r.cells.len() > MAX_CELLS {
        p.push(problem("cells", format!("holds more than {MAX_CELLS} cells")));
    }
    for (i, c) in r.cells.iter().enumerate().take(MAX_CELLS) {
        let at = format!("cells[{i}]");
        text(&mut p, &format!("{at}.label"), &c.label, MAX_TEXT, true);
        text(&mut p, &format!("{at}.status"), &c.status, 20, true);
        text(&mut p, &format!("{at}.error"), &c.error, MAX_TEXT, false);
        key(&mut p, &format!("{at}.comparison_key"), &c.comparison_key, true);
        key(&mut p, &format!("{at}.baseline_key"), &c.baseline_key, false);
        if let Some(s) = &c.steady {
            summary(&mut p, &format!("{at}.steady"), s);
        }
        if let Some(s) = &c.whole {
            summary(&mut p, &format!("{at}.whole"), s);
        }
        if let Some(h) = &c.histograms {
            histogram(&mut p, &format!("{at}.histograms.ttfb"), &h.ttfb);
            histogram(&mut p, &format!("{at}.histograms.total"), &h.total);
            if let Some(g) = &h.gw_delay {
                histogram(&mut p, &format!("{at}.histograms.gw_delay"), g);
            }
        }
        if let Some(s) = &c.series {
            let n = s.t.len();
            if n > MAX_POINTS {
                p.push(problem(format!("{at}.series"), format!("holds more than {MAX_POINTS} points")));
            }
            if s.step_s == 0 {
                p.push(problem(format!("{at}.series.step_s"), "is 0"));
            }
            for (name, col) in [
                ("ok", &s.ok),
                ("failed", &s.failed),
                ("in_flight", &s.in_flight),
                ("total_p50", &s.total_p50),
                ("total_p99", &s.total_p99),
                ("ttfb_p50", &s.ttfb_p50),
                ("gw_delay_p99", &s.gw_delay_p99),
            ] {
                if !col.is_empty() && col.len() != n {
                    p.push(problem(format!("{at}.series.{name}"), "is not as long as t"));
                }
                if col.iter().flatten().any(|v| !v.is_finite() || *v < 0.0) {
                    p.push(problem(format!("{at}.series.{name}"), "holds a number that is not finite"));
                }
            }
        }
        if c.flags.len() > 50 {
            p.push(problem(format!("{at}.flags"), "holds more than 50 flags"));
        }
        for (j, f) in c.flags.iter().enumerate() {
            text(&mut p, &format!("{at}.flags[{j}].code"), &f.code, 60, true);
            text(&mut p, &format!("{at}.flags[{j}].message"), &f.message, MAX_TEXT, true);
        }
    }
    if p.is_empty() { Ok(()) } else { Err(p) }
}

impl Report {
    /// The figure of a fact, as the console's list shows it.
    pub fn fact(&self, name: &str) -> Option<String> {
        self.environment.get(name).map(|f| match &f.value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    }
}
