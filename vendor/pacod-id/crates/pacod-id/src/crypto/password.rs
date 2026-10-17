//! Password hashes: Argon2id for new ones, and bcrypt for those imported from a product until the
//! next successful sign-in replaces them. Callers run these off the async
//! threads (`tokio::task::spawn_blocking`): a hash takes tens of milliseconds on purpose.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;

use super::random_bytes;

/// Argon2id with the OWASP minimum (m = 19 MiB, t = 2, p = 1); the memory is
/// configurable (PID_ARGON2_M_KIB) and never below this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cost {
    pub m_kib: u32,
}

impl Default for Cost {
    fn default() -> Self {
        Cost { m_kib: 19_456 }
    }
}

impl Cost {
    fn params(self) -> Params {
        Params::new(self.m_kib.max(19_456), 2, 1, None).expect("valid Argon2 parameters")
    }

    fn argon2(self) -> Argon2<'static> {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, self.params())
    }
}

pub fn hash(password: &str, cost: Cost) -> anyhow::Result<String> {
    let salt_bytes: [u8; 16] = random_bytes();
    let salt = SaltString::from_b64(&base64::engine::general_purpose::STANDARD_NO_PAD.encode(salt_bytes))
        .map_err(|e| anyhow::anyhow!("salt: {e}"))?;
    Ok(cost
        .argon2()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing: {e}"))?
        .to_string())
}

/// The outcome of checking a password against a stored hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checked {
    pub ok: bool,
    /// The hash should be replaced with one of the current kind and cost (bcrypt, or Argon2
    /// with less memory than configured).
    pub rehash: bool,
}

pub fn verify(password: &str, stored: &str, cost: Cost) -> Checked {
    if stored.starts_with("$2a$") || stored.starts_with("$2b$") || stored.starts_with("$2y$") {
        let ok = bcrypt::verify(password, stored).unwrap_or(false);
        return Checked { ok, rehash: ok };
    }
    let Ok(parsed) = PasswordHash::new(stored) else { return Checked { ok: false, rehash: false } };
    let ok = Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok();
    let weaker = parsed.algorithm.as_str() != "argon2id"
        || Params::try_from(&parsed).map_or(true, |p| p.m_cost() < cost.params().m_cost() || p.t_cost() < 2);
    Checked { ok, rehash: ok && weaker }
}

/// A hash no password matches, checked for unknown accounts so they take as long as known
/// ones (made once at start, at the configured cost).
pub fn dummy_hash(cost: Cost) -> String {
    let secret: [u8; 32] = random_bytes();
    hash(&hex::encode(secret), cost).expect("hashing a random string")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argon2id_round_trip() {
        let cost = Cost::default();
        let h = hash("correct horse battery", cost).unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert_eq!(verify("correct horse battery", &h, cost), Checked { ok: true, rehash: false });
        assert_eq!(verify("wrong", &h, cost), Checked { ok: false, rehash: false });
        assert_ne!(h, hash("correct horse battery", cost).unwrap(), "each hash has its own salt");
    }

    #[test]
    fn bcrypt_hashes_verify_once_and_ask_to_be_replaced() {
        // What an imported product may store: bcrypt with cost 12 (a lower cost here keeps the test quick).
        let h = bcrypt::hash("imported password", 4).unwrap();
        assert!(h.starts_with("$2b$"));
        assert_eq!(verify("imported password", &h, Cost::default()), Checked { ok: true, rehash: true });
        assert_eq!(verify("nope", &h, Cost::default()), Checked { ok: false, rehash: false });
    }

    #[test]
    fn a_raised_cost_asks_for_a_new_hash() {
        let h = hash("pw", Cost::default()).unwrap();
        assert!(verify("pw", &h, Cost { m_kib: 65_536 }).rehash);
        assert!(!verify("pw", &h, Cost::default()).rehash);
    }

    #[test]
    fn garbage_never_matches() {
        assert!(!verify("x", "not a hash", Cost::default()).ok);
        assert!(!verify("x", "", Cost::default()).ok);
        assert!(!verify("x", &dummy_hash(Cost::default()), Cost::default()).ok);
    }
}
