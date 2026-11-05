//! The pages the price tracker reads for a provider: pricing pages and model pages, which a
//! headless browser renders and a model reads, and machine-readable price lists (feeds).
//!
//! The weekly scan proposes pages; an administrator confirms each one, and from then on the
//! daily check reads it. What the check finds out about a page (when it last changed, what it
//! said) is kept apart in `price_page_state`, so that a daily check makes no configuration
//! change.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Doc, FeedFormat};

/// What a page is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    /// A web page, rendered in a browser and read by a model.
    #[default]
    Page,
    /// A machine-readable price list, read without a browser or a model.
    Feed,
}

/// Whether the daily check reads a page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PageStatus {
    /// Found by the scan, waiting for an administrator to confirm it.
    Proposed,
    /// Read by the daily check.
    #[default]
    Active,
    /// Not read, and not proposed again.
    Dismissed,
}

/// Who put a page on the list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FoundBy {
    /// An administrator added it.
    #[default]
    Admin,
    /// The weekly scan found it on the provider's site.
    Scan,
    /// The gateway knows it as the provider's pricing page (the provider presets).
    Preset,
    /// A source of the price job of an earlier release.
    Migration,
}

/// One page of a provider's, as the price tracker reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PricePage {
    /// The provider's object id.
    pub provider_id: String,
    /// The page's address, normalised (`pacod_gw_events::pages::normalise_url`).
    pub url: String,
    #[serde(default)]
    pub kind: PageKind,
    /// For feeds: the shape of the list.
    #[serde(default)]
    pub format: FeedFormat,
    /// For feeds: the prefix the list files this provider's models under, where it is not the
    /// gateway's name for the provider (LiteLLM: `zai`, `moonshot`, `gemini`, `deepseek`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_prefix: Option<String>,
    #[serde(default)]
    pub status: PageStatus,
    #[serde(default)]
    pub found_by: FoundBy,
    /// Where two pages of a provider say different things about a model, the lower number
    /// wins. Pages of the same priority are taken in the order they were added.
    #[serde(default)]
    pub priority: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// The page's title, as the scan saw it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// How strongly the scan judged the page to list models and prices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// The page this one replaces, where the scan found that a page moved here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
    /// Who confirmed or dismissed the page, and when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
}

impl Doc for PricePage {
    const KIND: &'static str = "price_page";
    const PREFIX: &'static str = "ppg";
}

impl PricePage {
    /// The name the page is unique under: one provider has a page at an address once. One
    /// address can serve several providers (LiteLLM's list).
    pub fn unique_name(provider_id: &str, url: &str) -> String {
        format!("{provider_id} {url}")
    }
}
