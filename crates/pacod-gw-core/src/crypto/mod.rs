//! Secrets handling: envelope encryption for provider credentials, HMAC hashing for gateway
//! keys and tokens, and password hashing for console logins.

pub mod envelope;
pub mod keys;
pub mod password;

use ring::rand::{SecureRandom, SystemRandom};

/// Fills a buffer from the operating system's random source.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    SystemRandom::new().fill(&mut b).expect("the system random source is available");
    b
}

/// SHA-256 of `data`.
/// A key check value: the first eight bytes of an HMAC over a fixed text, keyed with the
/// secret. Instances compare it to find one that was given another key or pepper; it reveals
/// nothing about the secret itself.
pub fn check_value(secret: &[u8]) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret);
    hex::encode(&ring::hmac::sign(&key, b"pacod-gw key check").as_ref()[..8])
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
