//! The response cache on a call (`docs/plugin-concept.md`, section 15).
//!
//! Once a call is planned, the workspace's answering binding (the response cache plugin) says
//! whether the call may be answered from a kept answer and whether its own answer is kept, and
//! under which key. The core does the rest: it looks the key up in the store, bounded by the
//! binding's time; a hit is written in the caller's dialect by the managed path; on a miss the
//! provider's answer is captured as it is relayed, and kept when the call settles well, in a
//! task of its own, off the request path.
//!
//! The cache never fails a call. A store that cannot answer is a miss, whatever the binding's
//! `on_error` says, and an answer that cannot be kept is simply not kept.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use pacod_gw_dialects::ProviderStream;
use pacod_gw_dialects::assemble::Assembler;
use pacod_gw_dialects::canonical::{Event, Response};
use pacod_gw_dialects::sse::SseFramer;
use pacod_gw_events::usage::cache_result;
use pacod_gw_plugin::Keep;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ActiveBinding;
use crate::answers::{AnswerStore, StoreError, store_key};
use crate::dataplane::Gw;
use crate::model::PluginMode;
use crate::telemetry::metrics;

/// The form an answer is kept in. Raised when it changes; older answers then read as misses.
const FORMAT: u32 = 1;

/// An answer as the store keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Kept {
    pub v: u32,
    pub response: Response,
    /// The request id (`gw_…`) of the call whose answer this is.
    pub call_id: String,
    /// When it was kept, in seconds since the epoch.
    pub stored_at: u64,
}

impl Kept {
    /// Its age now, in seconds.
    pub fn age(&self) -> u64 {
        now_seconds().saturating_sub(self.stored_at)
    }

    /// What the audit record says about the answer a call was served from.
    pub fn audit(&self, result: &str) -> Value {
        json!({ "result": result, "source_call_id": self.call_id, "stored_at": self.stored_at, "age_s": self.age() })
    }
}

fn now_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// What the store gave for a call.
pub enum Found {
    /// A kept answer, and the result the event records: `hit`, or `would_hit` for a binding in
    /// shadow mode, whose call goes to the provider all the same.
    Kept(Box<Kept>, &'static str),
    /// Nothing usable: not there, unreadable, or the store failed or was too slow.
    Nothing,
}

/// Looks for the answer kept for `keep` in the workspace, within the binding's time.
pub async fn lookup(gw: &Gw, binding: &ActiveBinding, workspace_id: &str, keep: &Keep) -> Found {
    let store = gw.answers.as_ref();
    let started = Instant::now();
    let key = store_key(workspace_id, &keep.key);
    let got = tokio::time::timeout(binding.timeout, store.get(&key)).await;
    metrics::record_response_cache_lookup(store.kind(), started.elapsed());
    let bytes = match got {
        Ok(Ok(Some(b))) => b,
        Ok(Ok(None)) => return Found::Nothing,
        Ok(Err(StoreError::Unavailable)) => {
            metrics::record_response_cache_error(store.kind(), "skipped");
            return Found::Nothing;
        }
        Ok(Err(e)) => {
            metrics::record_response_cache_error(store.kind(), "get");
            tracing::debug!(error = %e, store = store.kind(), "the response cache could not be read");
            return Found::Nothing;
        }
        Err(_) => {
            metrics::record_response_cache_error(store.kind(), "get");
            tracing::debug!(store = store.kind(), "the response cache did not answer in time");
            return Found::Nothing;
        }
    };
    match serde_json::from_slice::<Kept>(&bytes) {
        Ok(k) if k.v == FORMAT => {
            let result = if binding.mode == PluginMode::Shadow { cache_result::WOULD_HIT } else { cache_result::HIT };
            Found::Kept(Box::new(k), result)
        }
        _ => Found::Nothing,
    }
}

/// How the answer of a call is being captured, to be kept.
enum Capture {
    /// Not yet: the answer is whole and handed over by the managed path.
    Waiting,
    Whole(Box<Response>),
    /// A stream, read as it is relayed.
    Stream(Box<StreamCapture>),
    /// Larger than what is kept.
    TooLarge,
}

/// The provider's stream, read into a complete answer as it is relayed.
struct StreamCapture {
    framer: SseFramer,
    reader: Box<dyn ProviderStream>,
    events: Vec<Event>,
    assembler: Assembler,
    bytes: usize,
}

impl StreamCapture {
    fn feed(&mut self, chunk: &[u8]) {
        let Self { framer, reader, events, assembler, .. } = self;
        framer.feed(chunk, |ev| reader.read(&ev.event, &ev.data, events));
        for e in events.drain(..) {
            assembler.push(&e);
        }
    }

    fn finish(mut self) -> Option<Response> {
        let Self { framer, reader, events, assembler, .. } = &mut self;
        framer.finish(|ev| reader.read(&ev.event, &ev.data, events));
        reader.finish(events);
        for e in events.drain(..) {
            assembler.push(&e);
        }
        self.assembler.finish()
    }
}

/// Where a call's answer goes once the call has settled well.
struct Pending {
    store: Arc<dyn AnswerStore>,
    key: String,
    keep: Keep,
    binding: Arc<ActiveBinding>,
    capture: Capture,
}

/// What the response cache does with one call. Lives in its recorder.
#[derive(Default)]
pub struct AnswerState {
    pending: Option<Pending>,
    /// The age of the kept answer a hit was served from, for the `Age` header.
    pub age: Option<u64>,
}

impl std::fmt::Debug for AnswerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnswerState").field("keeps", &self.pending.is_some()).field("age", &self.age).finish()
    }
}

impl AnswerState {
    /// The call's answer is to be kept under `keep`, once captured.
    pub fn keep(&mut self, gw: &Gw, binding: Arc<ActiveBinding>, workspace_id: &str, keep: Keep) {
        self.pending = Some(Pending {
            store: gw.answers.clone(),
            key: store_key(workspace_id, &keep.key),
            keep,
            binding,
            capture: Capture::Waiting,
        });
    }

    /// Whether an answer is wanted: the managed path reads a whole answer only then.
    pub fn wants(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| matches!(p.capture, Capture::Waiting))
    }

    /// The whole answer, as read from the provider.
    pub fn whole(&mut self, response: Response) {
        if let Some(p) = self.pending.as_mut() {
            p.capture = Capture::Whole(Box::new(response));
        }
    }

    /// The answer is a stream: `reader` reads the provider's events as they are relayed.
    pub fn stream(&mut self, reader: Box<dyn ProviderStream>) {
        if let Some(p) = self.pending.as_mut() {
            p.capture = Capture::Stream(Box::new(StreamCapture {
                framer: SseFramer::new(),
                reader,
                events: Vec::new(),
                assembler: Assembler::new(),
                bytes: 0,
            }));
        }
    }

    /// A chunk of the provider's stream, after the client has it.
    pub fn feed(&mut self, chunk: &[u8]) {
        let Some(p) = self.pending.as_mut() else { return };
        let Capture::Stream(s) = &mut p.capture else { return };
        // A stream carries its answer in many small events, several times its size. Past four
        // times what is kept the answer will not fit, and holding more would only cost memory.
        s.bytes += chunk.len();
        if s.bytes > p.keep.max_bytes.saturating_mul(4) {
            p.capture = Capture::TooLarge;
            return;
        }
        s.feed(chunk);
    }

    /// The call settled: a call that ended well, with an answer worth keeping, has its answer
    /// written to the store in a task of its own. `ok` says whether it ended well.
    pub fn settle(&mut self, ok: bool, call_id: String) {
        let Some(p) = self.pending.take() else { return };
        if !ok {
            return;
        }
        let response = match p.capture {
            Capture::Whole(r) => Some(*r),
            Capture::Stream(s) => s.finish(),
            Capture::Waiting | Capture::TooLarge => None,
        };
        let Some(response) = response else {
            metrics::record_response_cache("not_kept");
            return;
        };
        let keeps = p.binding.bound.as_ref().is_ok_and(|b| b.keeps(&response));
        let encoded = serde_json::to_vec(&Kept { v: FORMAT, response, call_id, stored_at: now_seconds() });
        let value = match encoded {
            Ok(v) if keeps && v.len() <= p.keep.max_bytes => Bytes::from(v),
            _ => {
                metrics::record_response_cache("not_kept");
                return;
            }
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        let (store, key, ttl) = (p.store, p.key, p.keep.ttl);
        rt.spawn(async move {
            // A write that takes long is given up: the next call simply misses.
            match tokio::time::timeout(Duration::from_secs(2), store.put(&key, value, ttl)).await {
                Ok(Ok(())) => metrics::record_response_cache("stored"),
                Ok(Err(StoreError::Unavailable)) => metrics::record_response_cache_error(store.kind(), "skipped"),
                Ok(Err(e)) => {
                    metrics::record_response_cache_error(store.kind(), "put");
                    tracing::debug!(error = %e, store = store.kind(), "an answer could not be kept");
                }
                Err(_) => metrics::record_response_cache_error(store.kind(), "put"),
            }
        });
    }
}
