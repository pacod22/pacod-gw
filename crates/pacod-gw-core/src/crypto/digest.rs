//! Keyed hashes of what callers send, for records that must tell values apart without holding
//! them: the arguments of a tool call (catalogue MCP-14).
//!
//! A plain hash of small arguments (`{"user_id": 42}`) is reversed by trying values, so the hash
//! is an HMAC under a key derived from the key-encryption key (`PGW_KEK`), one key per workspace:
//! equal arguments give equal hashes within a workspace and say nothing across workspaces.
//!
//! The key comes from the current key-encryption key. After a rotation the same arguments hash
//! differently, which only means that calls before and after it are not matched.

use ring::{hkdf, hmac};
use serde_json::Value;
use zeroize::Zeroizing;

use super::envelope::Kek;
use crate::json::canonical;

/// What the keys are derived for. Raised with the form of the hash.
const SALT: &[u8] = b"pacod-gw tool arguments v1";
/// What a hash starts with: its form.
const PREFIX: &str = "hmac1:";
/// Bytes of the tag kept: enough to tell calls apart, short enough for every record.
const TAG_BYTES: usize = 16;

/// The keyed hash of `arguments` in `workspace`. Equal values with members in another order
/// hash alike.
pub fn arguments_hmac(kek: &Kek, workspace: &str, arguments: &Value) -> String {
    let mut key = Zeroizing::new([0u8; 32]);
    kek.prk(SALT)
        .expand(&[b"arguments", workspace.as_bytes()], hkdf::HKDF_SHA256)
        .and_then(|okm| okm.fill(key.as_mut_slice()))
        .expect("32 bytes are within what HKDF-SHA256 gives");
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key.as_slice()), &canonical(arguments));
    format!("{PREFIX}{}", hex::encode(&tag.as_ref()[..TAG_BYTES]))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn equal_arguments_hash_alike_in_one_workspace_only() {
        let kek = Kek::new("k1", [7; 32]);
        let a = arguments_hmac(&kek, "ws_a", &json!({ "q": "x", "n": 1 }));
        assert_eq!(a, arguments_hmac(&kek, "ws_a", &json!({ "n": 1, "q": "x" })));
        assert_ne!(a, arguments_hmac(&kek, "ws_b", &json!({ "n": 1, "q": "x" })));
        assert_ne!(a, arguments_hmac(&kek, "ws_a", &json!({ "n": 2, "q": "x" })));
        assert_ne!(a, arguments_hmac(&Kek::new("k2", [8; 32]), "ws_a", &json!({ "n": 1, "q": "x" })));
        assert!(a.starts_with("hmac1:") && a.len() == "hmac1:".len() + 2 * TAG_BYTES, "{a}");
    }
}
