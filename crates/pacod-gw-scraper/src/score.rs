//! How strongly a page looks like one that lists a provider's models and prices, and which of
//! a site's addresses are worth opening in the browser.

use crate::text::{has_amount, has_quantity};

/// Second-level labels under which a registrable name has three labels (`example.co.uk`).
const SECOND_LEVEL: &[&str] = &["co", "com", "net", "org", "gov", "ac", "edu"];

/// The site a host belongs to: its registrable name, approximated by its last two labels, or
/// three under a country's second level (`co.uk`, `com.cn`). An address is its own site.
pub fn site_of(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    let keep = if n >= 3 && labels[n - 1].len() == 2 && SECOND_LEVEL.contains(&labels[n - 2]) { 3 } else { 2 };
    labels[n.saturating_sub(keep)..].join(".")
}

/// Addresses that are files, not pages.
const NOT_PAGES: &[&str] = &[
    ".png",
    ".jpg",
    ".jpeg",
    ".gif",
    ".svg",
    ".webp",
    ".ico",
    ".pdf",
    ".zip",
    ".gz",
    ".css",
    ".js",
    ".json",
    ".xml",
    ".txt",
    ".mp4",
    ".mp3",
    ".woff",
    ".woff2",
    ".md",
    ".markdown",
    ".rss",
    ".atom",
];

/// Language sections of a site (`/zh-cn/…`): a translation of a page the scan finds in English
/// says nothing more, and would be proposed beside it.
const TRANSLATIONS: &[&str] = &[
    "zh", "zh-cn", "zh-tw", "zh-hans", "zh-hant", "ja", "ja-jp", "ko", "ko-kr", "de", "fr", "es", "pt", "pt-br", "it",
    "ru", "ar", "hi", "id", "vi", "th", "tr", "nl", "pl",
];

/// How many of `keywords` an address's path holds, `None` for an address that is a file or a
/// translation.
pub fn keyword_hits(url: &url::Url, keywords: &[String]) -> Option<usize> {
    let path = url.path().to_ascii_lowercase();
    let first = path.trim_start_matches('/').split('/').next().unwrap_or_default();
    if NOT_PAGES.iter().any(|ext| path.ends_with(ext)) || TRANSLATIONS.contains(&first) {
        return None;
    }
    Some(keywords.iter().filter(|k| path.contains(&k.to_ascii_lowercase())).count())
}

/// Lines that state a price for a quantity: an amount with tokens, a million, or a unit
/// (`$2.00 / 1M tokens`, `$0.04 per image`).
pub fn price_lines(text: &str) -> Vec<&str> {
    const UNITS: &[&str] = &["mtok", "token", "image", "second", "minute", "request", "character", "search unit", "1k"];
    text.lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            has_amount(line) && (has_quantity(&lower) || UNITS.iter().any(|u| lower.contains(u)))
        })
        .collect()
}

/// The repository's models a page names, by the provider's ids, written as they are or with
/// spaces for hyphens.
pub fn model_hits(text: &str, known_models: &[String]) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    known_models
        .iter()
        .filter(|m| {
            let id = m.to_ascii_lowercase();
            id.len() > 1 && (lower.contains(&id) || lower.contains(&id.replace(['-', '_'], " ")))
        })
        .cloned()
        .collect()
}

/// A page's score: prices for a quantity count 2 each (at most 20 of them), models the
/// repository knows 3 each (at most 10), keywords in the address or the title 4 each (at most 3).
pub fn score(
    url: &url::Url,
    title: &str,
    text: &str,
    known_models: &[String],
    keywords: &[String],
) -> (f64, u32, Vec<String>) {
    let prices = price_lines(text).len().min(20);
    let models = model_hits(text, known_models);
    let place = format!("{} {}", url.path(), title).to_ascii_lowercase();
    let words = keywords.iter().filter(|k| place.contains(&k.to_ascii_lowercase())).count().min(3);
    let score = 2.0 * prices as f64 + 3.0 * models.len().min(10) as f64 + 4.0 * words as f64;
    (score, prices as u32, models)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keywords() -> Vec<String> {
        ["pricing", "price", "models", "rates", "billing"].map(String::from).to_vec()
    }

    #[test]
    fn a_host_belongs_to_its_registrable_site() {
        assert_eq!(site_of("api-docs.deepseek.com"), "deepseek.com");
        assert_eq!(site_of("platform.claude.com"), "claude.com");
        assert_eq!(site_of("docs.example.co.uk"), "example.co.uk");
        assert_eq!(site_of("open.bigmodel.cn"), "bigmodel.cn");
        assert_eq!(site_of("127.0.0.1"), "127.0.0.1");
        assert_eq!(site_of("localhost"), "localhost");
    }

    #[test]
    fn a_pricing_page_scores_above_a_news_page() {
        let known = vec!["deepseek-flash".to_owned(), "deepseek-v4-pro".to_owned()];
        let pricing = "Model | deepseek-flash | deepseek-v4-pro\n1M INPUT TOKENS | $0.30 | $1.32\n1M OUTPUT TOKENS | $1.20 | $3.96\n";
        let news = "DeepSeek released a new app on 5 November.\nRead more.";
        let u = |p: &str| url::Url::parse(&format!("https://api-docs.deepseek.com{p}")).unwrap();
        let (high, prices, models) =
            score(&u("/quick_start/pricing"), "Models & Pricing", pricing, &known, &keywords());
        assert_eq!((prices, models.len()), (2, 2));
        // Two keywords: pricing, models ("pricing" does not hold "price").
        assert_eq!(high, 2.0 * 2.0 + 3.0 * 2.0 + 4.0 * 2.0);
        let (low, ..) = score(&u("/news/2026-11-05"), "News", news, &known, &keywords());
        assert!(low < 10.0 && high > 10.0, "{low} {high}");
    }

    #[test]
    fn files_are_not_pages_and_keywords_count() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert_eq!(keyword_hits(&u("https://x.example/docs/pricing"), &keywords()), Some(1));
        assert_eq!(keyword_hits(&u("https://x.example/models/price-list"), &keywords()), Some(2));
        assert_eq!(keyword_hits(&u("https://x.example/logo.png"), &keywords()), None);
        assert_eq!(keyword_hits(&u("https://x.example/about"), &keywords()), Some(0));
        assert_eq!(keyword_hits(&u("https://x.example/quick_start/pricing.md"), &keywords()), None);
        assert_eq!(keyword_hits(&u("https://x.example/zh-cn/quick_start/pricing"), &keywords()), None);
        assert_eq!(keyword_hits(&u("https://x.example/docs/en/pricing"), &keywords()), Some(1), "English stays");
        assert_eq!(price_lines("gpt-x $2.00 / 1M tokens\nTotal $5\n$0.04 per image").len(), 2);
    }
}
