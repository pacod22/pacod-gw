//! Messages of the internal job API the analytics service and the scraper use on the core
//! (`/api/v1/internal/jobs/...`). The core hands out a lease so that only one instance runs a
//! job, and it is the only component that writes configuration.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The price tracker of earlier releases, run by the analytics service.
pub const JOB_PRICES: &str = "prices";
/// Reads the price pages the administrators confirmed, daily by default (the scraper).
pub const JOB_PRICE_CHECK: &str = "price_check";
/// Looks through the providers' sites for pages that list models and prices, weekly by
/// default (the scraper).
pub const JOB_PRICE_SCAN: &str = "price_scan";
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
    /// What the runner says about itself. The scraper sends it, so that the console can show
    /// whether it runs and reaches its browser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentStatus>,
}

/// A job runner's account of itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentStatus {
    /// The service, for example `gw-scraper`.
    pub service: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserStatus>,
}

/// Whether the scraper reaches the browser it renders pages in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserStatus {
    pub ok: bool,
    /// The browser's product and version, for example `Chrome/157.0.8103.27`.
    pub product: String,
    /// Why it is not reached.
    pub message: String,
}

/// Keeps the lease of a run while the run lasts: a long run outlasts any fixed lease.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatRequest {
    pub holder: String,
    pub ttl_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatReply {
    /// False when the run is no longer this holder's, or is over: stop working on it.
    pub keep_going: bool,
}

/// The work of a run of the daily check: the pages to look at.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckWork {
    pub pages: Vec<PageTask>,
    /// Whether the core has a model to read pages with. Without one, pages are fetched and
    /// fingerprinted, and their readings wait.
    pub extraction: bool,
}

/// One page of a check run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageTask {
    pub page_id: String,
    pub provider_id: String,
    /// The provider's name.
    pub provider: String,
    pub url: String,
    /// `page` (rendered in the browser and read by a model) or `feed` (a price list).
    pub kind: String,
    /// For feeds: `pacod` or `litellm`.
    pub format: String,
    pub key_prefix: Option<String>,
    /// The fingerprint of the page at its last reading; empty before the first.
    pub previous_hash: String,
    /// Read the page even when its fingerprint did not change: an administrator asked, or a
    /// model of the provider waits for its price.
    pub force: bool,
    /// The provider's ids of the repository's models at this provider.
    pub known_models: Vec<String>,
}

/// The work of a run of the weekly scan: where to look for pages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanWork {
    pub providers: Vec<ScanProvider>,
    /// How many candidates per provider are opened in the browser and scored.
    pub max_pages_per_provider: u32,
    /// The score a page needs to be reported.
    pub min_score: f64,
    /// Words in an address or title that make a page worth opening.
    pub keywords: Vec<String>,
}

/// One provider of a scan run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanProvider {
    pub provider_id: String,
    pub name: String,
    /// Where the scan starts: the provider's active pages, and the pages its preset names.
    pub roots: Vec<ScanRoot>,
    /// Every address the provider has a page at, whatever its status: not reported again.
    pub known_urls: Vec<String>,
    /// The provider's ids of the repository's models at this provider.
    pub known_models: Vec<String>,
}

/// What the scan found for one provider.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanReport {
    pub provider_id: String,
    /// Pages that list models and prices, best first.
    pub found: Vec<FoundPage>,
    /// What became of each place the scan started from.
    pub roots: Vec<RootStatus>,
    /// How many addresses the scan considered, and how many it opened in the browser.
    pub considered: u32,
    pub rendered: u32,
    /// What went wrong for the provider as a whole, in a sentence.
    pub message: String,
}

/// A page the scan judged to list models and prices.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FoundPage {
    pub url: String,
    /// Where the page ended up after redirects.
    pub final_url: String,
    pub title: String,
    pub score: f64,
    /// How many prices per million tokens (or per unit) the page shows.
    pub price_hits: u32,
    /// The repository's model ids the page names.
    pub model_hits: Vec<String>,
    /// A few of the lines that made the score, for the administrator who confirms the page.
    pub sample: Vec<String>,
}

/// What became of a place the scan started from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RootStatus {
    pub url: String,
    pub page_id: Option<String>,
    pub http_status: Option<u16>,
    pub final_url: String,
    /// `ok`, `gone` (404 or 410), `moved` (it redirects to another page) or `failed`.
    pub outcome: String,
    pub score: Option<f64>,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanRoot {
    pub url: String,
    /// The page at this address, when the provider has one.
    pub page_id: Option<String>,
    /// A page the provider's preset names that the provider has no page at yet.
    pub seed: bool,
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
    /// When the source last confirmed the figures, where it says so (the aggregator's
    /// catalogue). An unchanged price is confirmed as of then, not as of the fetch, so that a
    /// source that stopped looking does not keep it fresh. Absent from older messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
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
