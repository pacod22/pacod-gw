//! The response body of a relayed call.
//!
//! Each chunk from the provider is returned to the client connection as it arrives. A
//! reference-counted copy is kept and fed to the usage tap on the next poll, after the client
//! side has the chunk. There is no task and no channel per stream: the body is polled by the
//! client connection, and dropping it (the client went away) drops the provider connection,
//! which cancels the call upstream. The recorder inside settles in every case.
//!
//! Managed mode can put a [`Transform`] between the provider and the client. Proxy mode never
//! does: there the bytes that arrive are the bytes that leave.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use hyper::body::Incoming;
use pacod_gw_events::usage::status;
use tokio::time::Sleep;

use super::recorder::CallRecorder;

/// Errors of the relayed body, surfaced to the client connection.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("the provider's stream broke: {0}")]
    Upstream(String),
    #[error("the provider sent nothing for {0:?}")]
    Idle(Duration),
    #[error("the provider did not finish within the time allowed")]
    Deadline,
}

/// Rewrites a stream on its way to the client. Works on whole events: what it returns for a
/// chunk may be empty when the chunk ends inside an event.
pub trait Transform: Send {
    /// Takes a chunk from the provider and returns what goes to the client now.
    fn chunk(&mut self, chunk: &Bytes, rec: &mut CallRecorder) -> Bytes;

    /// The provider's stream ended. Returns what is still to be sent.
    fn end(&mut self, rec: &mut CallRecorder) -> Bytes;

    /// The provider's stream broke or went silent. Returns an error event in the caller's
    /// dialect; it is sent and the stream is then closed in an orderly way, so that the
    /// caller's SDK reports the error instead of a broken connection.
    fn error(&mut self, kind: &str, message: &str, rec: &mut CallRecorder) -> Bytes;
}

pub struct RelayBody {
    inner: Incoming,
    recorder: Option<CallRecorder>,
    transform: Option<Box<dyn Transform>>,
    /// The chunk returned by the previous poll, still to be fed to the tap.
    pending: Option<Bytes>,
    /// Set once the body has produced everything it will.
    done: bool,
    /// The length the provider declared for its body, when it declared one.
    declared_len: Option<u64>,
    /// Bytes received from the provider so far.
    received: u64,
    idle: Pin<Box<Sleep>>,
    idle_timeout: Duration,
    /// When the whole response must be complete, for calls that are not streams.
    deadline: Option<Instant>,
    /// When the provider last sent something; the idle timer is re-armed lazily against it.
    last_chunk: Instant,
}

impl RelayBody {
    pub fn new(inner: Incoming, recorder: CallRecorder, idle_timeout: Duration) -> Self {
        Self {
            declared_len: inner.size_hint().exact(),
            received: 0,
            inner,
            recorder: Some(recorder),
            transform: None,
            pending: None,
            done: false,
            idle: Box::pin(tokio::time::sleep(idle_timeout)),
            idle_timeout,
            deadline: None,
            last_chunk: Instant::now(),
        }
    }

    /// Ends the body with a timeout when the response is not complete by `deadline`.
    pub fn with_deadline(mut self, deadline: Option<Instant>) -> Self {
        if let Some(d) = deadline {
            let first = d.min(Instant::now() + self.idle_timeout);
            self.idle.as_mut().reset(tokio::time::Instant::from_std(first));
        }
        self.deadline = deadline;
        self
    }

    /// Puts a transform between the provider and the client.
    pub fn with_transform(mut self, transform: Box<dyn Transform>) -> Self {
        self.transform = Some(transform);
        self
    }

    fn feed_pending(&mut self) {
        if let (Some(chunk), Some(rec)) = (self.pending.take(), self.recorder.as_mut()) {
            let t = Instant::now();
            if let Some(tap) = rec.tap.as_mut() {
                tap.feed(&chunk);
            }
            rec.timeline.chunk_spent(t.elapsed());
        }
    }

    fn end(&mut self, outcome: Option<(&str, &str, String)>) {
        self.feed_pending();
        if let Some(mut rec) = self.recorder.take() {
            match outcome {
                Some((st, kind, msg)) => {
                    let http = rec.event.http_status;
                    rec.outcome(st, http, kind, &msg);
                }
                None => rec.complete_from_http(),
            }
            rec.timeline.client_chunk(Instant::now());
            rec.finish();
        }
    }

    /// Ends the body after a failure. With a transform the caller gets an error event and an
    /// orderly end; without one the connection is broken, which is all a relay can do.
    fn fail(
        &mut self,
        st: &str,
        kind: &'static str,
        msg: String,
        err: RelayError,
    ) -> Poll<Option<Result<Frame<Bytes>, RelayError>>> {
        self.feed_pending();
        let event = match (self.transform.as_mut(), self.recorder.as_mut()) {
            (Some(t), Some(rec)) => t.error(kind, &msg, rec),
            _ => Bytes::new(),
        };
        self.end(Some((st, kind, msg)));
        self.done = true;
        if self.transform.is_some() && !event.is_empty() {
            return Poll::Ready(Some(Ok(Frame::data(event))));
        }
        Poll::Ready(Some(Err(err)))
    }
}

impl Drop for RelayBody {
    fn drop(&mut self) {
        // A response with a known length is not polled again after its last chunk: the
        // connection drops the body instead. If the provider's body is complete, the call
        // finished; otherwise the client went away and the recorder settles it as cancelled.
        // Over HTTP/2 the end of the stream can arrive in a frame of its own, after the last
        // byte, so the declared length decides where there is one.
        let complete = self.inner.is_end_stream() || self.declared_len.is_some_and(|n| self.received >= n);
        if self.recorder.is_some() && complete {
            self.end(None);
        } else {
            self.feed_pending();
        }
    }
}

impl Body for RelayBody {
    type Data = Bytes;
    type Error = RelayError;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, RelayError>>> {
        let this = &mut *self;
        loop {
            // The client side already has the previous chunk: account for it now.
            this.feed_pending();
            if this.done {
                return Poll::Ready(None);
            }

            match Pin::new(&mut this.inner).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    let now = Instant::now();
                    this.last_chunk = now;
                    let data = match frame.into_data() {
                        Ok(data) => data,
                        // Trailers belong to the provider's framing; a transformed stream has its own.
                        Err(_) if this.transform.is_some() => continue,
                        Err(frame) => return Poll::Ready(Some(Ok(frame))),
                    };
                    this.received += data.len() as u64;
                    let Some(rec) = this.recorder.as_mut() else { return Poll::Ready(Some(Ok(Frame::data(data)))) };
                    rec.response_bytes += data.len() as u64;
                    rec.timeline.upstream_chunk(now);
                    let out = match this.transform.as_mut() {
                        Some(t) => {
                            let out = t.chunk(&data, rec);
                            rec.timeline.chunk_spent(now.elapsed());
                            out
                        }
                        None => data.clone(),
                    };
                    this.pending = Some(data);
                    if out.is_empty() {
                        // The chunk ended inside an event: nothing for the client yet.
                        continue;
                    }
                    rec.timeline.client_chunk(if this.transform.is_some() { Instant::now() } else { now });
                    return Poll::Ready(Some(Ok(Frame::data(out))));
                }
                Poll::Ready(Some(Err(e))) => {
                    let msg = e.to_string();
                    return this.fail(status::ERROR, "stream", msg.clone(), RelayError::Upstream(msg));
                }
                Poll::Ready(None) => {
                    let tail = match (this.transform.as_mut(), this.recorder.as_mut()) {
                        (Some(t), Some(rec)) => t.end(rec),
                        _ => Bytes::new(),
                    };
                    this.end(None);
                    this.done = true;
                    if !tail.is_empty() {
                        return Poll::Ready(Some(Ok(Frame::data(tail))));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    // The timer is only re-armed when it fires early, which keeps the
                    // per-chunk path free of timer work.
                    if this.idle.as_mut().poll(cx).is_ready() {
                        let idle_at = this.last_chunk + this.idle_timeout;
                        let (at, whole) = match this.deadline {
                            Some(d) if d <= idle_at => (d, true),
                            _ => (idle_at, false),
                        };
                        if Instant::now() >= at {
                            let d = this.idle_timeout;
                            return if whole {
                                let msg = "the provider did not finish within the time allowed".to_owned();
                                this.fail(status::TIMEOUT, "upstream_timeout", msg, RelayError::Deadline)
                            } else {
                                this.fail(
                                    status::TIMEOUT,
                                    "upstream_timeout",
                                    format!("no data for {d:?}"),
                                    RelayError::Idle(d),
                                )
                            };
                        }
                        this.idle.as_mut().reset(tokio::time::Instant::from_std(at));
                        // Register the waker with the re-armed timer.
                        let _ = this.idle.as_mut().poll(cx);
                    }
                    return Poll::Pending;
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        if self.transform.is_some() {
            // The transform may still have something to say after the provider's last byte.
            return self.done;
        }
        self.inner.is_end_stream() && self.pending.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        if self.transform.is_some() { SizeHint::default() } else { self.inner.size_hint() }
    }
}
