//! Gateway API keys, admin tokens and session tokens (concept section 5.5).
//!
//! A key is high-entropy random data, so it is stored and looked up as an HMAC-SHA256 with a
//! server-side pepper. No slow password hash sits on the request path.

use ring::hmac;
use zeroize::Zeroizing;

use super::random_bytes;

/// Prefix of gateway API keys used on the data plane.
pub const API_KEY_PREFIX: &str = "pgw";
/// Prefix of admin tokens used on the management API.
pub const ADMIN_TOKEN_PREFIX: &str = "pgwa";
/// Prefix of console session tokens.
pub const SESSION_PREFIX: &str = "pgws";

/// The server-side pepper for key hashing.
#[derive(Clone)]
pub struct Pepper(Zeroizing<Vec<u8>>);

impl std::fmt::Debug for Pepper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pepper(..)")
    }
}

impl Pepper {
    pub fn new(bytes: Vec<u8>) -> anyhow::Result<Self> {
        anyhow::ensure!(bytes.len() >= 16, "the key pepper must be at least 16 bytes");
        Ok(Self(Zeroizing::new(bytes)))
    }

    /// HMAC-SHA256 of a key or token. This is what is stored and what the snapshot is keyed by.
    pub fn hash(&self, secret: &str) -> [u8; 32] {
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.0);
        let tag = hmac::sign(&key, secret.as_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(tag.as_ref());
        out
    }
}

/// A freshly generated secret. The full text is shown to the user once and never stored.
pub struct NewSecret {
    /// The complete key, for example `pgw_018f3c…_Zk3…`.
    pub full: String,
}

/// Generates a key of the form `<prefix>_<id>_<secret>`. `id` is the record id without its own
/// prefix, so a key seen in a log can be traced to its record without revealing the secret.
pub fn generate(prefix: &str, id: &str) -> NewSecret {
    let secret: [u8; 32] = random_bytes();
    NewSecret { full: format!("{prefix}_{id}_{}", base62(&secret)) }
}

/// Extracts the record id from a key, without validating the secret.
pub fn id_of<'a>(key: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = key.strip_prefix(prefix)?.strip_prefix('_')?;
    let (id, secret) = rest.split_once('_')?;
    (!id.is_empty() && !secret.is_empty()).then_some(id)
}

/// A key with its secret part replaced, safe to show in logs and listings.
pub fn redact(key: &str) -> String {
    match key.rsplit_once('_') {
        Some((head, secret)) if secret.len() >= 8 => format!("{head}_…{}", &secret[secret.len() - 4..]),
        _ => "…".to_owned(),
    }
}

const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Encodes bytes with letters and digits only, so keys survive copy and paste and shells.
fn base62(bytes: &[u8]) -> String {
    // Six bits at a time: 32 random bytes give 42 characters.
    let mut out = String::with_capacity(bytes.len() * 8 / 6 + 1);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            let idx = ((acc >> bits) & 0x3f) as usize;
            // 64 values onto 62 characters: the two spare values reuse the first two letters.
            out.push(ALPHABET[idx % 62] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pepper() -> Pepper {
        Pepper::new(vec![9u8; 32]).unwrap()
    }

    #[test]
    fn generated_keys_have_the_documented_shape() {
        let k = generate(API_KEY_PREFIX, "018f3c2a").full;
        assert!(k.starts_with("pgw_018f3c2a_"));
        let secret = k.rsplit('_').next().unwrap();
        assert!(secret.len() >= 42, "secret part carries the entropy");
        assert!(secret.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(id_of(&k, API_KEY_PREFIX), Some("018f3c2a"));
        assert_eq!(id_of(&k, ADMIN_TOKEN_PREFIX), None);
        assert_ne!(generate(API_KEY_PREFIX, "a").full, generate(API_KEY_PREFIX, "a").full);
    }

    #[test]
    fn hash_is_stable_and_depends_on_pepper() {
        let p = pepper();
        assert_eq!(p.hash("pgw_a_b"), p.hash("pgw_a_b"));
        assert_ne!(p.hash("pgw_a_b"), p.hash("pgw_a_c"));
        assert_ne!(p.hash("pgw_a_b"), Pepper::new(vec![1u8; 32]).unwrap().hash("pgw_a_b"));
    }

    #[test]
    fn short_pepper_is_refused() {
        assert!(Pepper::new(vec![1u8; 8]).is_err());
    }

    #[test]
    fn redact_hides_the_secret() {
        let k = generate(API_KEY_PREFIX, "id1").full;
        let r = redact(&k);
        assert!(r.starts_with("pgw_id1_…"));
        assert_eq!(r.chars().count(), "pgw_id1_…".chars().count() + 4);
        assert_eq!(redact("nounderscore"), "…");
    }

    #[test]
    fn id_of_rejects_malformed() {
        assert_eq!(id_of("pgw__secret", API_KEY_PREFIX), None);
        assert_eq!(id_of("pgw_id_", API_KEY_PREFIX), None);
        assert_eq!(id_of("sk-ant-xyz", API_KEY_PREFIX), None);
    }
}
