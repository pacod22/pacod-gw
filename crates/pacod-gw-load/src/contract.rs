//! What the tester and a load agent say to each other (`/v1/…` on the agent).
//!
//! The tester is Python, so this is a JSON contract rather than shared code: the fixtures in
//! `tests/contract/` are read by the tests of both sides, and a change to a type here that
//! does not change them fails one side or the other. [`CONTRACT`] is bumped when an older
//! tester could misread a newer agent.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::histogram::Histogram;

/// The contract's version, in `GET /v1/info` and in every prepare request.
pub const CONTRACT: u32 = 1;

/// `GET /v1/info`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub contract: u32,
    pub name: String,
    pub version: String,
    /// The gateway this agent calls when a plan names no `base_url`.
    pub target: String,
    pub caps: Caps,
    /// CPUs the agent may use: its CPU figure is in percent of one, so `cpus * 100` is full.
    pub cpus: u32,
    pub busy: Option<Busy>,
}

/// The most an agent accepts, whatever the plan says (`LOAD_AGENT_MAX_*`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Caps {
    pub max_concurrency: u32,
    /// Calls a second, in rate mode.
    pub max_rate: f64,
    pub max_duration_s: u64,
    pub max_body_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Busy {
    pub run_id: String,
    pub state: RunState,
}

/// `POST /v1/runs`: one agent's part of a run.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Prepare {
    pub contract: u32,
    pub run_id: String,
    /// This agent's part, from 0, and how many agents share the run.
    pub part: u32,
    pub parts: u32,
    /// The key the calls carry. Held in memory for the run, never logged or echoed.
    pub key: String,
    /// The header the key goes in.
    pub auth_header: AuthHeader,
    /// Another gateway than the agent's own target, for this run only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub request: RequestSpec,
    pub profile: Profile,
    /// Seconds without a report after which the agent stops the run on its own, so that a
    /// tester that went away does not leave load running.
    pub lease_s: u64,
}

// Written by hand so that the key never reaches a log line through `{:?}`.
impl fmt::Debug for Prepare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Prepare")
            .field("run_id", &self.run_id)
            .field("part", &self.part)
            .field("parts", &self.parts)
            .field("key", &"[redacted]")
            .field("auth_header", &self.auth_header)
            .field("base_url", &self.base_url)
            .field("request", &self.request)
            .field("profile", &self.profile)
            .field("lease_s", &self.lease_s)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthHeader {
    /// `Authorization: Bearer <key>`.
    Authorization,
    XApiKey,
    XGoogApiKey,
}

impl AuthHeader {
    pub fn name(self) -> &'static str {
        match self {
            AuthHeader::Authorization => "authorization",
            AuthHeader::XApiKey => "x-api-key",
            AuthHeader::XGoogApiKey => "x-goog-api-key",
        }
    }

    pub fn value(self, key: &str) -> String {
        match self {
            AuthHeader::Authorization => format!("Bearer {key}"),
            AuthHeader::XApiKey | AuthHeader::XGoogApiKey => key.to_owned(),
        }
    }
}

/// The one request every call of the run sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestSpec {
    pub method: String,
    /// Below the base URL, with the query: `/v1/chat/completions`.
    pub path: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
    /// Whether the response is a stream; only reported back, the engine reads every response
    /// to its end either way.
    #[serde(default)]
    pub stream: bool,
}

/// How much load, and for how long.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub mode: Mode,
    /// Each stage moves the target in a straight line from where the previous one ended (0
    /// at the start) to its own target. A stage of 0 seconds jumps.
    pub stages: Vec<Stage>,
    /// Rate mode: calls started but not finished, above which an arrival is skipped (and
    /// counted) instead of started. Concurrency mode ignores it.
    #[serde(default)]
    pub max_in_flight: Option<u32>,
    pub timeout_s: u64,
    /// Calls this agent makes at most; the run ends when they are used up.
    #[serde(default)]
    pub max_calls: Option<u64>,
    /// After the last stage, how long calls still running may take to finish.
    #[serde(default = "default_drain")]
    pub drain_s: u64,
    #[serde(default)]
    pub http: HttpVersion,
    /// Connections opened before the start, so that the first second does not measure
    /// connection set-up.
    #[serde(default)]
    pub warmup_connections: u32,
}

fn default_drain() -> u64 {
    30
}

impl Profile {
    pub fn duration_s(&self) -> u64 {
        self.stages.iter().map(|s| s.duration_s).sum()
    }

    pub fn peak(&self) -> f64 {
        self.stages.iter().map(|s| s.target).fold(0.0, f64::max)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// A closed loop: `target` calls at a time, each started when the one before it ends.
    Concurrency,
    /// An open loop: `target` calls a second, started on time whether or not earlier ones
    /// have finished.
    Rate,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    pub duration_s: u64,
    pub target: f64,
}

/// HTTP/1.1 by default: over TLS the client would otherwise negotiate HTTP/2 and carry
/// thousands of calls over a handful of connections, which is not what most clients do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HttpVersion {
    #[default]
    #[serde(rename = "1.1")]
    Http1,
    #[serde(rename = "2")]
    Http2,
}

/// The answer to a prepare: the run is ready to start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prepared {
    pub run_id: String,
    pub state: RunState,
    pub probe: Probe,
}

/// One call made during prepare, before any load: it shows a wrong key, model or URL at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub status: Option<u16>,
    pub ttfb_ms: f64,
    pub total_ms: f64,
    pub error: Option<String>,
    /// The start of a response that was not a success, for the reason it gives.
    pub body_excerpt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Prepared,
    Running,
    /// No new calls; the last ones are finishing.
    Draining,
    Done,
    Cancelled,
    Failed,
}

impl RunState {
    pub fn finished(self) -> bool {
        matches!(self, RunState::Done | RunState::Cancelled | RunState::Failed)
    }
}

/// Why a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    /// The last stage ended.
    Duration,
    /// `max_calls` were made.
    MaxCalls,
    Cancelled,
    /// No report was asked for within the lease.
    LeaseExpired,
    Error,
}

/// `GET /v1/runs/{id}/report?after=<t>`: the seconds after `t`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub run_id: String,
    pub state: RunState,
    pub ended: Option<Ended>,
    #[serde(default)]
    pub error: String,
    pub elapsed_ms: u64,
    pub in_flight: u64,
    /// Calls finished so far, whatever their outcome.
    pub calls: u64,
    pub intervals: Vec<Interval>,
}

/// One second of one agent's run.
///
/// Every call that finished in the second is counted once: `ok` (a success, read to its
/// end), or with its status in `status` (an answer that was not a success), or in `errors`
/// (no answer, or an answer that broke off). `status` counts every answer, successes too.
/// The latencies are of the successes only: a quick 429 says nothing about how fast the
/// gateway serves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Interval {
    /// Seconds since this agent's start; the interval covers `[t, t + 1)`.
    pub t: u64,
    /// The target (calls at a time, or calls a second) at the end of the second.
    pub target: f64,
    pub started: u64,
    pub completed: u64,
    pub ok: u64,
    pub errors: Errors,
    pub status: BTreeMap<u16, u64>,
    /// Response body bytes.
    pub bytes: u64,
    pub in_flight: u64,
    /// Rate mode: arrivals not started because `max_in_flight` calls were running.
    pub skipped: u64,
    pub lag_ms: Lag,
    /// The agent's CPU use in the second, in percent of one CPU.
    pub cpu_pct: Option<f64>,
    pub ttfb: Histogram,
    pub total: Histogram,
    /// The gateway's own figure, `x-gw-delay-ms`, where the response had one.
    pub gw_delay: Histogram,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Errors {
    pub connect: u64,
    pub timeout: u64,
    /// The answer broke off.
    pub body: u64,
    pub other: u64,
    /// Calls still running when the run was stopped.
    pub cancelled: u64,
}

impl Errors {
    /// Calls that failed without a complete answer, not counting the cancelled ones.
    pub fn failed(&self) -> u64 {
        self.connect + self.timeout + self.body + self.other
    }
}

/// How late the rate-mode scheduler started calls: a high figure means the agent, not the
/// gateway, is the limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Lag {
    pub max: f64,
    pub mean: f64,
}

/// The body of every error the agent answers with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// `unauthorized`, `busy`, `invalid`, `not_found`, `failed`.
    pub code: String,
    pub message: String,
}
