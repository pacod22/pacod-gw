//! Password hashing for console logins. A slow hash is right here because logins are rare;
//! it is never used on the data plane.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use base64::Engine;

use super::random_bytes;

pub fn hash(password: &str) -> anyhow::Result<String> {
    let salt_bytes: [u8; 16] = random_bytes();
    let salt = SaltString::from_b64(&base64::engine::general_purpose::STANDARD_NO_PAD.encode(salt_bytes))
        .map_err(|e| anyhow::anyhow!("salt: {e}"))?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing failed: {e}"))?
        .to_string())
}

pub fn verify(password: &str, stored: &str) -> bool {
    match PasswordHash::new(stored) {
        Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_the_right_password_only() {
        let h = hash("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify("correct horse", &h));
        assert!(!verify("wrong", &h));
        assert!(!verify("correct horse", "not a hash"));
        assert_ne!(h, hash("correct horse").unwrap(), "each hash has its own salt");
    }
}
