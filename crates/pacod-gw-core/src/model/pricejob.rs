//! The price tracker's configuration (concept section 13.3).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Doc;

/// Where the tracker looks for a provider's prices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A machine-readable price list.
    Feed,
    /// A pricing page. Its text is given to an LLM, which extracts the rates.
    Page,
}

/// The shape of a price feed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedFormat {
    /// `{"currency": "USD", "models": {"<model id>": {"input": 2.0, "output": 10.0, …,
    /// "variants": {"batch": {"input": 1.0, …}}}}}`: the rates of a price, token rates per
    /// million tokens, the others per character, request or tool call.
    #[default]
    Pacod,
    /// LiteLLM's public list: `{"<model id>": {"input_cost_per_token": …, …}}`, rates per token;
    /// its `_batches`, `_priority` and `_flex` fields become those variants.
    Litellm,
}

/// One source of prices for one provider. Sources are tried in order until one yields rates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PriceSourceSpec {
    /// The provider's object id.
    pub provider_id: String,
    pub kind: SourceKind,
    pub url: String,
    /// For feeds.
    #[serde(default)]
    pub format: FeedFormat,
}

/// The template that extracts rates from pricing pages. Its calls go through the gateway's own
/// data plane and are accounted to the workspace like any other usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Extraction {
    /// Always the built-in `_system` workspace; empty is read as it.
    #[serde(default)]
    pub workspace_id: String,
    /// The template's name, without the `@`.
    pub template: String,
}

/// The price tracker: when it runs, where it looks, and how much it may change on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PriceJob {
    pub enabled: bool,
    /// A cron expression with five fields (minute, hour, day of month, month, day of week).
    pub schedule: String,
    /// The IANA time zone the schedule is read in.
    pub time_zone: String,
    pub sources: Vec<PriceSourceSpec>,
    /// The largest relative change that is applied without approval: 0.3 is 30 %. A larger
    /// change, and a model's first price, become a proposal.
    pub auto_apply_bound: f64,
    /// Age in days after which a price counts as stale and raises an alert.
    pub stale_days: u32,
    /// Needed for sources of kind `page`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extraction: Option<Extraction>,
}

impl Default for PriceJob {
    fn default() -> Self {
        Self {
            enabled: false,
            // Daily, at a quiet hour.
            schedule: "17 3 * * *".into(),
            time_zone: "UTC".into(),
            sources: Vec::new(),
            auto_apply_bound: 0.3,
            stale_days: 30,
            extraction: None,
        }
    }
}

impl Doc for PriceJob {
    const KIND: &'static str = "price_job";
    const PREFIX: &'static str = "price_job";
}

/// The id of the single price-job object.
pub const PRICE_JOB_ID: &str = "price_job";

impl PriceJob {
    /// The schedule as the `cron` crate reads it, which wants a seconds field first.
    pub fn parsed_schedule(&self) -> Result<cron::Schedule, String> {
        use std::str::FromStr;
        let fields = self.schedule.split_whitespace().count();
        if fields != 5 {
            return Err("schedule must have five fields: minute, hour, day of month, month, day of week".into());
        }
        cron::Schedule::from_str(&format!("0 {}", self.schedule))
            .map_err(|e| format!("schedule is not a cron expression: {e}"))
    }

    /// Whether a scheduled run is due at `now`, given when the job last started.
    pub fn due(&self, last_started: Option<chrono::DateTime<chrono::Utc>>, now: chrono::DateTime<chrono::Utc>) -> bool {
        if !self.enabled {
            return false;
        }
        let Some(last) = last_started else { return true };
        let (Ok(schedule), Ok(tz)) = (self.parsed_schedule(), self.time_zone.parse::<chrono_tz::Tz>()) else {
            return false;
        };
        schedule.after(&last.with_timezone(&tz)).next().is_some_and(|next| next <= now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn the_schedule_is_read_in_its_time_zone() {
        let job = PriceJob {
            enabled: true,
            schedule: "0 3 * * *".into(),
            time_zone: "Europe/Vienna".into(),
            ..Default::default()
        };
        // 03:00 in Vienna on 6 October 2026 is 01:00 UTC (summer time).
        let last = Utc.with_ymd_and_hms(2026, 10, 5, 1, 0, 0).unwrap();
        assert!(!job.due(Some(last), Utc.with_ymd_and_hms(2026, 10, 6, 0, 59, 0).unwrap()));
        assert!(job.due(Some(last), Utc.with_ymd_and_hms(2026, 10, 6, 1, 0, 0).unwrap()));
        assert!(job.due(None, last), "a job that never ran is due");
        assert!(!PriceJob { enabled: false, ..job.clone() }.due(None, last));

        assert!(PriceJob::default().parsed_schedule().is_ok());
        for bad in ["", "* * * *", "0 0 3 * * *", "61 3 * * *", "daily"] {
            assert!(PriceJob { schedule: bad.into(), ..Default::default() }.parsed_schedule().is_err(), "{bad}");
        }
    }
}
