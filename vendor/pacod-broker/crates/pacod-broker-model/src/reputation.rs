//! The reputation a subject has, as discovery and the APIs show it (docs/reputation.md).
//!
//! A score never travels alone: every one carries its components, how many pieces of evidence
//! it rests on, how confident the broker is, and when it was computed. The computation lives in
//! pacod-broker-reputation; this is only the view.

use std::collections::BTreeMap;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Reputation {
    /// The composite, 0 to 1.
    pub score: f64,
    /// 0 to 1: how much evidence there is. Below 0.3 the subject is `new`.
    pub confidence: f64,
    pub label: Label,
    /// Each component by its id (`availability`, `reliability`, …).
    pub components: BTreeMap<String, Component>,
    pub computed_at: Timestamp,
    /// An admin froze the score while looking into it; it does not change until unfrozen.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub frozen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Component {
    pub score: f64,
    /// The weight it has in the composite.
    pub weight: f64,
    /// Evidence counted, after decay (so fractional).
    pub samples: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Label {
    New,
    Low,
    Fair,
    Good,
    Excellent,
}

impl Label {
    pub fn of(score: f64, confidence: f64) -> Self {
        if confidence < 0.3 {
            Self::New
        } else if score < 0.4 {
            Self::Low
        } else if score < 0.6 {
            Self::Fair
        } else if score < 0.8 {
            Self::Good
        } else {
            Self::Excellent
        }
    }
}

impl Reputation {
    /// The reputation of a subject with no evidence yet: the neutral prior.
    pub fn neutral(at: Timestamp) -> Self {
        Self {
            score: 0.5,
            confidence: 0.0,
            label: Label::New,
            components: BTreeMap::new(),
            computed_at: at,
            frozen: false,
        }
    }
}
