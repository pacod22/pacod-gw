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

/// What one instance knows of its store without asking it: published in the instance's live
/// report, since both the breaker and a memory store belong to one process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreFigures {
    pub store: &'static str,
    /// Answers held, for a store this process holds itself. Expired answers nobody asked for
    /// since are counted until they are pushed out.
    pub entries: Option<u64>,
    pub bytes: Option<u64>,
    pub capacity_bytes: Option<u64>,
    /// Calls skip the store for a while after failures in a row.
    pub breaker_open: bool,
    pub failures_in_a_row: u32,
}

/// What a store shared by every instance says of itself, read on demand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedFigures {
    /// Keys in the database: the store's Valkey holds the answers alone.
    pub keys: u64,
    pub used_memory: u64,
    /// 0 when no limit is set.
    pub max_memory: u64,
    pub eviction_policy: String,
}

/// Which answers to forget.
#[derive(Debug, Clone, Copy)]
pub enum Forget<'a> {
    All,
    Workspaces(&'a [String]),
}

impl Forget<'_> {
    /// Whether a store key is among the answers to forget.
    pub fn covers(&self, key: &str) -> bool {
        match self {
            Forget::All => key.starts_with(KEY_PREFIX),
            Forget::Workspaces(ids) => key_workspace(key).is_some_and(|ws| ids.iter().any(|id| id == ws)),
        }
    }
}

pub trait AnswerStore: Send + Sync {
    /// `memory` or `valkey`, for logs and metrics.
    fn kind(&self) -> &'static str;

    /// Whether every instance reads the same answers: then one instance can forget them for all.
    fn shared(&self) -> bool;

    /// The value kept under `key`, unless it has expired.
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>, StoreError>>;

    /// Keeps `value` under `key` for `ttl`, replacing what was there.
    fn put<'a>(&'a self, key: &'a str, value: Bytes, ttl: Duration) -> BoxFuture<'a, Result<(), StoreError>>;

    fn figures(&self) -> StoreFigures;

    /// The figures of a shared store, `None` for one this process holds. Management work: it
    /// neither waits for nor trips the breaker of the request path.
    fn inspect(&self) -> BoxFuture<'_, Result<Option<SharedFigures>, StoreError>>;

    /// Removes answers to free their space, and says how many went. A clear does not depend
    /// on it: answers kept before a clear are misses anyway (`plugins/answers.rs`).
    fn forget<'a>(&'a self, which: Forget<'a>) -> BoxFuture<'a, Result<u64, StoreError>>;
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
    format!("{KEY_PREFIX}v1:{workspace_id}:{}", hex::encode(key))
}

/// What every store key starts with, whatever its version.
pub const KEY_PREFIX: &str = "pgw:answer:";

/// The workspace a store key belongs to: its fourth segment, in every version so far.
pub fn key_workspace(key: &str) -> Option<&str> {
    key.strip_prefix(KEY_PREFIX)?.split(':').nth(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_names_its_workspace() {
        let key = store_key("ws_a", &[7; 32]);
        assert_eq!(key_workspace(&key), Some("ws_a"));
        assert_eq!(key_workspace("other:key"), None);
        let ids = ["ws_a".to_owned()];
        assert!(Forget::Workspaces(&ids).covers(&key));
        assert!(!Forget::Workspaces(&ids).covers(&store_key("ws_b", &[7; 32])));
        assert!(Forget::All.covers(&key) && !Forget::All.covers("other:key"));
    }
}
