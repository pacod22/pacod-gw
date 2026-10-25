//! Where the response cache keeps answers (`docs/plugin-concept.md`, section 15): Valkey, shared
//! by every instance, or this instance's memory when no Valkey is configured.
//!
//! The store holds opaque bytes under a key, for a lifetime. What goes in, and when it is read,
//! is decided in `plugins/answers.rs`. A store that fails never fails a call: the call is then
//! served by its provider, as on a miss.

pub mod memory;
pub mod valkey;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;

pub use memory::MemoryStore;

/// Why the store could not answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Failed(String),
    /// The store failed several times in a row and is skipped for a while.
    #[error("the store is skipped while it is failing")]
    Unavailable,
}

pub trait AnswerStore: Send + Sync {
    /// `memory` or `valkey`, for logs and metrics.
    fn kind(&self) -> &'static str;

    /// The value kept under `key`, unless it has expired.
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>, StoreError>>;

    /// Keeps `value` under `key` for `ttl`, replacing what was there.
    fn put<'a>(&'a self, key: &'a str, value: Bytes, ttl: Duration) -> BoxFuture<'a, Result<(), StoreError>>;
}

/// The store the bootstrap configuration names: Valkey, or this instance's memory.
pub fn open(config: &crate::bootstrap::CacheConfig) -> anyhow::Result<Arc<dyn AnswerStore>> {
    match &config.valkey_url {
        Some(url) => Ok(Arc::new(valkey::ValkeyStore::new(url, config.valkey_password.as_deref())?)),
        None => Ok(Arc::new(MemoryStore::new(config.memory_bytes))),
    }
}

/// The store key of an answer: per workspace, so that the answers of one workspace can be found
/// together and never under another's key.
pub fn store_key(workspace_id: &str, key: &[u8; 32]) -> String {
    format!("pgw:answer:v1:{workspace_id}:{}", hex::encode(key))
}
