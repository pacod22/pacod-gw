//! The pages the price tracker reads: how their addresses are compared.
//!
//! The core stores a page under its address and refuses a second page at the same one; the
//! scraper reports the pages it found by address. Both must write an address the same way, or
//! a page found again would be proposed again.

/// Query parameters that only say where a visitor came from.
const TRACKING: &[&str] = &["utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content", "ref", "gclid"];

/// An address in the one form pages are stored and compared in: `http` or `https`, the host in
/// lower case, no default port, no fragment, no tracking parameters, and no slash at the end of
/// a path other than `/`.
pub fn normalise_url(raw: &str) -> Result<String, String> {
    let mut url = url::Url::parse(raw.trim()).map_err(|e| format!("\"{raw}\" is not a URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("\"{raw}\" is not an http or https address"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(format!("\"{raw}\" has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("\"{raw}\" carries a user name or password"));
    }
    url.set_fragment(None);
    let kept: Vec<(String, String)> =
        url.query_pairs().filter(|(k, _)| !TRACKING.contains(&k.as_ref())).map(|(k, v)| (k.into(), v.into())).collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
    let path = url.path().to_owned();
    if path.len() > 1 && path.ends_with('/') {
        url.set_path(path.trim_end_matches('/'));
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_page_written_otherwise_is_one_address() {
        let one = normalise_url("https://api-docs.deepseek.com/quick_start/pricing").unwrap();
        for same in [
            "https://API-DOCS.deepseek.com/quick_start/pricing/",
            "https://api-docs.deepseek.com:443/quick_start/pricing#prices",
            "https://api-docs.deepseek.com/quick_start/pricing?utm_source=news&ref=x",
            "  https://api-docs.deepseek.com/quick_start/pricing  ",
        ] {
            assert_eq!(normalise_url(same).unwrap(), one, "{same}");
        }
        assert_eq!(normalise_url("https://docs.x.ai").unwrap(), "https://docs.x.ai/");
        // A query that selects content stays.
        assert_eq!(
            normalise_url("https://example.com/pricing?tab=batch&utm_medium=x").unwrap(),
            "https://example.com/pricing?tab=batch"
        );
        for bad in ["ftp://example.com/prices", "not a url", "https://user:secret@example.com/pricing", "file:///etc"] {
            assert!(normalise_url(bad).is_err(), "{bad}");
        }
    }
}
