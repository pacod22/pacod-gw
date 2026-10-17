//! The product's copy of what pacod-id decides: bindings with their paths,
//! disabled and deleted principals, recently ended sessions, the scopes. Loaded from the
//! snapshot, then kept current by long-polling the changes; held in an `ArcSwap`, so a lookup
//! takes no lock.
//!
//! Starting never waits for pacod-id: [`AuthzCache::spawn`] returns at once and the follower
//! keeps trying. Until the first snapshot [`AuthzCache::is_ready`] is false and
//! [`AuthzCache::feed_age`] is `None`; a product refuses its admin surfaces then, and when the
//! feed is older than its limit, but not its data plane.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use pacod_id_authz::{Binding, ScopeRef};
use serde_json::Value;
use tokio::sync::{broadcast, watch};

use crate::{ApiError, IdClient};

/// Ended sessions are remembered this long: no access token outlives it (15 minutes at most).
const REVOKED_SIDS_FOR: i64 = 15 * 60;
/// The long poll's wait, in seconds; the HTTP client waits longer.
const WAIT: u64 = 25;

/// A binding as the product holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub id: String,
    pub role: String,
    pub path: Vec<ScopeRef>,
}

/// A change from the feed, for the product to act on (`principal.deleted`: delete its data;
/// `credential.revoked`: forget a cached token).
#[derive(Debug, Clone)]
pub struct Event {
    pub seq: i64,
    pub kind: String,
    /// The event's fields (`sub`, `sid`, `token_id`, `principal`, `role`, `path` …).
    pub data: Value,
}

/// The copy at one `seq`.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub seq: i64,
    pub catalog_version: Option<i64>,
    pub bindings: HashMap<String, Vec<Held>>,
    pub disabled: HashSet<String>,
    pub deleted: HashSet<String>,
    /// Ended sessions, with when the product learned of it (unix seconds).
    pub revoked_sids: HashMap<String, i64>,
    /// Scopes by `type:id`, with their display names.
    pub scope_names: HashMap<String, String>,
}

fn path_of(v: &Value) -> Option<Vec<ScopeRef>> {
    v.as_array()?.iter().map(|s| s.as_str().and_then(|s| ScopeRef::from_str(s).ok())).collect()
}

fn scope_key(v: &Value) -> Option<String> {
    let kind = v["type"].as_str()?;
    let id = v["id"].as_str()?;
    Some(format!("{kind}:{id}"))
}

impl Snapshot {
    fn from_json(v: &Value) -> Result<Self, ApiError> {
        let bad = |what: &str| ApiError::Malformed(format!("the snapshot's {what}"));
        let mut s = Snapshot {
            seq: v["seq"].as_i64().ok_or_else(|| bad("seq"))?,
            catalog_version: v["catalog"]["version"].as_i64(),
            ..Snapshot::default()
        };
        for b in v["bindings"].as_array().ok_or_else(|| bad("bindings"))? {
            s.add_binding(b);
        }
        let set = |key: &str| -> HashSet<String> {
            v[key]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                .unwrap_or_default()
        };
        s.disabled = set("disabled");
        s.deleted = set("deleted");
        let now = crate::now();
        for r in v["revoked_sids"].as_array().into_iter().flatten() {
            if let Some(sid) = r["sid"].as_str() {
                s.revoked_sids.insert(sid.to_owned(), now);
            }
        }
        for sc in v["scopes"].as_array().into_iter().flatten() {
            if let Some(k) = scope_key(sc) {
                s.scope_names.insert(k, sc["display_name"].as_str().unwrap_or_default().to_owned());
            }
        }
        Ok(s)
    }

    fn add_binding(&mut self, b: &Value) {
        let (Some(id), Some(principal), Some(role), Some(path)) =
            (b["id"].as_str(), b["principal"].as_str(), b["role"].as_str(), path_of(&b["path"]))
        else {
            return;
        };
        let held = self.bindings.entry(principal.to_owned()).or_default();
        if !held.iter().any(|h| h.id == id) {
            held.push(Held { id: id.to_owned(), role: role.to_owned(), path });
        }
    }

    fn apply(&mut self, e: &Event) {
        let d = &e.data;
        let sub = d["sub"].as_str().map(str::to_owned);
        match e.kind.as_str() {
            "binding.added" => self.add_binding(d),
            "binding.removed" => {
                if let (Some(p), Some(id)) = (d["principal"].as_str(), d["id"].as_str())
                    && let Some(held) = self.bindings.get_mut(p)
                {
                    held.retain(|h| h.id != id);
                    if held.is_empty() {
                        self.bindings.remove(p);
                    }
                }
            }
            "principal.disabled" => {
                self.disabled.extend(sub);
            }
            "principal.enabled" => {
                if let Some(s) = sub {
                    self.disabled.remove(&s);
                }
            }
            "principal.deleted" => {
                if let Some(s) = sub {
                    self.bindings.remove(&s);
                    self.disabled.remove(&s);
                    self.deleted.insert(s);
                }
            }
            "session.revoked" => {
                if let Some(sid) = d["sid"].as_str() {
                    self.revoked_sids.insert(sid.to_owned(), crate::now());
                }
            }
            "scope.upserted" => {
                if let Some(k) = scope_key(d) {
                    self.scope_names.insert(k, d["display_name"].as_str().unwrap_or_default().to_owned());
                }
            }
            "scope.deleted" => {
                if let Some(k) = scope_key(d) {
                    self.scope_names.remove(&k);
                }
            }
            "catalog.updated" => self.catalog_version = d["version"].as_i64().or(self.catalog_version),
            _ => {}
        }
        self.seq = self.seq.max(e.seq);
    }

    fn prune(&mut self) {
        let oldest = crate::now() - REVOKED_SIDS_FOR;
        self.revoked_sids.retain(|_, at| *at > oldest);
    }

    /// A principal's bindings, as the rules engine reads them.
    pub fn bindings_of(&self, principal: &str) -> Vec<Binding> {
        self.bindings
            .get(principal)
            .map(|h| h.iter().map(|b| Binding::new(b.role.clone(), b.path.clone())).collect())
            .unwrap_or_default()
    }
}

/// The product's copy, and its follower.
pub struct AuthzCache {
    state: ArcSwap<Snapshot>,
    ready: watch::Sender<bool>,
    seq: watch::Sender<i64>,
    /// Unix milliseconds of the last answer of the feed; 0 before the first.
    last_ok: AtomicI64,
    events: broadcast::Sender<Event>,
}

impl AuthzCache {
    /// Starts following the feed; returns at once. The follower stops when `stop` turns true.
    pub fn spawn(client: IdClient, stop: watch::Receiver<bool>) -> Arc<AuthzCache> {
        let cache = Arc::new(AuthzCache {
            state: ArcSwap::from_pointee(Snapshot::default()),
            ready: watch::Sender::new(false),
            seq: watch::Sender::new(0),
            last_ok: AtomicI64::new(0),
            events: broadcast::Sender::new(1024),
        });
        tokio::spawn(follow(cache.clone(), client, stop));
        cache
    }

    /// The copy as it is now.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.state.load_full()
    }

    /// The feed's `seq` the copy has applied.
    pub fn seq(&self) -> i64 {
        *self.seq.borrow()
    }

    /// Whether the first snapshot has arrived.
    pub fn is_ready(&self) -> bool {
        *self.ready.borrow()
    }

    /// The time since the feed last answered; `None` before the first answer.
    pub fn feed_age(&self) -> Option<Duration> {
        let at = self.last_ok.load(Ordering::Relaxed);
        (at > 0).then(|| {
            let now =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
            Duration::from_millis(now.saturating_sub(at) as u64)
        })
    }

    /// Waits until the copy has `seq` (read your own writes), at most `timeout`. Whether it did.
    pub async fn wait_for(&self, seq: i64, timeout: Duration) -> bool {
        let mut rx = self.seq.subscribe();
        tokio::time::timeout(timeout, rx.wait_for(|s| *s >= seq)).await.is_ok_and(|r| r.is_ok())
    }

    /// The changes as they arrive.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn bindings_of(&self, principal: &str) -> Vec<Binding> {
        self.state.load().bindings_of(principal)
    }

    pub fn is_disabled(&self, principal: &str) -> bool {
        let s = self.state.load();
        s.disabled.contains(principal) || s.deleted.contains(principal)
    }

    /// Whether a session has ended (in the last 15 minutes: older ones no token outlives).
    pub fn sid_revoked(&self, sid: &str) -> bool {
        self.state.load().revoked_sids.contains_key(sid)
    }

    fn mark_ok(&self) {
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        self.last_ok.store(now, Ordering::Relaxed);
    }

    fn store(&self, s: Snapshot) {
        let seq = s.seq;
        self.state.store(Arc::new(s));
        self.seq.send_if_modified(|v| {
            let newer = seq > *v;
            if newer {
                *v = seq;
            }
            newer
        });
    }
}

async fn pause(stop: &mut watch::Receiver<bool>, for_: Duration) -> bool {
    tokio::select! {
        _ = stop.wait_for(|s| *s) => true,
        _ = tokio::time::sleep(for_) => false,
    }
}

async fn follow(cache: Arc<AuthzCache>, client: IdClient, mut stop: watch::Receiver<bool>) {
    let mut backoff = Duration::from_secs(1);
    'snapshot: loop {
        if *stop.borrow() {
            return;
        }
        let snapshot = match client.snapshot_raw().await.and_then(|v| Snapshot::from_json(&v)) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "pacod-id's snapshot cannot be read; trying again");
                if pause(&mut stop, backoff).await {
                    return;
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        let mut after = snapshot.seq;
        // Deletions still waiting for this product's confirmation are told again on every load
        // (at start, after a gap): a product erases from these events, idempotently, and
        // confirms; one it missed while it was down is not left behind.
        let pending: Vec<Event> = snapshot
            .deleted
            .iter()
            .map(|sub| Event {
                seq: snapshot.seq,
                kind: "principal.deleted".into(),
                data: serde_json::json!({ "kind": "principal.deleted", "sub": sub, "from_snapshot": true }),
            })
            .collect();
        cache.store(snapshot);
        cache.mark_ok();
        cache.ready.send_replace(true);
        for e in pending {
            let _ = cache.events.send(e);
        }
        backoff = Duration::from_secs(1);
        loop {
            let answer = tokio::select! {
                _ = stop.wait_for(|s| *s) => return,
                a = client.changes_raw(after, WAIT) => a,
            };
            match answer {
                Ok(v) => {
                    let events: Vec<Event> = v["events"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|e| {
                            Some(Event {
                                seq: e["seq"].as_i64()?,
                                kind: e["kind"].as_str()?.to_owned(),
                                data: e.clone(),
                            })
                        })
                        .collect();
                    let mut next = (*cache.snapshot()).clone();
                    for e in &events {
                        next.apply(e);
                    }
                    // The answer's seq, not the last event's: changes for other products count.
                    next.seq = v["seq"].as_i64().unwrap_or(next.seq).max(next.seq);
                    next.prune();
                    after = next.seq;
                    cache.store(next);
                    cache.mark_ok();
                    for e in events {
                        let _ = cache.events.send(e);
                    }
                    backoff = Duration::from_secs(1);
                }
                Err(e) if e.status() == Some(410) => {
                    tracing::info!("pacod-id no longer keeps the changes this copy needs; reading the snapshot again");
                    continue 'snapshot;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "pacod-id's changes cannot be read; trying again");
                    if pause(&mut stop, backoff).await {
                        return;
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(seq: i64, kind: &str, data: Value) -> Event {
        Event { seq, kind: kind.into(), data }
    }

    #[test]
    fn events_change_the_copy() {
        let mut s = Snapshot::from_json(&json!({
            "seq": 10, "catalog": { "version": 1 },
            "bindings": [{ "id": "rb_1", "principal": "usr_a", "role": "gw.viewer", "path": ["platform", "organisation:o1"] }],
            "disabled": ["usr_x"], "deleted": [], "revoked_sids": [{ "sid": "sid_1", "sub": "usr_a" }],
            "scopes": [{ "type": "organisation", "id": "o1", "display_name": "Acme" }]
        }))
        .unwrap();
        assert_eq!(s.bindings_of("usr_a").len(), 1);
        s.apply(&event(
            11,
            "binding.added",
            json!({ "id": "rb_2", "principal": "usr_a", "role": "gw.auditor", "path": ["platform"] }),
        ));
        s.apply(&event(
            11,
            "binding.added",
            json!({ "id": "rb_2", "principal": "usr_a", "role": "gw.auditor", "path": ["platform"] }),
        ));
        assert_eq!(s.bindings_of("usr_a").len(), 2, "the same binding twice is one");
        s.apply(&event(12, "binding.removed", json!({ "id": "rb_1", "principal": "usr_a" })));
        assert_eq!(s.bindings["usr_a"][0].id, "rb_2");
        s.apply(&event(13, "principal.enabled", json!({ "sub": "usr_x" })));
        s.apply(&event(14, "principal.deleted", json!({ "sub": "usr_a" })));
        assert!(s.bindings_of("usr_a").is_empty() && s.deleted.contains("usr_a") && s.disabled.is_empty());
        s.apply(&event(15, "scope.deleted", json!({ "type": "organisation", "id": "o1" })));
        s.apply(&event(16, "catalog.updated", json!({ "version": 2 })));
        assert!(s.scope_names.is_empty() && s.catalog_version == Some(2) && s.seq == 16);
        // Ended sessions are forgotten once no token can outlive them.
        s.revoked_sids.insert("sid_old".into(), crate::now() - REVOKED_SIDS_FOR - 1);
        s.prune();
        assert!(s.revoked_sids.contains_key("sid_1") && !s.revoked_sids.contains_key("sid_old"));
    }
}
