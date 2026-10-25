//! Messages of the internal job API the analytics service uses on the core
//! (`/api/v1/internal/jobs/...`). The core hands out a lease so that only one analytics
//! instance runs a job, and it is the only component that writes configuration.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const JOB_PRICES: &str = "prices";
pub const JOB_DISCOVERY: &str = "discovery";
pub const JOB_RETENTION: &str = "retention";
/// Rebuilds the rollups every few minutes. Leased like the others, without a run record.
pub const JOB_ROLLUPS: &str = "rollups";
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

/// The rates of a price the tracker found. Token rates are per one million tokens, the others
/// per character, request or tool call. Every field is optional, so that a sender and a
/// receiver of different releases understand each other.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CandidateRates {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write_5m: Option<f64>,
    pub cache_write_1h: Option<f64>,
    /// Only where reasoning tokens are priced apart from output tokens.
    pub reasoning: Option<f64>,
    /// Speech: per character of input text.
    pub per_character: Option<f64>,
    pub per_request: Option<f64>,
    /// Per provider-executed tool call, for example a web search.
    pub per_server_tool_call: Option<f64>,
    /// Audio and image tokens, where they are priced apart from text tokens.
    pub input_audio: Option<f64>,
    pub output_audio: Option<f64>,
    pub input_image: Option<f64>,
    pub output_image: Option<f64>,
    /// Image models: per image made.
    pub per_image: Option<f64>,
    /// Transcription: per second of audio.
    pub per_audio_second: Option<f64>,
    /// Rerank: per search unit.
    pub per_search_unit: Option<f64>,
}

impl CandidateRates {
    /// Every rate with its name, in a fixed order.
    pub fn named(&self) -> [(&'static str, Option<f64>); 16] {
        [
            ("input", self.input),
            ("output", self.output),
            ("cache_read", self.cache_read),
            ("cache_write_5m", self.cache_write_5m),
            ("cache_write_1h", self.cache_write_1h),
            ("reasoning", self.reasoning),
            ("per_character", self.per_character),
            ("per_request", self.per_request),
            ("per_server_tool_call", self.per_server_tool_call),
            ("input_audio", self.input_audio),
            ("output_audio", self.output_audio),
            ("input_image", self.input_image),
            ("output_image", self.output_image),
            ("per_image", self.per_image),
            ("per_audio_second", self.per_audio_second),
            ("per_search_unit", self.per_search_unit),
        ]
    }

    pub fn is_empty(&self) -> bool {
        self.named().iter().all(|(_, v)| v.is_none())
    }
}

/// Rates for prompts larger than a threshold (long-context prices), as a source gives them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CandidateTier {
    pub above_input_tokens: u64,
    #[serde(flatten)]
    pub rates: CandidateRates,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, CandidateRates>,
}

/// Hours in which a variant applies by the clock (off-peak prices), as a source gives them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CandidateWindow {
    pub variant: String,
    /// ISO weekdays: 1 is Monday, 7 is Sunday.
    pub weekdays: Vec<u8>,
    /// `HH:MM-HH:MM` in UTC.
    pub hours_utc: Vec<String>,
}

/// A price the tracker found for one model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceCandidate {
    pub model_id: String,
    pub currency: String,
    #[serde(flatten)]
    pub rates: CandidateRates,
    /// Rates for variants the provider bills apart, by the name its usage reports (the
    /// service tier): `batch`, `priority`, `flex`, …
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, CandidateRates>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<CandidateTier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<CandidateWindow>,
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
