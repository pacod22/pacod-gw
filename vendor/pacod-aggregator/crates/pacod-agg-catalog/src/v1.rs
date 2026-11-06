//! Version 1 of the catalogue. Rates have the names and units of pacod-gw's prices: token rates
//! per million tokens, the others per unit (character, request, tool call, image, second of
//! audio, search unit), in the price's currency.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The value of `schema` in a catalogue of version 1.
pub const SCHEMA_V1: &str = "pacod.catalog/v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    /// `pacod.catalog/v1`: the major version, which a reader checks before anything else.
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub source: Source,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vendors: Vec<Vendor>,
    #[serde(default)]
    pub models: Vec<Model>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            schema: SCHEMA_V1.to_owned(),
            generated_at: None,
            source: Source::default(),
            vendors: Vec::new(),
            models: Vec::new(),
        }
    }
}

/// Where a catalogue came from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub kind: SourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The version of the program that wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    PacodAggregator,
    Openrouter,
    Litellm,
    /// The price feed of earlier pacod-gw releases, which pacod-broker also serves.
    PacodFeed,
    /// A file a person wrote or kept.
    #[default]
    File,
    /// A kind this reader does not know.
    #[serde(other)]
    Other,
}

/// A vendor: the company whose API serves the models (OpenAI, Anthropic, Google, …).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Vendor {
    /// The vendor's id ([`crate::vendors`]), the same in every source.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The currency its prices are in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Whether the catalogue lists all of the vendor's current models, so that a model missing
    /// from it is no longer offered. Only the aggregator says so, of a vendor whose pages it reads.
    #[serde(default)]
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
}

/// One model of one vendor.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// The catalogue's own id (`agm_…` at the aggregator). Optional in a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The vendor's id. A file may name a `provider` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// For a gateway reading a file: the provider that serves the model, by the gateway's own
    /// name for it, where the vendor alone does not say (a custom endpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// For a gateway reading a file: the name clients are to call the model by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The id the vendor's API takes (`gpt-5-mini`, `claude-sonnet-5-5`).
    pub api_id: String,
    /// Other ids the same model goes by: a dated snapshot, a spelling another source uses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Whether the vendor itself states `api_id`, on a page or in its API, rather than a source
    /// deriving it from a name.
    #[serde(default)]
    pub api_id_confirmed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ModelKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ModelStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated_at: Option<DateTime<Utc>>,
    /// When the vendor stops serving it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sunset_at: Option<DateTime<Utc>>,
    /// The `api_id` of the model that replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seen_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Properties::is_empty")]
    pub properties: Properties,
    /// Request parameters by name (`temperature`, `top_p`, `tool_choice`, …).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, Parameter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    /// Where a field's value came from, by the field's name (`context_window`, `price`, …).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provenance: BTreeMap<String, Provenance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Chat,
    Embedding,
    Image,
    Speech,
    Transcription,
    Rerank,
    Realtime,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    Active,
    Preview,
    Deprecated,
    Retired,
    #[serde(other)]
    Other,
}

/// What a model can do. Every field is optional: `None` is unknown.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Properties {
    /// Input and output tokens together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<Modalities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// PDF and other documents as input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documents: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    /// Output to a JSON schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<bool>,
    /// Output as JSON without a schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// The reasoning efforts the API takes (`low`, `medium`, `high`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_efforts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
    /// Whether the vendor caches prompts, priced or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<bool>,
    /// Tools the vendor runs itself (`web_search`, `code_execution`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_tools: Option<Vec<String>>,
}

impl Properties {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Fills what `self` leaves unknown from `other`.
    pub fn fill_from(&mut self, other: &Properties) {
        macro_rules! fill {
            ($($f:ident),*) => { $( if self.$f.is_none() { self.$f = other.$f.clone(); } )* };
        }
        fill!(
            context_window,
            max_output_tokens,
            modalities,
            vision,
            documents,
            tools,
            structured_output,
            json_mode,
            reasoning,
            reasoning_efforts,
            default_effort,
            prompt_cache,
            server_tools
        );
    }
}

/// What goes in and what comes out: `text`, `image`, `audio`, `video`, `document`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Modalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

/// How a model treats one request parameter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<Support>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// The API takes the parameter.
    Accepted,
    /// The API refuses a request that sets it.
    Rejected,
    /// The API takes only its default value.
    DefaultOnly,
    #[serde(other)]
    Other,
}

/// What a model costs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Price {
    #[serde(default = "usd")]
    pub currency: String,
    #[serde(default, skip_serializing_if = "Rates::is_empty")]
    pub rates: Rates,
    /// Rates the vendor bills apart, by the name its usage reports (the service tier): `batch`,
    /// `priority`, `flex`, `off_peak`, …
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, Rates>,
    /// Rates for long prompts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<Tier>,
    /// Hours in which a variant applies by the clock.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<Window>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<DateTime<Utc>>,
    /// When the price last changed at the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_at: Option<DateTime<Utc>>,
    /// When the source last confirmed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

impl Default for Price {
    fn default() -> Self {
        Self {
            currency: usd(),
            rates: Rates::default(),
            variants: BTreeMap::new(),
            tiers: Vec::new(),
            windows: Vec::new(),
            effective_from: None,
            changed_at: None,
            checked_at: None,
            evidence: None,
        }
    }
}

impl Price {
    /// Whether it says anything about cost.
    pub fn is_empty(&self) -> bool {
        self.rates.is_empty() && self.variants.values().all(Rates::is_empty) && self.tiers.is_empty()
    }
}

fn usd() -> String {
    "USD".to_owned()
}

/// Rates per million tokens, and per unit for the rest. `None` is not charged or not known.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_5m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<f64>,
    /// Only where reasoning tokens are priced apart from output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<f64>,
    /// Speech: per character of input text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_character: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_request: Option<f64>,
    /// Per tool call the vendor runs, a web search for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_server_tool_call: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_audio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_image: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_image: Option<f64>,
    /// Image models: per image made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_image: Option<f64>,
    /// Transcription: per second of audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_audio_second: Option<f64>,
    /// Rerank: per search unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_search_unit: Option<f64>,
}

impl Rates {
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

    /// A rate that cannot be a price: negative, not finite, or larger than any model charges.
    /// Sources mistake a price per thousand tokens for one per million, or write a router's
    /// placeholder; such a rate is better left out than applied.
    pub fn implausible(&self) -> Option<&'static str> {
        self.named().into_iter().find_map(|(name, v)| {
            let v = v?;
            let bound = match name {
                "per_character" => 0.01,
                "per_request" | "per_server_tool_call" | "per_image" => 10.0,
                "per_audio_second" => 0.1,
                "per_search_unit" => 1.0,
                _ => 10_000.0,
            };
            (!v.is_finite() || v < 0.0 || v > bound).then_some(name)
        })
    }
}

/// Rates that apply above a number of input tokens.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tier {
    pub above_input_tokens: u64,
    #[serde(flatten)]
    pub rates: Rates,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variants: BTreeMap<String, Rates>,
}

/// Hours in which a variant applies by the clock (off-peak prices).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub variant: String,
    /// ISO weekdays: 1 is Monday, 7 is Sunday.
    pub weekdays: Vec<u8>,
    /// `HH:MM-HH:MM` in UTC.
    pub hours_utc: Vec<String>,
}

/// What a price was read from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The words the figures were taken from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `rendered_page`, `feed`, `manual`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<DateTime<Utc>>,
}

/// Where one value came from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
}

impl Catalog {
    /// The vendor entry for `id`, if the catalogue has one.
    pub fn vendor(&self, id: &str) -> Option<&Vendor> {
        self.vendors.iter().find(|v| v.id == id)
    }
}
