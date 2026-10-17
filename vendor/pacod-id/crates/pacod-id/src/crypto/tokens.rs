//! Ids and token formats.
//!
//! - Record ids: `<prefix>_<uuid v7>` (usr_, svc_, rb_, …).
//! - Tokens a person or script holds: `<prefix>_<id>_<secret>` (pidr_ refresh tokens, pidt_
//!   service tokens), so a token seen in a log leads to its record without revealing it. The
//!   secret is 43 characters of base62: about 256 bits.
//! - Opaque values (cookies, codes, links): base64url of 32 random bytes.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;

use super::random_bytes;

pub const REFRESH_PREFIX: &str = "pidr";
pub const SERVICE_TOKEN_PREFIX: &str = "pidt";

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `len` characters of base62, each uniform: random bytes of 248 and above are drawn again,
/// since 248 is the largest multiple of 62 a byte holds.
pub fn base62(len: usize) -> String {
    let mut out = String::with_capacity(len);
    while out.len() < len {
        for b in random_bytes::<64>() {
            if b < 248 && out.len() < len {
                out.push(ALPHABET[usize::from(b % 62)] as char);
            }
        }
    }
    out
}

/// A record id: `<prefix>_<uuid v7>`.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7().simple())
}

/// A token for the record `id`: `<prefix>_<id without its prefix>_<secret>`.
pub fn token(prefix: &str, id: &str) -> String {
    let bare = id.split_once('_').map_or(id, |(_, rest)| rest);
    format!("{prefix}_{bare}_{}", base62(43))
}

/// The record id part of a token, without checking the secret.
pub fn id_of<'a>(token: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = token.strip_prefix(prefix)?.strip_prefix('_')?;
    let (id, secret) = rest.split_once('_')?;
    (!id.is_empty() && !secret.is_empty()).then_some(id)
}

/// 32 random bytes, base64url: for cookies, codes and links.
pub fn opaque() -> String {
    B64.encode(random_bytes::<32>())
}

/// A token with its secret part replaced, safe to show in logs and listings.
pub fn redact(token: &str) -> String {
    match token.rsplit_once('_') {
        Some((head, secret)) if secret.len() >= 8 => format!("{head}_…{}", &secret[secret.len() - 4..]),
        _ => "…".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_carry_their_record_id() {
        let id = new_id("grt");
        let t = token(REFRESH_PREFIX, &id);
        assert!(t.starts_with("pidr_"));
        assert_eq!(id_of(&t, REFRESH_PREFIX), id.strip_prefix("grt_"));
        assert_eq!(t.rsplit('_').next().unwrap().len(), 43);
        assert_eq!(id_of("pidr_x", REFRESH_PREFIX), None);
        assert_eq!(id_of(&t, SERVICE_TOKEN_PREFIX), None);
        assert!(redact(&t).ends_with(&t[t.len() - 4..]) && !redact(&t).contains(&t[t.len() - 20..t.len() - 4]));
    }

    #[test]
    fn base62_is_letters_and_digits_and_spread_evenly() {
        let s = base62(62 * 400);
        assert!(s.bytes().all(|b| b.is_ascii_alphanumeric()));
        let mut counts = [0u32; 128];
        for b in s.bytes() {
            counts[usize::from(b)] += 1;
        }
        // 400 expected per character; a biased encoder gives two characters twice that.
        assert!(ALPHABET.iter().all(|c| (250..=560).contains(&counts[usize::from(*c)])), "{counts:?}");
    }

    #[test]
    fn opaque_values_differ() {
        assert_ne!(opaque(), opaque());
        assert_eq!(opaque().len(), 43);
    }
}
