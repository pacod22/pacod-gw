//! The catalogue of LLM models that pacod-aggregator publishes (`pacod.catalog/v1`): what each
//! model is, what it can do, which parameters it takes and what it costs. Readers turn the
//! lists of OpenRouter and LiteLLM, and the price feed of earlier pacod-gw releases, into the
//! same catalogue, so that a consumer reads every source one way.
//!
//! pacod-gw vendors this crate (its `scripts/vendor-pacod-aggregator.sh`), and its gateways read
//! every source with it: the aggregator's API, OpenRouter, LiteLLM, an uploaded file and the
//! models file of its configuration. The crate therefore depends on nothing of the workspace.
//!
//! Version 1 grows by adding fields only. Readers ignore what they do not know, and `null` or a
//! missing field means unknown, never "not supported".

pub mod feed;
pub mod find;
pub mod ids;
pub mod litellm;
pub mod openrouter;
pub mod v1;
pub mod vendors;

mod detect;

pub use detect::{Format, ParseError, detect, parse};
pub use find::{Found, Index};
pub use v1::*;
