//! Cost of a call from its usage and a price version (concept section 13.2).

use pacod_gw_events::Usage;

use crate::model::{Price, Rates};

/// The result of pricing a call.
#[derive(Debug, Clone, PartialEq)]
pub struct Costed {
    /// `None` when a rate the call needs is missing.
    pub amount: Option<f64>,
    pub currency: String,
    pub price_version: u32,
    /// The variant whose rates were used; empty for the standard rates.
    pub variant: String,
    /// True when the call used something the price has no rate for.
    pub price_missing: bool,
}

impl Costed {
    /// A call on a model that has no price at all.
    pub fn unpriced() -> Self {
        Self { amount: None, currency: String::new(), price_version: 0, variant: String::new(), price_missing: true }
    }

    /// A call on a model marked free.
    pub fn free(currency: &str) -> Self {
        Self {
            amount: Some(0.0),
            currency: currency.to_owned(),
            price_version: 0,
            variant: String::new(),
            price_missing: false,
        }
    }
}

/// Prices `usage` with `price`. `variant` selects separate rates (for example `batch`) when
/// the price has them; an unknown variant uses the standard rates.
pub fn cost(price: &Price, usage: &Usage, variant: &str) -> Costed {
    let (rates, variant): (&Rates, &str) = match price.variants.get(variant) {
        Some(r) if !variant.is_empty() => (r, variant),
        _ => (&price.rates, ""),
    };
    // A long prompt: the highest tier it exceeds, for the standard rates or this variant.
    let prompt = usage.prompt_total();
    let tier = price.tiers.iter().filter(|t| prompt > t.above_input_tokens).max_by_key(|t| t.above_input_tokens);
    let tiered;
    let rates = match tier.and_then(|t| if variant.is_empty() { Some(&t.rates) } else { t.variants.get(variant) }) {
        Some(over) => {
            let mut r = rates.clone();
            r.overlay(over);
            tiered = r;
            &tiered
        }
        None => rates,
    };
    let mut missing = false;
    // Token rates are per million. The products are added first and divided once, so that
    // the result carries no more rounding error than a single division.
    let mut per_million = 0.0;
    let mut total = 0.0;

    // Tokens: count times rate per million. A count without a rate makes the price incomplete.
    let mut tokens = |count: u64, rate: Option<f64>| {
        if count == 0 {
            return;
        }
        match rate {
            Some(r) => per_million += count as f64 * r,
            None => missing = true,
        }
    };

    tokens(usage.input_tokens, rates.input);
    // A cache read without its own rate is charged as input: it never costs more than that.
    tokens(usage.cache_read_tokens, rates.cache_read.or(rates.input));

    // Cache writes: by lifetime where the provider reports the split, otherwise at the
    // five-minute rate, which is the default lifetime.
    let split = usage.cache_write_5m_tokens + usage.cache_write_1h_tokens;
    let write_5m = rates.cache_write_5m.or(rates.input);
    if split > 0 {
        tokens(usage.cache_write_5m_tokens, write_5m);
        tokens(usage.cache_write_1h_tokens, rates.cache_write_1h.or(write_5m));
        tokens(usage.cache_write_tokens.saturating_sub(split), write_5m);
    } else {
        tokens(usage.cache_write_tokens, write_5m);
    }

    // Reasoning tokens are part of the output tokens. They are priced apart only where the
    // price has a reasoning rate.
    match rates.reasoning {
        Some(r) if usage.reasoning_tokens > 0 => {
            tokens(usage.output_tokens.saturating_sub(usage.reasoning_tokens), rates.output);
            tokens(usage.reasoning_tokens, Some(r));
        }
        _ => tokens(usage.output_tokens, rates.output),
    }

    let mut units = |count: u64, rate: Option<f64>| {
        if count == 0 {
            return;
        }
        match rate {
            Some(r) => total += count as f64 * r,
            None => missing = true,
        }
    };
    units(usage.characters, rates.per_character);
    units(u64::from(usage.server_tool_calls), rates.per_server_tool_call);
    if let Some(r) = rates.per_request {
        total += r;
    }
    // Twelve decimals are far below any price and remove the last binary artefacts.
    let total = ((total + per_million / 1_000_000.0) * 1e12).round() / 1e12;

    Costed {
        amount: (!missing).then_some(total),
        currency: price.currency.clone(),
        price_version: price.version,
        variant: variant.to_owned(),
        price_missing: missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PriceSource, PriceStatus};
    use chrono::Utc;

    fn price(rates: Rates) -> Price {
        Price {
            model_id: "mdl_1".into(),
            version: 37,
            currency: "USD".into(),
            rates,
            variants: Default::default(),
            tiers: Vec::new(),
            windows: Vec::new(),
            effective_from: Utc::now(),
            source: PriceSource::Manual,
            status: PriceStatus::Active,
            evidence_url: String::new(),
            evidence_text: String::new(),
            confirmed_at: None,
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn concept_worked_example() {
        // Claude Sonnet 5.5: 2.00 input, 10.00 output, 0.20 cache read, USD per 1M.
        let p = price(Rates { input: Some(2.0), output: Some(10.0), cache_read: Some(0.2), ..Default::default() });
        let u = Usage { input_tokens: 812, cache_read_tokens: 6100, output_tokens: 420, ..Default::default() };
        let c = cost(&p, &u, "");
        assert_eq!(c.amount, Some(0.007044), "whole token counts give an exact figure");
        assert_eq!((c.price_version, c.currency.as_str(), c.price_missing), (37, "USD", false));
        // Without a cache-read rate the cached tokens cost as much as input.
        let no_cache = price(Rates { input: Some(2.0), output: Some(10.0), ..Default::default() });
        assert!(close(cost(&no_cache, &u, "").amount.unwrap(), 0.018024));
    }

    #[test]
    fn cache_writes_by_lifetime() {
        let p = price(Rates {
            input: Some(2.0),
            output: Some(10.0),
            cache_write_5m: Some(2.5),
            cache_write_1h: Some(4.0),
            ..Default::default()
        });
        let u = Usage {
            cache_write_tokens: 300,
            cache_write_5m_tokens: 100,
            cache_write_1h_tokens: 200,
            ..Default::default()
        };
        assert!(close(cost(&p, &u, "").amount.unwrap(), (100.0 * 2.5 + 200.0 * 4.0) / 1e6));
        // No split reported: everything at the five-minute rate.
        let u = Usage { cache_write_tokens: 300, ..Default::default() };
        assert!(close(cost(&p, &u, "").amount.unwrap(), 300.0 * 2.5 / 1e6));
    }

    #[test]
    fn reasoning_is_output_unless_priced_apart() {
        let u = Usage { output_tokens: 1000, reasoning_tokens: 400, ..Default::default() };
        let same = price(Rates { output: Some(10.0), ..Default::default() });
        assert!(close(cost(&same, &u, "").amount.unwrap(), 1000.0 * 10.0 / 1e6));
        let apart = price(Rates { output: Some(10.0), reasoning: Some(20.0), ..Default::default() });
        assert!(close(cost(&apart, &u, "").amount.unwrap(), (600.0 * 10.0 + 400.0 * 20.0) / 1e6));
    }

    #[test]
    fn units_speech_and_server_tools() {
        let p = price(Rates {
            per_character: Some(0.000015),
            per_server_tool_call: Some(0.01),
            per_request: Some(0.001),
            ..Default::default()
        });
        let u = Usage { characters: 2000, server_tool_calls: 3, ..Default::default() };
        assert!(close(cost(&p, &u, "").amount.unwrap(), 2000.0 * 0.000015 + 3.0 * 0.01 + 0.001));
    }

    #[test]
    fn a_missing_rate_is_flagged_never_guessed() {
        let p = price(Rates { input: Some(2.0), ..Default::default() });
        let u = Usage { input_tokens: 10, output_tokens: 5, ..Default::default() };
        let c = cost(&p, &u, "");
        assert_eq!(c.amount, None);
        assert!(c.price_missing);
        // Nothing used that lacks a rate: fine.
        let c = cost(&p, &Usage { input_tokens: 10, ..Default::default() }, "");
        assert!(!c.price_missing && c.amount.is_some());
        assert!(Costed::unpriced().price_missing);
        assert_eq!(Costed::free("USD").amount, Some(0.0));
    }

    #[test]
    fn variants_have_their_own_rates() {
        let mut p = price(Rates { input: Some(2.0), output: Some(10.0), ..Default::default() });
        p.variants.insert("batch".into(), Rates { input: Some(1.0), output: Some(5.0), ..Default::default() });
        let u = Usage { input_tokens: 1_000_000, output_tokens: 1_000_000, ..Default::default() };
        let c = cost(&p, &u, "batch");
        assert!(close(c.amount.unwrap(), 6.0));
        assert_eq!(c.variant, "batch");
        let c = cost(&p, &u, "unknown");
        assert!(close(c.amount.unwrap(), 12.0));
        assert_eq!(c.variant, "");
    }

    #[test]
    fn a_long_prompt_takes_its_tier_and_a_variant_its_own() {
        let mut p = price(Rates { input: Some(2.0), output: Some(12.0), cache_read: Some(0.2), ..Default::default() });
        p.variants.insert("priority".into(), Rates { input: Some(3.6), output: Some(21.6), ..Default::default() });
        p.variants.insert("flex".into(), Rates { input: Some(1.0), output: Some(6.0), ..Default::default() });
        let mut above = crate::model::PriceTier {
            above_input_tokens: 200_000,
            rates: Rates { input: Some(4.0), output: Some(18.0), cache_read: Some(0.4), ..Default::default() },
            ..Default::default()
        };
        above.variants.insert("priority".into(), Rates { input: Some(7.2), output: Some(32.4), ..Default::default() });
        p.tiers.push(above);
        let usage = |input: u64, cached: u64| Usage {
            input_tokens: input,
            cache_read_tokens: cached,
            output_tokens: 1_000_000,
            ..Default::default()
        };
        // At the threshold: not above it.
        let c = cost(&p, &usage(150_000, 50_000), "");
        assert_eq!(c.amount, Some(0.3 + 0.01 + 12.0));
        // Above it, counting the cached tokens too: the whole call at the long-context rates.
        let c = cost(&p, &usage(150_000, 50_001), "");
        assert_eq!(c.amount, Some(0.6 + 0.0200004 + 18.0));
        assert_eq!(cost(&p, &usage(300_000, 0), "priority").amount, Some(2.16 + 32.4));
        // A variant without long-context rates keeps its own.
        assert_eq!(cost(&p, &usage(300_000, 0), "flex").amount, Some(0.3 + 6.0));
    }

    #[test]
    fn windows_pick_a_variant_by_the_clock() {
        use chrono::TimeZone;
        let mut p = price(Rates { input: Some(0.3), output: Some(1.2), ..Default::default() });
        p.variants.insert("off_peak".into(), Rates { input: Some(0.15), output: Some(0.6), ..Default::default() });
        // DeepSeek: off-peak outside 01-04 and 06-10 UTC on weekdays, and all weekend.
        p.windows.push(crate::model::PriceWindow {
            variant: "off_peak".into(),
            weekdays: vec![1, 2, 3, 4, 5],
            hours_utc: vec!["00:00-01:00".into(), "04:00-06:00".into(), "10:00-00:00".into()],
        });
        p.windows.push(crate::model::PriceWindow {
            variant: "off_peak".into(),
            weekdays: vec![6, 7],
            hours_utc: vec!["00:00-00:00".into()],
        });
        let at = |d: u32, h: u32, m: u32| Utc.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap();
        // 2026-10-12 is a Monday.
        assert_eq!(p.window_variant(at(12, 0, 59)), Some("off_peak"));
        assert_eq!(p.window_variant(at(12, 1, 0)), None, "peak starts at 01:00");
        assert_eq!(p.window_variant(at(12, 3, 59)), None);
        assert_eq!(p.window_variant(at(12, 4, 0)), Some("off_peak"));
        assert_eq!(p.window_variant(at(12, 9, 59)), None);
        assert_eq!(p.window_variant(at(12, 10, 0)), Some("off_peak"));
        assert_eq!(p.window_variant(at(12, 23, 59)), Some("off_peak"));
        assert_eq!(p.window_variant(at(11, 2, 0)), Some("off_peak"), "a Sunday");
        let malformed =
            crate::model::PriceWindow { variant: "x".into(), weekdays: vec![1], hours_utc: vec!["25:00-26:00".into()] };
        assert!(malformed.spans().is_none());
    }
}
