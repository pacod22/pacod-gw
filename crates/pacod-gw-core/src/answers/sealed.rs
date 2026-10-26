//! Answers sealed before they reach a store that other machines read: Valkey
//! (`docs/plugin-concept.md`, section 15.7).
//!
//! A store shared by every instance holds what users asked and were answered for as long as
//! the answers live, perhaps on another host, and whoever reaches it could read them or put
//! answers of their own there. Each answer is therefore sealed with AES-256-GCM under a key
//! derived from the key-encryption key (`PGW_KEK`) for its workspace and its name, and the name
//! is itself a keyed hash: someone who reads the store learns neither what was asked nor, by
//! trying a question, whether it was asked.
//!
//! Keys are derived from the current key-encryption key only. After a rotation the answers
//! sealed under the previous one are not found under their names any more and expire in their
//! time: one round of misses, nothing to rewrap.
//!
//! What does not open (an answer changed in the store, or one kept before answers were sealed)
//! is a miss: never read as it is, and never an error.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use ring::{hkdf, hmac};
use zeroize::Zeroizing;

use super::{AnswerStore, Forget, KEY_PREFIX, SharedFigures, StoreError, StoreFigures, key_workspace};
use crate::crypto::envelope::{Kek, open_with, seal_with};

/// What the keys are derived for. Raised with the form of a sealed answer.
const SALT: &[u8] = b"pacod-gw response cache v1";
/// What a sealed answer starts with.
const MAGIC: &[u8; 4] = b"pgs1";
/// The version segment of the names sealed answers are kept under. Plain answers were kept
/// under `v1`, which is never read.
const NAME_VERSION: &str = "v2";

/// A store whose answers are sealed, around one that holds bytes for every instance.
pub struct SealedStore {
    inner: Arc<dyn AnswerStore>,
    prk: hkdf::Prk,
}

impl SealedStore {
    pub fn new(inner: Arc<dyn AnswerStore>, kek: &Kek) -> Self {
        Self { inner, prk: kek.prk(SALT) }
    }

    fn derive(&self, info: &[&[u8]]) -> Zeroizing<[u8; 32]> {
        let mut key = Zeroizing::new([0u8; 32]);
        self.prk
            .expand(info, hkdf::HKDF_SHA256)
            .and_then(|okm| okm.fill(key.as_mut_slice()))
            .expect("32 bytes are within what HKDF-SHA256 gives");
        key
    }

    /// The workspace of a key the core gave, and the name its answer is kept under: the
    /// workspace in the clear, so that clears find a workspace's answers, and the rest a hash
    /// keyed for the workspace. `None` for a key that names no workspace, which the core never
    /// gives.
    fn name<'k>(&self, key: &'k str) -> Option<(&'k str, String)> {
        let ws = key_workspace(key)?;
        let name_key = self.derive(&[b"name", ws.as_bytes()]);
        let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, name_key.as_slice()), key.as_bytes());
        Some((ws, format!("{KEY_PREFIX}{NAME_VERSION}:{ws}:{}", hex::encode(tag.as_ref()))))
    }

    /// The key the answer kept under `name` is sealed with: its own, so that no two answers
    /// share a key and random nonces stay far from repeating.
    fn seal_key(&self, ws: &str, name: &str) -> Zeroizing<[u8; 32]> {
        self.derive(&[b"seal", ws.as_bytes(), name.as_bytes()])
    }

    fn seal(&self, ws: &str, name: &str, value: &[u8]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend(seal_with(&self.seal_key(ws, name), name.as_bytes(), value));
        out
    }

    fn open(&self, ws: &str, name: &str, stored: &[u8]) -> Option<Bytes> {
        let sealed = stored.strip_prefix(MAGIC)?;
        match open_with(&self.seal_key(ws, name), name.as_bytes(), sealed) {
            Ok(v) => Some(Bytes::copy_from_slice(&v)),
            Err(_) => {
                tracing::debug!(workspace_id = ws, "a kept answer did not open; it is a miss");
                None
            }
        }
    }
}

impl AnswerStore for SealedStore {
    fn kind(&self) -> &'static str {
        self.inner.kind()
    }

    fn shared(&self) -> bool {
        self.inner.shared()
    }

    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>, StoreError>> {
        Box::pin(async move {
            let Some((ws, name)) = self.name(key) else { return Ok(None) };
            Ok(self.inner.get(&name).await?.and_then(|stored| self.open(ws, &name, &stored)))
        })
    }

    fn put<'a>(&'a self, key: &'a str, value: Bytes, ttl: Duration) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let Some((ws, name)) = self.name(key) else { return Ok(()) };
            let sealed = Bytes::from(self.seal(ws, &name, &value));
            self.inner.put(&name, sealed, ttl).await
        })
    }

    fn figures(&self) -> StoreFigures {
        self.inner.figures()
    }

    fn inspect(&self) -> BoxFuture<'_, Result<Option<SharedFigures>, StoreError>> {
        self.inner.inspect()
    }

    /// The names keep the workspace where clears look for it, so the inner store forgets
    /// sealed answers as it forgot plain ones.
    fn forget<'a>(&'a self, which: Forget<'a>) -> BoxFuture<'a, Result<u64, StoreError>> {
        self.inner.forget(which)
    }

    fn prepare(&self) -> BoxFuture<'_, ()> {
        self.inner.prepare()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answers::{MemoryStore, store_key};

    /// A sealed store over a memory store, and the memory store to look into.
    fn stores(kek: u8) -> (SealedStore, Arc<MemoryStore>) {
        let inner = Arc::new(MemoryStore::new(1 << 20));
        (SealedStore::new(inner.clone(), &Kek::new("k", [kek; 32])), inner)
    }

    const TTL: Duration = Duration::from_secs(60);
    const ANSWER: &[u8] = br#"{"v":1,"response":{"content":"Oslo is the capital of Norway"}}"#;

    #[tokio::test]
    async fn an_answer_is_kept_sealed_under_a_name_that_says_nothing() {
        let (sealed, inner) = stores(1);
        let key = store_key("ws_a", &[7; 32]);
        sealed.put(&key, Bytes::from_static(ANSWER), TTL).await.unwrap();
        assert_eq!(sealed.get(&key).await.unwrap().as_deref(), Some(ANSWER));

        assert_eq!(inner.get(&key).await.unwrap(), None, "not under the core's key");
        let (_, name) = sealed.name(&key).unwrap();
        assert!(name.starts_with("pgw:answer:v2:ws_a:") && !name.contains(&hex::encode([7u8; 32])), "{name}");
        assert_eq!(key_workspace(&name), Some("ws_a"), "clears still find the workspace");
        let stored = inner.get(&name).await.unwrap().expect("kept");
        assert!(stored.starts_with(MAGIC));
        assert!(!stored.windows(4).any(|w| w == b"Oslo"), "no plain text in the store");
    }

    #[tokio::test]
    async fn what_does_not_open_is_a_miss() {
        let (sealed, inner) = stores(1);
        let key = store_key("ws_a", &[7; 32]);
        let (_, name) = sealed.name(&key).unwrap();

        // An answer kept in the clear, as before answers were sealed, or put there by whoever
        // reaches the store.
        inner.put(&name, Bytes::from_static(ANSWER), TTL).await.unwrap();
        assert_eq!(sealed.get(&key).await.unwrap(), None);

        // Changed in the store.
        sealed.put(&key, Bytes::from_static(ANSWER), TTL).await.unwrap();
        let mut stored = inner.get(&name).await.unwrap().unwrap().to_vec();
        let last = stored.len() - 1;
        stored[last] ^= 1;
        inner.put(&name, Bytes::from(stored), TTL).await.unwrap();
        assert_eq!(sealed.get(&key).await.unwrap(), None);

        // Copied under another workspace's name.
        sealed.put(&key, Bytes::from_static(ANSWER), TTL).await.unwrap();
        let other = store_key("ws_b", &[7; 32]);
        let (_, other_name) = sealed.name(&other).unwrap();
        inner.put(&other_name, inner.get(&name).await.unwrap().unwrap(), TTL).await.unwrap();
        assert_eq!(sealed.get(&other).await.unwrap(), None);
    }

    #[tokio::test]
    async fn another_key_encryption_key_finds_nothing() {
        let (sealed, inner) = stores(1);
        let key = store_key("ws_a", &[7; 32]);
        sealed.put(&key, Bytes::from_static(ANSWER), TTL).await.unwrap();
        let (_, name) = sealed.name(&key).unwrap();

        let rotated = SealedStore::new(inner.clone(), &Kek::new("k2", [2; 32]));
        let (_, rotated_name) = rotated.name(&key).unwrap();
        assert_ne!(name, rotated_name, "another name after a rotation");
        assert_eq!(rotated.get(&key).await.unwrap(), None);
        // Even under the old name, the answer does not open with the new key.
        inner.put(&rotated_name, inner.get(&name).await.unwrap().unwrap(), TTL).await.unwrap();
        assert_eq!(rotated.get(&key).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_workspace_is_forgotten_with_its_sealed_answers() {
        let (sealed, inner) = stores(1);
        for ws in ["ws_a", "ws_b"] {
            sealed.put(&store_key(ws, &[7; 32]), Bytes::from_static(ANSWER), TTL).await.unwrap();
        }
        let ids = ["ws_a".to_owned()];
        assert_eq!(sealed.forget(Forget::Workspaces(&ids)).await.unwrap(), 1);
        assert_eq!(sealed.get(&store_key("ws_a", &[7; 32])).await.unwrap(), None);
        assert!(sealed.get(&store_key("ws_b", &[7; 32])).await.unwrap().is_some());
        assert_eq!(inner.figures().entries, Some(1));
    }
}
