//! Which format a file is in, and reading it whatever it is.

use std::fmt;

use serde_json::Value;

use crate::v1::{Catalog, SCHEMA_V1};
use crate::{feed, litellm, openrouter};

/// The formats a catalogue can be read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `pacod.catalog/v1`: the aggregator's API, a gateway's models file, an upload.
    Catalog,
    Openrouter,
    Litellm,
    /// The price feed of earlier pacod-gw releases.
    PacodFeed,
}

impl Format {
    /// The name a request or a setting gives the format: `catalog`, `openrouter`, `litellm`,
    /// `pacod`.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "catalog" | "v1" => Self::Catalog,
            "openrouter" => Self::Openrouter,
            "litellm" => Self::Litellm,
            "pacod" | "pacod_feed" => Self::PacodFeed,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Openrouter => "openrouter",
            Self::Litellm => "litellm",
            Self::PacodFeed => "pacod",
        }
    }
}

/// Why a file could not be read.
#[derive(Debug)]
pub enum ParseError {
    /// Not JSON at all; where the parser stopped.
    Json(serde_json::Error),
    /// JSON, but none of the formats.
    Unknown,
    /// A catalogue of a version this reader does not know.
    Schema(String),
    /// The format, but not in its shape.
    Shape { format: Format, reason: String },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "not JSON: {e}"),
            Self::Unknown => {
                write!(f, "neither a catalogue ({SCHEMA_V1}) nor OpenRouter's, LiteLLM's or a pacod price list")
            }
            Self::Schema(s) => write!(f, "a catalogue of schema {s}; this gateway reads {SCHEMA_V1}"),
            Self::Shape { format, reason } => write!(f, "not a valid {} file: {reason}", format.name()),
        }
    }
}

impl std::error::Error for ParseError {}

/// The format of a parsed file, by what only that format has.
pub fn detect(v: &Value) -> Option<Format> {
    let o = v.as_object()?;
    if o.get("schema").and_then(Value::as_str).is_some_and(|s| s.starts_with("pacod.catalog/")) {
        return Some(Format::Catalog);
    }
    if o.get("data").is_some_and(Value::is_array) {
        return Some(Format::Openrouter);
    }
    if o.get("models").is_some_and(Value::is_object) {
        return Some(Format::PacodFeed);
    }
    if o.values().any(|e| e.get("litellm_provider").is_some()) {
        return Some(Format::Litellm);
    }
    None
}

/// Reads a file in `format`, or in the format it turns out to be in.
pub fn parse(body: &[u8], format: Option<Format>) -> Result<Catalog, ParseError> {
    let v: Value = serde_json::from_slice(body).map_err(ParseError::Json)?;
    let format = match format {
        Some(f) => f,
        None => detect(&v).ok_or(ParseError::Unknown)?,
    };
    let shape = |reason: String| ParseError::Shape { format, reason };
    match format {
        Format::Catalog => {
            let schema = v.get("schema").and_then(Value::as_str).unwrap_or_default();
            if schema != SCHEMA_V1 {
                return Err(ParseError::Schema(schema.to_owned()));
            }
            serde_json::from_value(v).map_err(|e| shape(e.to_string()))
        }
        Format::Openrouter => openrouter::read(&v).map_err(shape),
        Format::Litellm => litellm::read(&v).map_err(shape),
        Format::PacodFeed => feed::read(&v).map_err(shape),
    }
}
