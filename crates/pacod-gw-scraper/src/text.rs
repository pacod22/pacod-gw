//! Fingerprints of a page's text. The check reads a page with the extraction model only when
//! its fingerprint changed, and a fingerprint over the whole text would change every day: a
//! date, a banner, a counter. So it is taken over the lines that carry what the tracker reads:
//! every table row, and every line with a model the provider has, an amount of money, a quantity
//! of tokens, a percentage, or a word that prices or capabilities are written with. A change to
//! a context window alone changes it, as does one to a price or to off-peak hours.

/// Words of the lines that state prices, hours and what a model takes.
const SIGNALS: &[&str] = &[
    "peak",
    "discount",
    "batch",
    "cache",
    "context",
    "input",
    "output",
    "image",
    "vision",
    "audio",
    "video",
    "pdf",
    "document",
    "function",
    "tool",
    "json",
    "structured",
    "reasoning",
    "thinking",
    "modalit",
];

use ring::digest::{Context, SHA256, digest};

/// SHA-256 of a text, in hex.
pub fn sha256(text: &str) -> String {
    hex::encode(digest(&SHA256, text.as_bytes()))
}

/// The fingerprint of a page's text for the provider's models (`known_models`, the provider's
/// ids). White space within a line does not count.
pub fn fingerprint(text: &str, known_models: &[String]) -> String {
    // A page writes an id as it is or with spaces (`deepseek flash`).
    let ids: Vec<String> = known_models
        .iter()
        .map(|m| m.trim().to_ascii_lowercase())
        .filter(|m| m.len() > 1)
        .flat_map(|m| [m.replace(['-', '_'], " "), m])
        .collect();
    let mut ctx = Context::new(&SHA256);
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if line.contains(" | ")
            || has_amount(&line)
            || has_quantity(&lower)
            || has_percent(&line)
            || ids.iter().any(|id| lower.contains(id))
            || SIGNALS.iter().any(|w| lower.contains(w))
        {
            ctx.update(line.as_bytes());
            ctx.update(b"\n");
        }
    }
    hex::encode(ctx.finish())
}

/// Whether a line states an amount of money: a currency sign next to a digit (`$2.00`, `¥ 8`),
/// or a figure followed by a currency code (`2.00 USD`).
pub fn has_amount(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let sign = |c: char| matches!(c, '$' | '€' | '£' | '¥' | '￥');
    let digit_near = |i: usize| {
        chars.get(i).is_some_and(char::is_ascii_digit)
            || (chars.get(i) == Some(&' ') && chars.get(i + 1).is_some_and(char::is_ascii_digit))
    };
    if chars.iter().enumerate().any(|(i, c)| sign(*c) && digit_near(i + 1)) {
        return true;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    words.windows(2).any(|w| {
        w[0].chars().next().is_some_and(|c| c.is_ascii_digit()) && matches!(w[1], "USD" | "EUR" | "CNY" | "GBP" | "JPY")
    })
}

/// Whether a line states a percentage: `50 %`, `50%`.
fn has_percent(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    chars.iter().enumerate().any(|(i, c)| {
        *c == '%'
            && (i.checked_sub(1).and_then(|j| chars.get(j)).is_some_and(char::is_ascii_digit)
                || (i >= 2 && chars[i - 1] == ' ' && chars[i - 2].is_ascii_digit()))
    })
}

/// Whether a line states a quantity of tokens: `128k`, `1M`, `200,000 tokens`, `per million`.
pub fn has_quantity(lower: &str) -> bool {
    if lower.contains("tokens") || lower.contains("per million") || lower.contains("/ 1m") || lower.contains("/1m") {
        return true;
    }
    lower.split(|c: char| !c.is_ascii_alphanumeric() && c != '.').any(|word| {
        let Some(number) = word.strip_suffix('k').or_else(|| word.strip_suffix('m')) else { return false };
        !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit() || c == '.')
            && number.chars().any(|c| c.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "Pricing\nLast updated 5 November 2026. 1,204 people read this today.\n\
                        Model | Context | Input | Output\n\
                        deepseek-flash | 128K | $0.30 / 1M tokens | $1.20 / 1M tokens\n\
                        Off-peak: 50 % off between 16:30 and 00:30 UTC\n\
                        DeepSeek Flash takes images.\n";

    fn known() -> Vec<String> {
        vec!["deepseek-flash".into()]
    }

    #[test]
    fn a_date_or_a_counter_does_not_change_the_fingerprint() {
        let other_day = PAGE.replace("5 November 2026. 1,204", "6 November 2026. 1,377");
        assert_eq!(fingerprint(PAGE, &known()), fingerprint(&other_day, &known()));
        assert_ne!(sha256(PAGE), sha256(&other_day), "the whole text did change");
        let spaced = PAGE.replace("deepseek-flash | 128K", "deepseek-flash   |   128K");
        assert_eq!(fingerprint(PAGE, &known()), fingerprint(&spaced, &known()), "white space does not count");
    }

    #[test]
    fn a_price_a_window_or_a_capability_changes_it() {
        let base = fingerprint(PAGE, &known());
        for changed in [
            PAGE.replace("$0.30", "$0.25"),
            PAGE.replace("128K", "256K"),
            PAGE.replace("takes images", "takes images and audio"),
            PAGE.replace("16:30 and 00:30", "17:00 and 01:00"),
            format!("{PAGE}Batch: 2.00 USD per run\n"),
            format!("{PAGE}Up to 64m tokens a day\n"),
        ] {
            assert_ne!(fingerprint(&changed, &known()), base, "{changed}");
        }
    }

    #[test]
    fn amounts_and_quantities_are_told_from_other_text() {
        assert!(has_amount("from $2.00") && has_amount("¥ 8 per million") && has_amount("2.50 USD"));
        assert!(!has_amount("costs $$ nothing") && !has_amount("2026 USD-pegged"));
        assert!(has_quantity("128k context") && has_quantity("1m") && has_quantity("2.5m tokens"));
        assert!(!has_quantity("k-means and m-dash") && !has_quantity("released in 2026"));
        assert!(has_percent("50 % off") && has_percent("save 50%") && !has_percent("100 percent"));
    }
}
