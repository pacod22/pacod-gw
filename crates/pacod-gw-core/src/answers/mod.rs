//! Where the response cache keeps answers (`docs/plugin-concept.md`, section 15): Valkey, shared
//! by every instance and sealed for it (`sealed.rs`), or this instance's memory when no Valkey
//! is configured.
//!
//! The store holds opaque bytes under a key, for a lifetime. What goes in, and when it is read,
//! is decided in `plugins/answers.rs`. A store that fails never fails a call: the call is then
//! served by its provider, as on a miss.

pub mod memory;
pub mod sealed;
pub mod valkey;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use futures_util::future::BoxFuture;

pub use memory::MemoryStore;
pub use sealed::SealedStore;

use crate::crypto::envelope::Kek;
use crate::snapshot::{AnswerClears, ClearRt, Snapshot};

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

    /// Gets ready, at start and in the background, what the first call would otherwise wait
    /// for: a connection, whose TLS handshake may take longer than a lookup's time. A store
    /// that does not answer yet is left to the calls.
    fn prepare(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// The store the bootstrap configuration names: Valkey, with answers sealed under the current
/// key-encryption key, or this instance's memory, which no other process reads.
pub fn open(config: &crate::bootstrap::CacheConfig, keks: &[Kek]) -> anyhow::Result<Arc<dyn AnswerStore>> {
    match &config.valkey_url {
        Some(url) => {
            let kek =
                keks.first().context("the response cache's Valkey seals answers with PGW_KEK, which is not set")?;
            let valkey =
                valkey::ValkeyStore::new(url, config.valkey_password.as_deref(), config.valkey_ca_file.as_deref())?;
            Ok(Arc::new(SealedStore::new(Arc::new(valkey), kek)))
        }
        None => Ok(Arc::new(MemoryStore::new(config.memory_bytes))),
    }
}

/// The store key of an answer: per workspace, so that the answers of one workspace can be found
/// together and never under another's key.
pub fn store_key(workspace_id: &str, key: &[u8; 32]) -> String {
    format!("{KEY_PREFIX}v1:{workspace_id}:{}", hex::encode(key))
}

/// Which workspaces a new snapshot cleared since `before`: `None` when none, `Some(None)` for
/// all of them.
pub fn newly_cleared(before: &AnswerClears, after: &Snapshot) -> Option<Option<Vec<String>>> {
    let later = |new: &ClearRt, old: Option<&ClearRt>| old.is_none_or(|o| new.at > o.at);
    let now = &after.answer_clears;
    if now.platform.as_ref().is_some_and(|p| later(p, before.platform.as_ref())) {
        return Some(None);
    }
    let orgs: Vec<&String> =
        now.organisations.iter().filter(|(id, c)| later(c, before.organisations.get(*id))).map(|(id, _)| id).collect();
    let mut workspaces: Vec<String> = after
        .workspaces
        .values()
        .filter(|w| orgs.contains(&&w.organisation_id))
        .map(|w| w.id.clone())
        .chain(now.workspaces.iter().filter(|(id, c)| later(c, before.workspaces.get(*id))).map(|(id, _)| id.clone()))
        .collect();
    workspaces.sort_unstable();
    workspaces.dedup();
    (!workspaces.is_empty()).then_some(Some(workspaces))
}

/// Frees the space of the answers a clear made misses, in a store this instance holds alone.
/// Every gateway does this for its own memory when it applies the clear; a shared store is
/// cleaned by the management instance that made the clear.
pub async fn forget_cleared(
    config: Arc<crate::snapshot::ConfigState>,
    store: Arc<dyn AnswerStore>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let first = config.snapshot();
    let (mut version, mut clears) = (first.version, first.answer_clears.clone());
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! { _ = tick.tick() => {} _ = stop.changed() => break }
        let snap = config.snapshot();
        if snap.version == version {
            continue;
        }
        if let Some(which) = newly_cleared(&clears, &snap) {
            let forgot = match &which {
                None => store.forget(Forget::All).await,
                Some(ids) => store.forget(Forget::Workspaces(ids)).await,
            };
            if let Ok(n) = forgot {
                tracing::debug!(removed = n, "the answers of a cleared response cache were removed");
            }
        }
        (version, clears) = (snap.version, snap.answer_clears.clone());
    }
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

    #[test]
    fn a_new_snapshot_says_which_workspaces_it_cleared() {
        use crate::snapshot::WorkspaceRt;
        let clear = |s: i64| ClearRt {
            at: chrono::DateTime::from_timestamp(s, 0).unwrap(),
            by: String::new(),
            by_kind: String::new(),
        };
        let ws = |id: &str, org: &str| {
            let snap = crate::snapshot::build(
                1,
                vec![
                    row("organisation", org, None, serde_json::json!({"name": org})),
                    row("workspace", id, Some(org), serde_json::json!({"name": id})),
                ],
                &[],
            );
            Arc::clone(&snap.workspaces[id])
        };
        let mut after = Snapshot::default();
        for (id, org) in [("ws_1", "org_1"), ("ws_2", "org_1"), ("ws_3", "org_2")] {
            let w: Arc<WorkspaceRt> = ws(id, org);
            after.workspaces.insert(id.into(), w);
        }
        let before = AnswerClears::default();
        assert_eq!(newly_cleared(&before, &after), None);

        after.answer_clears.workspaces.insert("ws_3".into(), clear(10));
        assert_eq!(newly_cleared(&before, &after), Some(Some(vec!["ws_3".into()])));
        let mut seen = after.answer_clears.clone();
        assert_eq!(newly_cleared(&seen, &after), None, "already applied");

        after.answer_clears.organisations.insert("org_1".into(), clear(20));
        assert_eq!(newly_cleared(&seen, &after), Some(Some(vec!["ws_1".into(), "ws_2".into()])));
        seen = after.answer_clears.clone();
        after.answer_clears.platform = Some(clear(30));
        assert_eq!(newly_cleared(&seen, &after), Some(None));
    }

    fn row(kind: &str, id: &str, parent: Option<&str>, doc: serde_json::Value) -> crate::store::Row {
        crate::store::Row {
            id: id.into(),
            kind: kind.into(),
            parent: parent.map(Into::into),
            name: None,
            doc,
            version: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }
}
