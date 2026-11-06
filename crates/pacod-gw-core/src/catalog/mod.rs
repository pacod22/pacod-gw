//! Model data from outside the configuration: a catalogue (`pacod.catalog/v1`, read with the
//! vendored `pacod-agg-catalog`) from pacod-aggregator, OpenRouter, LiteLLM, an uploaded file or
//! the models file. `convert` turns its entries into the gateway's own objects, for the full
//! gateway's sync (`mgmt/catalog.rs`) and for the lite gateway's models file alike; `fetch`
//! fetches a source within the egress policy.

pub mod convert;
pub mod fetch;
