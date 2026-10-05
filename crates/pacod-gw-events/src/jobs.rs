//! Messages of the internal job API the analytics service uses on the core
//! (`/api/v1/internal/jobs/...`). The core hands out a lease so that only one analytics
//! instance runs a job, and it is the only component that writes configuration.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const JOB_PRICES: &str = "prices";
pub const JOB_DISCOVERY: &str = "discovery";
pub const JOB_RETENTION: &str = "retention";
pub const JOB_BATCH_RESULTS: &str = "batch_results";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRequest {
    pub job: String,
    pub holder: String,
    /// How long the lease is held, in seconds, unless renewed.
    pub ttl_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseReply {
    pub granted: bool,
    /// The run to report results against. Present when the lease was granted and the job is due.
    pub run_id: Option<String>,
    /// True when the job should run now (schedule reached or "run now" requested).
    pub due: bool,
    /// Job-specific configuration and work list.
    #[serde(default)]
    pub work: serde_json::Value,
}

/// A price the tracker found for one model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceCandidate {
    pub model_id: String,
    pub currency: String,
    /// Per one million tokens.
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write_5m: Option<f64>,
    pub cache_write_1h: Option<f64>,
    /// `feed`, `extracted`, or `import`.
    pub source: String,
    pub evidence_url: String,
    /// The text the figures were taken from.
    pub evidence_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    pub run_id: String,
    pub job: String,
    pub finished_at: DateTime<Utc>,
    pub ok: bool,
    #[serde(default)]
    pub message: String,
    /// Job-specific result, for the price job a list of `PriceCandidate` plus the models
    /// for which nothing was found.
    #[serde(default)]
    pub result: serde_json::Value,
}
