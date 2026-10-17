//! Answers kept per instance, so that the same text is not classified twice in a short time
//! (`docs/plugin-concept.md`, section 5.9). Retries, regenerated answers and agents that loop
//! send the same text again.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::rules::Answer;

/// The most answers an instance keeps, across all workspaces.
pub const CAPACITY: usize = 10_000;

pub struct Cache {
    /// Keys are hashed with a per-process random key: a caller cannot craft a text that lands
    /// on another's answer.
    hasher: RandomState,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    answers: HashMap<u64, (Instant, Answer)>,
    /// Keys in the order they were added, oldest first, to drop the oldest when full.
    order: VecDeque<u64>,
}

impl Default for Cache {
    fn default() -> Self {
        Self { hasher: RandomState::new(), inner: Mutex::new(Inner::default()) }
    }
}

impl Cache {
    /// The key of a text in a workspace, for a binding as it is now (its settings and its
    /// catalogue's version are in `fingerprint`). Never shared across workspaces.
    pub fn key(&self, workspace_id: &str, fingerprint: u64, text: &str) -> u64 {
        let mut h = self.hasher.build_hasher();
        workspace_id.hash(&mut h);
        fingerprint.hash(&mut h);
        text.hash(&mut h);
        h.finish()
    }

    /// The answer kept for `key`, if it is younger than `ttl`.
    pub fn get(&self, key: u64, ttl: Duration) -> Option<Answer> {
        let inner = self.inner.lock();
        let (at, answer) = inner.answers.get(&key)?;
        (at.elapsed() < ttl).then(|| answer.clone())
    }

    pub fn put(&self, key: u64, answer: Answer) {
        let mut inner = self.inner.lock();
        if inner.answers.insert(key, (Instant::now(), answer)).is_none() {
            inner.order.push_back(key);
        }
        while inner.answers.len() > CAPACITY {
            let Some(old) = inner.order.pop_front() else { break };
            inner.answers.remove(&old);
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().answers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn answer(label: &str) -> Answer {
        Answer {
            label: label.into(),
            confidence: 1.0,
            flags: Default::default(),
            scores: Default::default(),
            choices: Default::default(),
            engine: "llm".into(),
            detail: Value::Null,
        }
    }

    #[test]
    fn answers_are_kept_per_workspace_for_a_while() {
        let c = Cache::default();
        let k = c.key("ws_a", 7, "hello");
        assert_ne!(k, c.key("ws_b", 7, "hello"), "workspaces never share answers");
        assert_ne!(k, c.key("ws_a", 8, "hello"), "a changed binding or catalogue asks again");
        c.put(k, answer("smalltalk"));
        assert_eq!(c.get(k, Duration::from_secs(60)).map(|a| a.label), Some("smalltalk".into()));
        assert!(c.get(k, Duration::ZERO).is_none(), "too old");
    }

    #[test]
    fn the_oldest_go_when_it_is_full() {
        let c = Cache::default();
        for n in 0..(CAPACITY + 5) {
            c.put(n as u64, answer("x"));
        }
        assert_eq!(c.len(), CAPACITY);
        assert!(c.get(0, Duration::from_secs(60)).is_none());
        assert!(c.get((CAPACITY + 4) as u64, Duration::from_secs(60)).is_some());
    }
}
