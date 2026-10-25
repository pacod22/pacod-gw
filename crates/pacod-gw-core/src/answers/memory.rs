//! Answers kept in this instance's memory: the store of the minimal gateway, and of a full
//! gateway without Valkey. Bounded by the bytes it holds; the oldest answers go first.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use parking_lot::Mutex;

use super::{AnswerStore, StoreError};

pub struct MemoryStore {
    capacity: usize,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// By key: when it expires, its generation, and the value.
    entries: HashMap<String, (Instant, u64, Bytes)>,
    /// Keys in the order they were written, with the generation written then. A key written
    /// again has a newer generation; its older place in the queue is passed over.
    order: VecDeque<(String, u64)>,
    bytes: usize,
    generation: u64,
}

impl Inner {
    fn remove(&mut self, key: &str) {
        if let Some((_, _, v)) = self.entries.remove(key) {
            self.bytes -= v.len() + key.len();
        }
    }
}

impl MemoryStore {
    /// A store that holds at most `capacity` bytes of keys and answers.
    pub fn new(capacity: usize) -> Self {
        Self { capacity, inner: Mutex::new(Inner::default()) }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get_now(&self, key: &str) -> Option<Bytes> {
        let mut inner = self.inner.lock();
        let (expires, _, value) = inner.entries.get(key)?;
        if *expires > Instant::now() {
            return Some(value.clone());
        }
        inner.remove(key);
        None
    }

    fn put_now(&self, key: &str, value: Bytes, ttl: Duration) {
        let size = value.len() + key.len();
        if size > self.capacity {
            return;
        }
        let mut inner = self.inner.lock();
        inner.remove(key);
        inner.generation += 1;
        let generation = inner.generation;
        inner.entries.insert(key.to_owned(), (Instant::now() + ttl, generation, value));
        inner.order.push_back((key.to_owned(), generation));
        inner.bytes += size;
        while inner.bytes > self.capacity {
            let Some((old, at)) = inner.order.pop_front() else { break };
            if inner.entries.get(&old).is_some_and(|(_, g, _)| *g == at) {
                inner.remove(&old);
            }
        }
        // Places of keys written again, or removed, would otherwise pile up.
        if inner.order.len() > 2 * inner.entries.len() + 64 {
            let Inner { entries, order, .. } = &mut *inner;
            order.retain(|(k, g)| entries.get(k).is_some_and(|(_, current, _)| current == g));
        }
    }
}

impl AnswerStore for MemoryStore {
    fn kind(&self) -> &'static str {
        "memory"
    }

    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>, StoreError>> {
        Box::pin(async move { Ok(self.get_now(key)) })
    }

    fn put<'a>(&'a self, key: &'a str, value: Bytes, ttl: Duration) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.put_now(key, value, ttl);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_kept_for_its_lifetime() {
        let s = MemoryStore::new(1 << 20);
        s.put_now("a", Bytes::from_static(b"one"), Duration::from_secs(60));
        assert_eq!(s.get_now("a"), Some(Bytes::from_static(b"one")));
        s.put_now("b", Bytes::from_static(b"two"), Duration::ZERO);
        assert_eq!(s.get_now("b"), None, "expired");
        assert_eq!(s.len(), 1, "an expired answer is dropped when it is asked for");
    }

    #[test]
    fn the_oldest_answers_go_when_it_is_full() {
        let s = MemoryStore::new(100);
        for n in 0..10 {
            s.put_now(&format!("k{n}"), Bytes::from(vec![0u8; 20]), Duration::from_secs(60));
        }
        assert!(s.get_now("k0").is_none() && s.get_now("k5").is_none());
        assert!(s.get_now("k9").is_some() && s.get_now("k8").is_some());
        assert!(s.inner.lock().bytes <= 100);
        s.put_now("big", Bytes::from(vec![0u8; 200]), Duration::from_secs(60));
        assert!(s.get_now("big").is_none(), "larger than the whole store");
    }

    #[test]
    fn writing_a_key_again_replaces_it() {
        let s = MemoryStore::new(60);
        s.put_now("a", Bytes::from(vec![1u8; 20]), Duration::from_secs(60));
        s.put_now("b", Bytes::from(vec![2u8; 20]), Duration::from_secs(60));
        s.put_now("a", Bytes::from(vec![3u8; 20]), Duration::from_secs(60));
        // `a`'s first place in the queue is passed over: `b` is the oldest now.
        s.put_now("c", Bytes::from(vec![4u8; 20]), Duration::from_secs(60));
        assert_eq!(s.get_now("a").map(|v| v[0]), Some(3));
        assert!(s.get_now("b").is_none());
        assert!(s.get_now("c").is_some());
    }
}
