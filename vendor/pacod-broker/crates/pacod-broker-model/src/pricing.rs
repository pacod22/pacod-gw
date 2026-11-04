//! Prices over time (docs/concept.md §2.3).
//!
//! A resource's prices are a list of **periods**, each valid from an instant until the next one
//! (or until `valid_to`). Within a period, **windows** give recurring variants (off-peak hours,
//! weekends), **overrides** give one-off prices for an absolute span (a spot discount), **tiers**
//! give volume prices and **commitments** reserved prices. The fields of `Rates` mirror
//! pacod-gw's (`crates/pacod-gw-core/src/model/repo.rs`), plus compute units.
//!
//! The effective price at an instant is: the period valid then; an override covering the instant,
//! else the first window open then, else the period's base rates. A rate the variant leaves out
//! keeps the base rate. Tiers and commitments are shown, never applied: the broker does not know
//! the caller's volume.
//!
//! Prices are never retroactive: a period that has started cannot be changed, only ended
//! (`valid_to` moved, not before now) and followed by a new one. A missing price means
//! "unpriced", never zero.

use std::collections::BTreeMap;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::limits;
use crate::schedule::{Clock, Recurring, Span, Zone};
use crate::validate::{self, Problems};

/// The prices of a resource.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pricing {
    /// ISO 4217, upper case: `EUR`, `USD`.
    #[serde(default = "usd")]
    pub currency: String,
    /// The resource costs nothing. Then no period is needed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub free: bool,
    /// The zone the windows' times are in.
    #[serde(default)]
    pub timezone: Zone,
    #[serde(default)]
    pub periods: Vec<PricePeriod>,
}

fn usd() -> String {
    "USD".into()
}

/// One price-sheet version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PricePeriod {
    pub valid_from: Timestamp,
    /// Open when absent: valid until the next period starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<Timestamp>,
    pub rates: Rates,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<PriceWindow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<PriceOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<Tier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commitments: Vec<Commitment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Rates, each optional. Token rates are per million tokens; the others per unit named.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rates {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_request: Option<f64>,
    /// Per tool or MCP call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_call: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_image: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_audio_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_hour: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_gpu_hour: Option<f64>,
}

/// A recurring variant within a period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceWindow {
    /// A slug: `off-peak`, `weekend`.
    pub name: String,
    #[serde(flatten)]
    pub when: Recurring,
    pub rates: Rates,
}

/// A one-off price for an absolute span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PriceOverride {
    pub from: Timestamp,
    pub to: Timestamp,
    pub rates: Rates,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A volume price: the rates for usage above `above` units within `per` (a month by default).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Tier {
    pub above: f64,
    pub unit: TierUnit,
    pub rates: Rates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TierUnit {
    InputTokens,
    OutputTokens,
    Requests,
    Hours,
}

/// A reserved price for a commitment of at least `min_hours`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Commitment {
    pub min_hours: u32,
    pub rates: Rates,
}

/// The price in effect at one instant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EffectivePrice {
    pub at: Timestamp,
    pub currency: String,
    pub free: bool,
    pub rates: Rates,
    /// `base`, `window:<name>` or `override`.
    pub source: String,
    pub period_from: Option<Timestamp>,
    pub period_to: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<Tier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commitments: Vec<Commitment>,
}

/// The prices over a span: each rate's lowest, highest and time-weighted mean, and when they
/// change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PriceSummary {
    pub from: Timestamp,
    pub to: Timestamp,
    pub currency: String,
    pub free: bool,
    pub rates: BTreeMap<String, RateRange>,
    /// The instants at which the effective price changes, with the source from then on.
    pub changes: Vec<PriceChange>,
    /// The share of the span with no price at all (before the first period, between periods).
    pub unpriced_share: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RateRange {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PriceChange {
    pub at: Timestamp,
    pub source: String,
}

impl Rates {
    /// The rate names, as filters and sorts name them (`max_price.input`, `sort: price:input`).
    pub const NAMES: [&'static str; 12] = [
        "input",
        "output",
        "cache_read",
        "cache_write",
        "reasoning",
        "per_request",
        "per_call",
        "per_image",
        "per_audio_second",
        "per_second",
        "per_hour",
        "per_gpu_hour",
    ];

    fn fields(&self) -> [Option<f64>; 12] {
        [
            self.input,
            self.output,
            self.cache_read,
            self.cache_write,
            self.reasoning,
            self.per_request,
            self.per_call,
            self.per_image,
            self.per_audio_second,
            self.per_second,
            self.per_hour,
            self.per_gpu_hour,
        ]
    }

    fn fields_mut(&mut self) -> [&mut Option<f64>; 12] {
        [
            &mut self.input,
            &mut self.output,
            &mut self.cache_read,
            &mut self.cache_write,
            &mut self.reasoning,
            &mut self.per_request,
            &mut self.per_call,
            &mut self.per_image,
            &mut self.per_audio_second,
            &mut self.per_second,
            &mut self.per_hour,
            &mut self.per_gpu_hour,
        ]
    }

    pub fn get(&self, name: &str) -> Option<f64> {
        Self::NAMES.iter().position(|n| *n == name).and_then(|i| self.fields()[i])
    }

    pub fn is_known(name: &str) -> bool {
        Self::NAMES.contains(&name)
    }

    pub fn is_empty(&self) -> bool {
        self.fields().iter().all(Option::is_none)
    }

    pub fn named(&self) -> impl Iterator<Item = (&'static str, f64)> + '_ {
        Self::NAMES.iter().zip(self.fields()).filter_map(|(n, v)| v.map(|v| (*n, v)))
    }

    /// These rates with every rate `top` sets replaced.
    pub fn overlay(&self, top: &Rates) -> Rates {
        let mut out = *self;
        for (slot, v) in out.fields_mut().into_iter().zip(top.fields()) {
            if v.is_some() {
                *slot = v;
            }
        }
        out
    }

    fn validate(&self, p: &mut Problems, path: &str) {
        for (name, v) in self.named() {
            p.check(v.is_finite() && v >= 0.0, format!("{path}.{name}"), "a number of zero or more");
        }
    }
}

impl PricePeriod {
    fn contains(&self, t: Timestamp, next_from: Option<Timestamp>) -> bool {
        t >= self.valid_from && self.end(next_from).is_none_or(|e| t < e)
    }

    fn end(&self, next_from: Option<Timestamp>) -> Option<Timestamp> {
        match (self.valid_to, next_from) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

impl Pricing {
    pub fn validate(&self, p: &mut Problems, path: &str) {
        p.check(
            self.currency.len() == 3 && self.currency.bytes().all(|b| b.is_ascii_uppercase()),
            format!("{path}.currency"),
            "an ISO 4217 code, upper case",
        );
        p.check(
            self.free || !self.periods.is_empty(),
            format!("{path}.periods"),
            "at least one period, or `free: true`",
        );
        p.check(self.periods.len() <= limits::PRICE_PERIODS, format!("{path}.periods"), "too many periods");
        for (i, w) in self.periods.windows(2).enumerate() {
            p.check(
                w[0].valid_from < w[1].valid_from,
                format!("{path}.periods[{}]", i + 1),
                "periods are listed in order of valid_from",
            );
            if let Some(to) = w[0].valid_to {
                p.check(to <= w[1].valid_from, format!("{path}.periods[{}]", i + 1), "overlaps the period before it");
            }
        }
        for (i, per) in self.periods.iter().enumerate() {
            let at = format!("{path}.periods[{i}]");
            if let Some(to) = per.valid_to {
                p.check(to > per.valid_from, format!("{at}.valid_to"), "after valid_from");
            }
            per.rates.validate(p, &format!("{at}.rates"));
            p.check(self.free || !per.rates.is_empty(), format!("{at}.rates"), "at least one rate");
            for (name, list) in [
                ("windows", per.windows.len()),
                ("overrides", per.overrides.len()),
                ("tiers", per.tiers.len()),
                ("commitments", per.commitments.len()),
            ] {
                p.check(list <= limits::PRICE_PARTS, format!("{at}.{name}"), "too many");
            }
            for (j, w) in per.windows.iter().enumerate() {
                p.check(crate::ids::is_slug(&w.name, limits::SLUG_CHARS), format!("{at}.windows[{j}].name"), "a slug");
                w.when.validate(p, &format!("{at}.windows[{j}]"));
                w.rates.validate(p, &format!("{at}.windows[{j}].rates"));
            }
            for (j, o) in per.overrides.iter().enumerate() {
                p.check(o.to > o.from, format!("{at}.overrides[{j}]"), "`to` after `from`");
                p.check(
                    o.from >= per.valid_from && per.valid_to.is_none_or(|e| o.to <= e),
                    format!("{at}.overrides[{j}]"),
                    "within the period",
                );
                o.rates.validate(p, &format!("{at}.overrides[{j}].rates"));
            }
            for (j, t) in per.tiers.iter().enumerate() {
                p.check(t.above.is_finite() && t.above > 0.0, format!("{at}.tiers[{j}].above"), "a positive number");
                t.rates.validate(p, &format!("{at}.tiers[{j}].rates"));
            }
            for (j, c) in per.commitments.iter().enumerate() {
                p.check(c.min_hours >= 1, format!("{at}.commitments[{j}].min_hours"), "at least 1");
                c.rates.validate(p, &format!("{at}.commitments[{j}].rates"));
            }
        }
    }

    /// Refuses a change that would rewrite prices already in force. A period that started
    /// before `now` must stay as it was, except that its end may be set or moved, not before
    /// `now`; periods may be added after it.
    pub fn check_not_retroactive(&self, old: &Pricing, now: Timestamp, p: &mut Problems, path: &str) {
        if self.currency != old.currency && old.periods.iter().any(|x| x.valid_from <= now) {
            p.add(
                format!("{path}.currency"),
                "cannot change while a period is in force; announce a new resource version",
            );
        }
        for o in old.periods.iter().filter(|x| x.valid_from <= now) {
            let Some(n) = self.periods.iter().find(|x| x.valid_from == o.valid_from) else {
                if o.valid_to.is_none_or(|e| e > now) {
                    p.add(
                        format!("{path}.periods"),
                        format_args!("the period from {} has started; it can be ended, not removed", o.valid_from),
                    );
                }
                continue;
            };
            let mut same = n.clone();
            same.valid_to = o.valid_to;
            if same != *o {
                p.add(
                    format!("{path}.periods"),
                    format_args!(
                        "the period from {} has started; prices are never retroactive, add a new period",
                        o.valid_from
                    ),
                );
            }
            if n.valid_to != o.valid_to && n.valid_to.is_some_and(|e| e < now) {
                p.add(
                    format!("{path}.periods"),
                    format_args!("the period from {} cannot end in the past", o.valid_from),
                );
            }
        }
    }

    /// The period in force at `t`, if any.
    pub fn period_in_force(&self, t: Timestamp) -> Option<&PricePeriod> {
        self.period_at(t).map(|(_, p)| p)
    }

    fn period_at(&self, t: Timestamp) -> Option<(usize, &PricePeriod)> {
        // Periods are few and in order; a linear scan from the end finds recent ones first.
        let n = self.periods.len();
        (0..n).rev().find_map(|i| {
            let next = self.periods.get(i + 1).map(|x| x.valid_from);
            let per = &self.periods[i];
            per.contains(t, next).then_some((i, per))
        })
    }

    /// The price in effect at `t`; `None` when no period covers `t` and the resource is not free.
    pub fn at(&self, t: Timestamp) -> Option<EffectivePrice> {
        self.at_clock(&Clock::new(t))
    }

    pub fn at_clock(&self, clock: &Clock) -> Option<EffectivePrice> {
        let t = clock.t();
        let Some((i, per)) = self.period_at(t) else {
            return self.free.then(|| EffectivePrice {
                at: t,
                currency: self.currency.clone(),
                free: true,
                rates: Rates::default(),
                source: "free".into(),
                period_from: None,
                period_to: None,
                tiers: vec![],
                commitments: vec![],
            });
        };
        let (rates, source) = self.rates_in(per, clock);
        Some(EffectivePrice {
            at: t,
            currency: self.currency.clone(),
            free: self.free,
            rates,
            source,
            period_from: Some(per.valid_from),
            period_to: per.end(self.periods.get(i + 1).map(|x| x.valid_from)),
            tiers: per.tiers.clone(),
            commitments: per.commitments.clone(),
        })
    }

    /// The effective rates within a period at `t`, and where they come from.
    fn rates_in(&self, per: &PricePeriod, clock: &Clock) -> (Rates, String) {
        match self.variant(per, clock) {
            Variant::Override(o) => (per.rates.overlay(&o.rates), "override".into()),
            Variant::Window(w) => (per.rates.overlay(&w.rates), format!("window:{}", w.name)),
            Variant::Base => (per.rates, "base".into()),
        }
    }

    fn variant<'a>(&self, per: &'a PricePeriod, clock: &Clock) -> Variant<'a> {
        let t = clock.t();
        if let Some(o) = per.overrides.iter().find(|o| t >= o.from && t < o.to) {
            return Variant::Override(o);
        }
        if let Some(w) = per.windows.iter().find(|w| w.when.contains_at(clock, &self.timezone)) {
            return Variant::Window(w);
        }
        Variant::Base
    }

    /// One rate at `t`: the cheap path the discovery filters and sorts use.
    pub fn rate_at(&self, name: &str, t: Timestamp) -> Option<f64> {
        self.rate_at_clock(name, &Clock::new(t))
    }

    /// One rate at the clock's instant, without allocating.
    pub fn rate_at_clock(&self, name: &str, clock: &Clock) -> Option<f64> {
        let Some((_, per)) = self.period_at(clock.t()) else {
            return self.free.then_some(0.0);
        };
        let top = match self.variant(per, clock) {
            Variant::Override(o) => o.rates.get(name),
            Variant::Window(w) => w.rates.get(name),
            Variant::Base => None,
        };
        top.or_else(|| per.rates.get(name)).or(self.free.then_some(0.0))
    }

    /// The prices over `span`.
    pub fn during(&self, span: Span) -> PriceSummary {
        let mut cuts = Vec::new();
        for per in &self.periods {
            cuts.push(per.valid_from);
            cuts.extend(per.valid_to);
            for o in &per.overrides {
                cuts.extend([o.from, o.to]);
            }
            for w in &per.windows {
                w.when.boundaries(span.from, span.to, self.timezone.tz(), &mut cuts);
            }
        }
        let total = span.seconds();
        let mut acc: BTreeMap<&'static str, (f64, f64, f64, f64)> = BTreeMap::new();
        let mut changes: Vec<PriceChange> = Vec::new();
        let mut unpriced = 0.0;
        let mut last_source = String::new();
        for (start, end, mid) in span.pieces(cuts) {
            let secs = end.duration_since(start).as_secs_f64();
            match self.at(mid) {
                None => {
                    unpriced += secs;
                    if last_source != "unpriced" {
                        changes.push(PriceChange { at: start, source: "unpriced".into() });
                        last_source = "unpriced".into();
                    }
                }
                Some(e) => {
                    if e.source != last_source {
                        if changes.len() < 100 {
                            changes.push(PriceChange { at: start, source: e.source.clone() });
                        }
                        last_source = e.source.clone();
                    }
                    for (name, v) in e.rates.named() {
                        let a = acc.entry(name).or_insert((f64::INFINITY, f64::NEG_INFINITY, 0.0, 0.0));
                        a.0 = a.0.min(v);
                        a.1 = a.1.max(v);
                        a.2 += v * secs;
                        a.3 += secs;
                    }
                }
            }
        }
        PriceSummary {
            from: span.from,
            to: span.to,
            currency: self.currency.clone(),
            free: self.free,
            rates: acc
                .into_iter()
                .map(|(k, (min, max, sum, secs))| (k.to_string(), RateRange { min, max, avg: sum / secs }))
                .collect(),
            changes,
            unpriced_share: if total > 0.0 { unpriced / total } else { 0.0 },
        }
    }
}

enum Variant<'a> {
    Override(&'a PriceOverride),
    Window(&'a PriceWindow),
    Base,
}

/// The text of a price note, checked like any other plain text.
pub(crate) fn validate_notes(pricing: &Pricing, p: &mut Problems, path: &str) {
    for (i, per) in pricing.periods.iter().enumerate() {
        if let Some(n) = &per.note {
            validate::text_len(p, &format!("{path}.periods[{i}].note"), n, limits::NOTE_CHARS);
        }
        for (j, o) in per.overrides.iter().enumerate() {
            if let Some(r) = &o.reason {
                validate::text_len(p, &format!("{path}.periods[{i}].overrides[{j}].reason"), r, limits::NOTE_CHARS);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn sample() -> Pricing {
        serde_json::from_value(json!({
            "currency": "EUR",
            "timezone": "Europe/Sofia",
            "periods": [
                {
                    "valid_from": "2026-01-01T00:00:00Z",
                    "valid_to": "2026-11-10T00:00:00Z",
                    "rates": {"input": 2.0, "output": 8.0},
                    "windows": [{"name": "off-peak", "from_time": "22:00", "to_time": "06:00", "rates": {"input": 1.0}}],
                    "overrides": [{"from": "2026-11-05T00:00:00Z", "to": "2026-11-06T00:00:00Z", "rates": {"input": 0.5, "output": 2.0}, "reason": "launch week"}]
                },
                {"valid_from": "2026-11-10T00:00:00Z", "rates": {"input": 3.0, "output": 9.0}}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn the_effective_price_follows_periods_windows_and_overrides() {
        let p = sample();
        let mut probs = Problems::new();
        p.validate(&mut probs, "pricing");
        assert!(probs.is_empty(), "{probs}");

        // Monday 12:00 local (10:00Z): base rates.
        let e = p.at(ts("2026-11-02T10:00:00Z")).unwrap();
        assert_eq!((e.rates.input, e.rates.output, e.source.as_str()), (Some(2.0), Some(8.0), "base"));
        // 23:00 local (21:00Z): the off-peak window lowers input only.
        let e = p.at(ts("2026-11-02T21:00:00Z")).unwrap();
        assert_eq!((e.rates.input, e.rates.output, e.source.as_str()), (Some(1.0), Some(8.0), "window:off-peak"));
        // The override wins over the window.
        let e = p.at(ts("2026-11-05T21:00:00Z")).unwrap();
        assert_eq!((e.rates.input, e.source.as_str()), (Some(0.5), "override"));
        // The next period.
        assert_eq!(p.rate_at("input", ts("2026-11-12T10:00:00Z")), Some(3.0));
        // Before the first period: unpriced.
        assert!(p.at(ts("2025-06-01T00:00:00Z")).is_none());
    }

    #[test]
    fn a_span_reports_min_max_and_time_weighted_mean() {
        let p = sample();
        // A whole Monday in Sofia: 8 h of off-peak (22–06) at 1.0, 16 h at 2.0.
        let s = Span::new(ts("2026-11-01T22:00:00Z"), ts("2026-11-02T22:00:00Z")).unwrap();
        let sum = p.during(s);
        let input = sum.rates["input"];
        assert_eq!((input.min, input.max), (1.0, 2.0));
        assert!((input.avg - (8.0 * 1.0 + 16.0 * 2.0) / 24.0).abs() < 1e-9, "{}", input.avg);
        assert_eq!(sum.rates["output"].avg, 8.0);
        assert_eq!(sum.unpriced_share, 0.0);
        assert!(sum.changes.len() >= 2);
    }

    #[test]
    fn overlapping_or_unordered_periods_are_refused() {
        let p: Pricing = serde_json::from_value(json!({
            "periods": [
                {"valid_from": "2026-01-01T00:00:00Z", "valid_to": "2026-03-01T00:00:00Z", "rates": {"input": 1.0}},
                {"valid_from": "2026-02-01T00:00:00Z", "rates": {"input": -1.0}}
            ]
        }))
        .unwrap();
        let mut probs = Problems::new();
        p.validate(&mut probs, "pricing");
        let all = probs.to_string();
        assert!(all.contains("overlaps"), "{all}");
        assert!(all.contains("periods[1].rates.input: a number of zero or more"), "{all}");
    }

    #[test]
    fn started_periods_cannot_be_rewritten() {
        let old = sample();
        let now = ts("2026-11-03T00:00:00Z");
        let mut changed = old.clone();
        changed.periods[0].rates.input = Some(1.5);
        let mut probs = Problems::new();
        changed.check_not_retroactive(&old, now, &mut probs, "pricing");
        assert!(probs.to_string().contains("never retroactive"), "{probs}");

        // Ending the running period early and adding one is fine.
        let mut ended = old.clone();
        ended.periods[0].valid_to = Some(ts("2026-11-07T00:00:00Z"));
        ended.periods.insert(
            1,
            PricePeriod {
                valid_from: ts("2026-11-07T00:00:00Z"),
                valid_to: Some(ts("2026-11-10T00:00:00Z")),
                ..old.periods[1].clone()
            },
        );
        let mut probs = Problems::new();
        ended.check_not_retroactive(&old, now, &mut probs, "pricing");
        ended.validate(&mut probs, "pricing");
        assert!(probs.is_empty(), "{probs}");
    }

    #[test]
    fn free_resources_need_no_period() {
        let p: Pricing = serde_json::from_value(json!({"free": true})).unwrap();
        let mut probs = Problems::new();
        p.validate(&mut probs, "pricing");
        assert!(probs.is_empty(), "{probs}");
        assert_eq!(p.rate_at("input", ts("2026-11-03T00:00:00Z")), Some(0.0));
        assert!(p.at(ts("2026-11-03T00:00:00Z")).unwrap().free);
    }
}
