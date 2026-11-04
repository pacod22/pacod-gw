//! Identifiers.
//!
//! Providers and resources get ids derived from who they are, not random ones: a provider that
//! re-announces after a restart, or after the broker restarted, gets the same ids again, so its
//! reputation and every report about it stay attached. Leases, tickets and evidence records get
//! random ids.

use ring::digest::{SHA256, digest};

/// The id of the provider a principal announces as, on the broker `origin`.
///
/// `principal` is the pacod-id subject in `oidc` mode and `anon:<name>` in `open` mode.
pub fn provider_id(origin: &str, principal: &str) -> String {
    format!("prv_{}", short_hash(&[origin, principal]))
}

/// The id of the resource a provider announces under `key`.
pub fn resource_id(provider_id: &str, key: &str) -> String {
    format!("res_{}", short_hash(&[provider_id, key]))
}

/// The first 16 bytes of SHA-256 over the parts, each followed by a zero byte so that
/// `("ab","c")` and `("a","bc")` differ.
fn short_hash(parts: &[&str]) -> String {
    let mut input = Vec::with_capacity(parts.iter().map(|p| p.len() + 1).sum());
    for p in parts {
        input.extend_from_slice(p.as_bytes());
        input.push(0);
    }
    hex::encode(&digest(&SHA256, &input).as_ref()[..16])
}

/// Whether `s` is a slug: lower-case ASCII letters, digits and `-`, `_`, `.`, `:`, `/`, starting
/// with a letter or digit. Keys, names, kinds, protocols and tags are slugs.
pub fn is_slug(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_distinct() {
        let p = provider_id("b1", "sub-1");
        assert_eq!(p, provider_id("b1", "sub-1"));
        assert_ne!(p, provider_id("b2", "sub-1"));
        assert!(p.starts_with("prv_") && p.len() == 36);
        assert_ne!(resource_id(&p, "ab"), resource_id(&p, "a"));
        assert_ne!(short_hash(&["ab", "c"]), short_hash(&["a", "bc"]));
    }

    #[test]
    fn slugs() {
        assert!(is_slug("gpt-4o", 96));
        assert!(is_slug("other:weather/v2", 96));
        assert!(!is_slug("Upper", 96));
        assert!(!is_slug("-lead", 96));
        assert!(!is_slug("", 96));
        assert!(!is_slug("with space", 96));
    }
}
