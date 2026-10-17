//! Envelope encryption of signing keys and other secrets.
//!
//! Each secret is encrypted with its own data key (AES-256-GCM). The data key is encrypted
//! with the key-encryption key, which lives outside the database. The record carries the id of
//! the key-encryption key so that it can be rotated.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::random_bytes;

/// The key-encryption key, with its id.
#[derive(Clone)]
pub struct Kek {
    pub id: String,
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for Kek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kek").field("id", &self.id).finish_non_exhaustive()
    }
}

impl Kek {
    pub fn new(id: impl Into<String>, key: [u8; 32]) -> Self {
        Self { id: id.into(), key: Zeroizing::new(key) }
    }

    /// See [`super::check_value`].
    pub fn check_value(&self) -> String {
        super::check_value(self.key.as_slice())
    }

    /// Parses a base64-encoded 32-byte key.
    pub fn from_base64(id: impl Into<String>, encoded: &str) -> anyhow::Result<Self> {
        let raw = Zeroizing::new(B64.decode(encoded.trim())?);
        let key: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("the key-encryption key must be 32 bytes, base64-encoded"))?;
        Ok(Self::new(id, key))
    }
}

/// An encrypted secret as stored in the database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sealed {
    pub kek_id: String,
    /// The data key, encrypted with the key-encryption key (nonce prepended), base64.
    pub wrapped_dek: String,
    /// The secret, encrypted with the data key (nonce prepended), base64.
    pub ciphertext: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    #[error("the secret was encrypted with key-encryption key \"{0}\", which is not configured")]
    UnknownKek(String),
    #[error("the stored secret is malformed or was encrypted with a different key")]
    Decrypt,
}

fn seal_with(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let nonce_bytes: [u8; 12] = random_bytes();
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).expect("32-byte key"));
    let mut buf = plaintext.to_vec();
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce_bytes), Aad::from(aad), &mut buf)
        .expect("sealing in memory cannot fail");
    let mut out = Vec::with_capacity(12 + buf.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&buf);
    out
}

fn open_with(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    if sealed.len() < 12 + AES_256_GCM.tag_len() {
        return Err(EnvelopeError::Decrypt);
    }
    let (nonce, ct) = sealed.split_at(12);
    let nonce: [u8; 12] = nonce.try_into().map_err(|_| EnvelopeError::Decrypt)?;
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).map_err(|_| EnvelopeError::Decrypt)?);
    let mut buf = Zeroizing::new(ct.to_vec());
    let len = key
        .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::from(aad), &mut buf)
        .map_err(|_| EnvelopeError::Decrypt)?
        .len();
    buf.truncate(len);
    Ok(buf)
}

/// Encrypts `plaintext`. `context` binds the ciphertext to its record (for example the
/// signing key's id), so a ciphertext copied onto another record does not decrypt.
pub fn seal(kek: &Kek, context: &str, plaintext: &[u8]) -> Sealed {
    let dek: Zeroizing<[u8; 32]> = Zeroizing::new(random_bytes());
    Sealed {
        kek_id: kek.id.clone(),
        wrapped_dek: B64.encode(seal_with(&kek.key, context.as_bytes(), dek.as_slice())),
        ciphertext: B64.encode(seal_with(&dek, context.as_bytes(), plaintext)),
    }
}

/// Decrypts a secret. `keks` holds the current key-encryption key and any older ones.
pub fn open(keks: &[Kek], context: &str, sealed: &Sealed) -> Result<Zeroizing<Vec<u8>>, EnvelopeError> {
    let kek =
        keks.iter().find(|k| k.id == sealed.kek_id).ok_or_else(|| EnvelopeError::UnknownKek(sealed.kek_id.clone()))?;
    let wrapped = B64.decode(&sealed.wrapped_dek).map_err(|_| EnvelopeError::Decrypt)?;
    let dek_bytes = open_with(&kek.key, context.as_bytes(), &wrapped)?;
    let dek: [u8; 32] = dek_bytes.as_slice().try_into().map_err(|_| EnvelopeError::Decrypt)?;
    let dek = Zeroizing::new(dek);
    let ct = B64.decode(&sealed.ciphertext).map_err(|_| EnvelopeError::Decrypt)?;
    open_with(&dek, context.as_bytes(), &ct)
}

/// Wraps the data key of `sealed` under `current`, after a rotation of the key-encryption key.
/// The secret's own ciphertext stays as it is. `None` when it is already under `current`.
pub fn rewrap(keks: &[Kek], current: &Kek, context: &str, sealed: &Sealed) -> Result<Option<Sealed>, EnvelopeError> {
    if sealed.kek_id == current.id {
        return Ok(None);
    }
    let kek =
        keks.iter().find(|k| k.id == sealed.kek_id).ok_or_else(|| EnvelopeError::UnknownKek(sealed.kek_id.clone()))?;
    let wrapped = B64.decode(&sealed.wrapped_dek).map_err(|_| EnvelopeError::Decrypt)?;
    let dek = open_with(&kek.key, context.as_bytes(), &wrapped)?;
    Ok(Some(Sealed {
        kek_id: current.id.clone(),
        wrapped_dek: B64.encode(seal_with(&current.key, context.as_bytes(), &dek)),
        ciphertext: sealed.ciphertext.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kek(id: &str, byte: u8) -> Kek {
        Kek::new(id, [byte; 32])
    }

    #[test]
    fn round_trip() {
        let k = kek("k1", 7);
        let sealed = seal(&k, "cred_1", b"sk-ant-secret");
        assert_eq!(&open(&[k], "cred_1", &sealed).unwrap()[..], b"sk-ant-secret");
        assert!(!sealed.ciphertext.contains("sk-ant"));
    }

    #[test]
    fn same_plaintext_encrypts_differently() {
        let k = kek("k1", 7);
        let a = seal(&k, "c", b"x");
        let b = seal(&k, "c", b"x");
        assert_ne!(a.ciphertext, b.ciphertext);
        assert_ne!(a.wrapped_dek, b.wrapped_dek);
    }

    #[test]
    fn wrong_context_key_or_tampering_fails() {
        let k = kek("k1", 7);
        let sealed = seal(&k, "cred_1", b"secret");
        assert!(matches!(open(std::slice::from_ref(&k), "cred_2", &sealed), Err(EnvelopeError::Decrypt)));
        assert!(matches!(open(&[kek("k1", 8)], "cred_1", &sealed), Err(EnvelopeError::Decrypt)));
        assert!(matches!(open(&[kek("k2", 7)], "cred_1", &sealed), Err(EnvelopeError::UnknownKek(_))));

        let mut bad = sealed.clone();
        let mut raw = B64.decode(&bad.ciphertext).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        bad.ciphertext = B64.encode(raw);
        assert!(matches!(open(&[k], "cred_1", &bad), Err(EnvelopeError::Decrypt)));
    }

    #[test]
    fn rotation_keeps_old_records_readable() {
        let old = kek("2026-01", 1);
        let new = kek("2026-10", 2);
        let sealed_old = seal(&old, "c", b"a");
        let sealed_new = seal(&new, "c", b"b");
        let ring = [new, old];
        assert_eq!(&open(&ring, "c", &sealed_old).unwrap()[..], b"a");
        assert_eq!(&open(&ring, "c", &sealed_new).unwrap()[..], b"b");
    }

    #[test]
    fn kek_from_base64_checks_length() {
        assert!(Kek::from_base64("k", &B64.encode([0u8; 32])).is_ok());
        assert!(Kek::from_base64("k", &B64.encode([0u8; 16])).is_err());
        assert!(Kek::from_base64("k", "not base64!").is_err());
    }

    #[test]
    fn rewrapping_moves_the_data_key_to_the_current_key() {
        let (old, new) = (kek("2026-01", 1), kek("2027-01", 2));
        let sealed = seal(&old, "cred_1", b"sk-secret");
        let moved = rewrap(&[new.clone(), old.clone()], &new, "cred_1", &sealed).unwrap().unwrap();
        assert_eq!(moved.kek_id, "2027-01");
        assert_eq!(moved.ciphertext, sealed.ciphertext, "the secret itself is not encrypted again");
        assert_eq!(open(std::slice::from_ref(&new), "cred_1", &moved).unwrap().as_slice(), b"sk-secret");
        assert!(rewrap(std::slice::from_ref(&new), &new, "cred_1", &moved).unwrap().is_none(), "already current");
        assert!(matches!(
            rewrap(std::slice::from_ref(&new), &new, "cred_1", &sealed),
            Err(EnvelopeError::UnknownKek(_))
        ));
        assert!(rewrap(&[new.clone(), old], &new, "cred_2", &sealed).is_err(), "bound to its record");
    }
}
