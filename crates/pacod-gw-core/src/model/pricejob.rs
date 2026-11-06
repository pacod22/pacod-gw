//! The price job's configuration: the daily sync of model data and the age at which a price
//! counts as stale (concept section 13.3).

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Doc;

/// The kind of a source of earlier releases.
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

/// One source of prices for one provider, as earlier releases configured them. No longer read:
/// model data comes from the sync's source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PriceSourceSpec {
    /// The provider's object id.
    pub provider_id: String,
    pub kind: SourceKind,
    pub url: String,
    /// For feeds.
    #[serde(default)]
    pub format: FeedFormat,
    /// For feeds: the prefix the feed files this provider's models under, where it is not the
    /// gateway's name for the provider. LiteLLM lists GLM as `zai/…`, Kimi as `moonshot/…`,
    /// Gemini as `gemini/…` and DeepSeek as `deepseek/…`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_prefix: Option<String>,
}

/// The template of earlier releases that extracted rates from pricing pages. No longer used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Extraction {
    /// Always the built-in `_system` workspace; empty is read as it.
    #[serde(default)]
    pub workspace_id: String,
    /// The template's name, without the `@`.
    pub template: String,
}

/// Where the daily model-data sync takes model information and prices from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    /// pacod-aggregator's catalogue, at `PGW_AGGREGATOR_URL`: what the vendors' own pages say.
    PacodAggregator,
    /// OpenRouter's list of models. Its prices apply for `openrouter_price_vendors` only.
    Openrouter,
    /// LiteLLM's list of models and prices.
    Litellm,
    /// The models file of the configuration (`PGW_MODELS_FILE`) alone, read again each day:
    /// for installations without outbound access.
    File,
}

impl CatalogSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::PacodAggregator => "pacod_aggregator",
            Self::Openrouter => "openrouter",
            Self::Litellm => "litellm",
            Self::File => "file",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::PacodAggregator, Self::Openrouter, Self::Litellm, Self::File].into_iter().find(|s| s.name() == name)
    }
}

/// Who set the job's values: the environment at start (`PGW_CATALOG_*`), which sets them again
/// at every start, or an administrator, whose values stand from then on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Env,
    /// Also every job saved before the environment could set it.
    #[default]
    Console,
}

/// OpenRouter's prices are the vendor's own for these vendors; for others, open-weight models
/// in particular, they are usually a third party's.
pub const OPENROUTER_PRICE_VENDORS: [&str; 4] = ["openai", "anthropic", "google", "xai"];

/// The daily sync of model data: its source and when it runs, and how old a price may get.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PriceJob {
    /// Whether the sync runs on its schedule.
    pub enabled: bool,
    /// When the model data is synced. A cron expression with five fields (minute, hour, day of
    /// month, month, day of week), as standard cron reads it: days of the week from 0 (Sunday)
    /// to 7 (Sunday again) or by name. It may restrict the day of the month or the day of the
    /// week, not both.
    pub schedule: String,
    /// The IANA time zone the schedule is read in.
    pub time_zone: String,
    /// The sources of earlier releases. No longer read; kept as they are for an instance of an
    /// earlier release.
    pub sources: Vec<PriceSourceSpec>,
    /// No longer used: every valid price a source states applies, unless the model's price is
    /// pinned. Kept, and still checked, so that an older instance reads the same job.
    pub auto_apply_bound: f64,
    /// Age in days after which a price counts as stale and raises an alert.
    pub stale_days: u32,
    /// The template of earlier releases that read pricing pages. No longer used; kept as it is
    /// for an instance of an earlier release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extraction: Option<Extraction>,
    /// Where the sync takes model information and prices from, on `schedule`. Always set, so
    /// that a job saved without the field cannot read as another source: `enabled` is what
    /// stops the sync.
    pub source: CatalogSource,
    /// OpenRouter's list of models.
    pub openrouter_url: String,
    /// LiteLLM's list of models and prices.
    pub litellm_url: String,
    /// The vendors whose prices are taken from OpenRouter. Its properties apply for every vendor.
    pub openrouter_price_vendors: Vec<String>,
    /// Whether the properties a source states (context window, modalities, capabilities) apply
    /// to the models, besides the prices.
    pub apply_properties: bool,
    /// Who set these values. Read-only: saving the job makes it `console`.
    pub origin: Origin,
}

impl Default for PriceJob {
    fn default() -> Self {
        Self {
            // On, so that a new installation has current prices without being asked.
            enabled: true,
            // Daily, at a quiet hour.
            schedule: "17 3 * * *".into(),
            time_zone: "UTC".into(),
            sources: Vec::new(),
            auto_apply_bound: 0.3,
            stale_days: 30,
            extraction: None,
            source: CatalogSource::PacodAggregator,
            openrouter_url: pacod_agg_catalog::openrouter::URL.into(),
            litellm_url: pacod_agg_catalog::litellm::URL.into(),
            openrouter_price_vendors: OPENROUTER_PRICE_VENDORS.map(String::from).to_vec(),
            apply_properties: true,
            origin: Origin::Console,
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
    /// The schedule as the `cron` crate reads it.
    pub fn parsed_schedule(&self) -> Result<cron::Schedule, String> {
        parse_cron(&self.schedule)
    }

    /// Whether a scheduled run is due at `now`, given when the job last started.
    pub fn due(&self, last_started: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        self.enabled && due_by(&self.schedule, &self.time_zone, last_started, now)
    }
}

/// The days of the week as the `cron` crate names them, Sunday first.
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// A five-field cron expression (minute, hour, day of month, month, day of week) as standard
/// cron reads it, turned into what the `cron` crate reads. That crate wants a seconds field
/// first, numbers the days of the week from 1 (Sunday) to 7, and runs only on days that match
/// both day fields. Standard cron numbers them from 0 (Sunday) to 7 (Sunday again) and runs on
/// days that match either field. The day of the week is therefore passed on as names, and an
/// expression that restricts both day fields is refused rather than read differently.
pub fn parse_cron(expr: &str) -> Result<cron::Schedule, String> {
    use std::str::FromStr;
    let fields: Vec<&str> = expr.split_whitespace().collect();
    let [minute, hour, day_of_month, month, day_of_week] = fields[..] else {
        return Err("schedule must have five fields: minute, hour, day of month, month, day of week".into());
    };
    let day_of_week = match weekdays(day_of_week).map_err(|e| format!("schedule: {e}"))? {
        None => "*".to_owned(),
        Some(_) if day_of_month != "*" => {
            return Err("schedule restricts both the day of the month and the day of the week, which standard cron \
                        reads as either of them: set one of the two to *"
                .into());
        }
        Some(days) => WEEKDAYS.iter().zip(days).filter(|(_, on)| *on).map(|(d, _)| *d).collect::<Vec<_>>().join(","),
    };
    cron::Schedule::from_str(&format!("0 {minute} {hour} {day_of_month} {month} {day_of_week}"))
        .map_err(|e| format!("schedule is not a cron expression: {e}"))
}

/// The days a standard cron day-of-week field names, Sunday first. `None` where it names every
/// day: such a field restricts nothing.
fn weekdays(field: &str) -> Result<Option<[bool; 7]>, String> {
    let number = |s: &str| -> Result<usize, String> {
        match s.parse::<usize>() {
            Ok(n) if n <= 7 => Ok(n),
            Ok(_) => Err(format!("{s} is not a day of the week (0 to 7, Sunday is 0 and 7)")),
            Err(_) => WEEKDAYS
                .iter()
                .position(|d| d.eq_ignore_ascii_case(s))
                .ok_or_else(|| format!("\"{s}\" is not a day of the week (0 to 7, or sun, mon, … sat)")),
        }
    };
    let mut days = [false; 7];
    for item in field.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => {
                let step = step.parse::<usize>().ok().filter(|s| *s > 0);
                (range, step.ok_or_else(|| format!("\"{item}\" has no valid step"))?)
            }
            None => (item, 1),
        };
        let (from, to) = match range.split_once('-') {
            _ if range == "*" => (0, 6),
            Some((from, to)) => (number(from)?, number(to)?),
            // A single day with a step runs from that day to the end of the week.
            None if step > 1 => (number(range)?, 7),
            None => (number(range)?, number(range)?),
        };
        if from > to {
            return Err(format!("the range \"{range}\" goes backwards; write it as two ranges"));
        }
        for day in (from..=to).step_by(step) {
            days[day % 7] = true;
        }
    }
    Ok(days.iter().any(|on| !on).then_some(days))
}

/// Whether a run on `schedule`, read in `time_zone`, is due at `now`, given when the job last
/// started. A job that never ran is due; a schedule or time zone that cannot be read never is.
pub fn due_by(schedule: &str, time_zone: &str, last_started: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    let Some(last) = last_started else { return true };
    next_after(schedule, time_zone, last).is_some_and(|next| next <= now)
}

/// The first start of `schedule`, read in `time_zone`, after `from`.
pub fn next_after(schedule: &str, time_zone: &str, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let tz = time_zone.parse::<chrono_tz::Tz>().ok()?;
    parse_cron(schedule).ok()?.after(&from.with_timezone(&tz)).next().map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone, Utc};

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

    /// The days on which `schedule` runs at 03:00 UTC in the week from Sunday 8 November 2026.
    fn days_of(schedule: &str) -> Vec<String> {
        let mut from = Utc.with_ymd_and_hms(2026, 11, 7, 12, 0, 0).unwrap();
        let mut days = Vec::new();
        while let Some(next) = next_after(schedule, "UTC", from).filter(|t| t.day() <= 14) {
            days.push(next.format("%a").to_string());
            from = next;
        }
        days
    }

    #[test]
    fn days_of_the_week_are_numbered_as_standard_cron_numbers_them() {
        assert_eq!(days_of("0 3 * * 1-5"), ["Mon", "Tue", "Wed", "Thu", "Fri"]);
        assert_eq!(days_of("0 3 * * mon-fri"), ["Mon", "Tue", "Wed", "Thu", "Fri"]);
        for sunday in ["0", "7", "sun", "SUN"] {
            assert_eq!(days_of(&format!("0 3 * * {sunday}")), ["Sun"], "{sunday}");
        }
        assert_eq!(days_of("0 3 * * 5-7"), ["Sun", "Fri", "Sat"]);
        assert_eq!(days_of("0 3 * * */2"), ["Sun", "Tue", "Thu", "Sat"]);
        assert_eq!(days_of("0 3 * * 1,3"), ["Mon", "Wed"]);
        assert_eq!(days_of("0 3 * * 1/2"), ["Sun", "Mon", "Wed", "Fri"]);
        assert_eq!(days_of("0 3 * * *").len(), 7);
        // A week from Monday 9 November: the weekly default runs on that Monday.
        let saturday = Utc.with_ymd_and_hms(2026, 11, 7, 12, 0, 0).unwrap();
        assert_eq!(
            next_after("41 4 * * 1", "UTC", saturday),
            Some(Utc.with_ymd_and_hms(2026, 11, 9, 4, 41, 0).unwrap())
        );
    }

    #[test]
    fn a_schedule_restricts_one_of_the_day_fields() {
        assert!(parse_cron("0 3 1 * 1").is_err(), "standard cron reads this as the 1st or a Monday");
        assert!(parse_cron("0 3 1 * *").is_ok());
        // Every day of the week restricts nothing.
        assert!(parse_cron("0 3 1 * 0-6").is_ok());
        for bad in ["0 3 * * 8", "0 3 * * fri-mon", "0 3 * * 1/0", "0 3 * * someday", "0 3 * * 1,,2"] {
            assert!(parse_cron(bad).is_err(), "{bad}");
        }
    }
}
