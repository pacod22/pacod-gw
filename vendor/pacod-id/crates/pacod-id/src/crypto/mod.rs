//! Secrets: envelope encryption under the key-encryption key, HMAC for high-entropy tokens,
//! slow hashes for passwords, and the token formats.

pub mod envelope;
pub mod password;
pub mod tokens;

use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::Zeroizing;

/// Fills a buffer from the operating system's random source.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    SystemRandom::new().fill(&mut b).expect("the system random source is available");
    b
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let d = ring::digest::digest(&ring::digest::SHA256, data);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

/// Constant-time equality for secrets.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// A key check value: the first eight bytes of an HMAC over a fixed text, keyed with the
/// secret. Instances compare it to find one that was given another key; it reveals nothing
/// about the secret itself.
pub fn check_value(secret: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    hex::encode(&hmac::sign(&key, b"pacod-id key check").as_ref()[..8])
}

/// A server-side secret for HMAC hashing of tokens: PID_TOKEN_PEPPER, or a product's pepper
/// for tokens imported from it (PID_LEGACY_PEPPERS).
#[derive(Clone)]
pub struct Pepper(Zeroizing<Vec<u8>>);

impl std::fmt::Debug for Pepper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pepper(..)")
    }
}

impl Pepper {
    pub fn new(bytes: Vec<u8>) -> anyhow::Result<Self> {
        anyhow::ensure!(bytes.len() >= 16, "a pepper must be at least 16 bytes");
        Ok(Self(Zeroizing::new(bytes)))
    }

    pub fn check_value(&self) -> String {
        check_value(&self.0)
    }

    /// HMAC-SHA256 of a token.
    pub fn hash(&self, secret: &str) -> [u8; 32] {
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.0);
        let tag = hmac::sign(&key, secret.as_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(tag.as_ref());
        out
    }

    /// A key derived from the pepper for one purpose (for example the known-device cookie).
    pub fn derive(&self, purpose: &str) -> [u8; 32] {
        self.hash(&format!("pacod-id derive {purpose}"))
    }
}
