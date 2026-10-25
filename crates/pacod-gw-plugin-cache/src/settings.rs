//! A binding's settings (`docs/plugin-concept.md`, section 15.3).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which calls are answered from the cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Calls {
    /// Calls with temperature 0: in the request the provider is sent, or, for a model that
    /// takes no temperature, in the caller's request. The caller expects the same answer to
    /// the same request.
    #[default]
    Deterministic,
    /// Every call. A caller that asks again gets the same answer, whatever the temperature.
    All,
}

/// The longest an answer may be kept: a week.
pub const MAX_TTL_SECONDS: u32 = 604_800;

/// The settings of a response cache binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub calls: Calls,
    /// How long an answer is kept, in seconds.
    pub ttl_seconds: u32,
    /// Larger answers are not kept, in bytes as stored.
    pub max_answer_bytes: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { calls: Calls::Deterministic, ttl_seconds: 3_600, max_answer_bytes: 1 << 20 }
    }
}

impl Settings {
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !(1..=MAX_TTL_SECONDS).contains(&self.ttl_seconds) {
            out.push(format!("ttl_seconds must be between 1 and {MAX_TTL_SECONDS}"));
        }
        if !(1_024..=(16 << 20)).contains(&self.max_answer_bytes) {
            out.push("max_answer_bytes must be between 1024 and 16777216".into());
        }
        out
    }
}
