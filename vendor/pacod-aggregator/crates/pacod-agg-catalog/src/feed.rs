//! The price feed of earlier pacod-gw releases, which pacod-broker serves too: rates by model id
//! (`openai/gpt-5-mini`, or the id alone), token rates per million tokens, with the model's
//! properties as the gateway's price tracker read them.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use crate::v1::{Catalog, Modalities, Model, Price, Properties, Rates, Source, SourceKind, Tier, Vendor, Window};
use crate::vendors;

#[derive(Debug, Deserialize)]
struct Feed {
    #[serde(default = "usd")]
    currency: String,
    models: BTreeMap<String, Entry>,
}

fn usd() -> String {
    "USD".to_owned()
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(flatten)]
    rates: Rates,
    #[serde(default)]
    variants: BTreeMap<String, Rates>,
    #[serde(default)]
    tiers: Vec<Tier>,
    #[serde(default)]
    windows: Vec<Window>,
    #[serde(default)]
    properties: FeedProperties,
}

/// The properties as the feed names them.
#[derive(Debug, Default, Deserialize)]
struct FeedProperties {
    context_window: Option<u64>,
    max_output_tokens: Option<u64>,
    input_modalities: Option<Vec<String>>,
    output_modalities: Option<Vec<String>>,
    vision: Option<bool>,
    tools: Option<bool>,
    structured_output: Option<bool>,
    reasons: Option<bool>,
}

/// Reads a feed. A key `vendor/model` names its vendor; a key without one leaves it to the
/// reader to find the model by its id alone.
pub fn read(feed: &Value) -> Result<Catalog, String> {
    let feed: Feed = serde_json::from_value(feed.clone()).map_err(|e| format!("not a pacod price feed: {e}"))?;
    let mut vendor_ids = BTreeSet::new();
    let mut models = Vec::new();
    for (key, e) in feed.models {
        let (vendor, api_id) = match key.split_once('/') {
            Some((v, id)) => (Some(v.to_owned()), id.to_owned()),
            None => (None, key),
        };
        if let Some(v) = &vendor {
            vendor_ids.insert(v.clone());
        }
        let p = e.properties;
        let modalities = (p.input_modalities.is_some() || p.output_modalities.is_some()).then(|| Modalities {
            input: p.input_modalities.unwrap_or_default(),
            output: p.output_modalities.unwrap_or_default(),
        });
        let price = Price {
            currency: feed.currency.clone(),
            rates: e.rates,
            variants: e.variants,
            tiers: e.tiers,
            windows: e.windows,
            ..Default::default()
        };
        models.push(Model {
            vendor,
            api_id,
            properties: Properties {
                context_window: p.context_window,
                max_output_tokens: p.max_output_tokens,
                modalities,
                vision: p.vision,
                tools: p.tools,
                structured_output: p.structured_output,
                reasoning: p.reasons,
                ..Default::default()
            },
            price: (!price.is_empty()).then_some(price),
            ..Default::default()
        });
    }
    let vendors = vendor_ids
        .into_iter()
        .map(|id| Vendor {
            display_name: vendors::info(&id).map(|i| i.display_name.to_owned()),
            currency: Some(feed.currency.clone()),
            id,
            ..Default::default()
        })
        .collect();
    Ok(Catalog {
        source: Source { kind: SourceKind::PacodFeed, url: None, version: None },
        vendors,
        models,
        ..Default::default()
    })
}
