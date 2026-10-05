//! Streamed response bodies: frames, and the scripted ways of delivering or breaking them.

use std::{collections::VecDeque, io, time::Duration};

use axum::body::Body;
use bytes::{Bytes, BytesMut};
use futures_util::stream;
use serde_json::Value;
use tokio::sync::watch;

use super::{behavior::Behavior, server::wait_shutdown};

/// How long a broken stream waits before the connection is dropped, so that the chunks sent
/// before the break reach the client first (the server writes them out while the body is idle).
const FLUSH_GRACE: Duration = Duration::from_millis(20);

/// One unit of a streamed body as the provider would write it: an SSE event or an NDJSON line.
pub(crate) struct Frame {
    pub bytes: Bytes,
    /// Whether the frame carries a piece of the model's output (text, reasoning or tool
    /// arguments). `reset_after` and `stall_after` count these.
    pub content: bool,
}

impl Frame {
    /// A frame that carries a piece of the model's output.
    pub fn content(bytes: impl Into<Bytes>) -> Self {
        Self { bytes: bytes.into(), content: true }
    }

    /// A frame of protocol structure: starts, stops, pings, usage, terminators.
    pub fn meta(bytes: impl Into<Bytes>) -> Self {
        Self { bytes: bytes.into(), content: false }
    }
}

/// One server-sent event with `\n` line ends, as OpenAI and Anthropic write them.
pub(crate) fn sse(event: Option<&str>, data: &Value) -> Bytes {
    let mut text = String::new();
    if let Some(event) = event {
        text.push_str("event: ");
        text.push_str(event);
        text.push('\n');
    }
    text.push_str("data: ");
    text.push_str(&data.to_string());
    text.push_str("\n\n");
    text.into()
}

/// How a stream ends once its chunks are written.
enum End {
    Complete,
    /// Fail the body, which makes the server drop the connection mid-response.
    Abort,
    /// Keep the connection open and send nothing more.
    Stall,
}

struct Delivery {
    chunks: VecDeque<Bytes>,
    chunk_delay: Option<Duration>,
    started: bool,
    end: End,
    shutdown: watch::Receiver<bool>,
}

/// Builds the response body for `frames` under the stream options of `behavior`.
///
/// `reset_after` / `stall_after` cut the frames right after the n-th content frame (the smaller
/// n wins; a stream with fewer content frames completes normally), `split` then re-chunks the
/// remaining bytes, and `chunk_delay` spaces the chunks that are written.
pub(crate) fn body(frames: Vec<Frame>, behavior: &Behavior, shutdown: watch::Receiver<bool>) -> Body {
    let cut = match (behavior.reset_after, behavior.stall_after) {
        (Some(reset), Some(stall)) if stall < reset => Some((stall, End::Stall)),
        (Some(reset), _) => Some((reset, End::Abort)),
        (None, Some(stall)) => Some((stall, End::Stall)),
        (None, None) => None,
    };
    let (frames, end) = match cut {
        Some((limit, end)) => match cut_after(&frames, limit) {
            Some(keep) => (&frames[..keep], end),
            None => (&frames[..], End::Complete),
        },
        None => (&frames[..], End::Complete),
    };

    let chunks = match behavior.split {
        Some(size) => {
            let mut all = BytesMut::new();
            for frame in frames {
                all.extend_from_slice(&frame.bytes);
            }
            let all = all.freeze();
            (0..all.len()).step_by(size).map(|start| all.slice(start..all.len().min(start + size))).collect()
        }
        None => frames.iter().map(|frame| frame.bytes.clone()).collect(),
    };

    let delivery = Delivery { chunks, chunk_delay: behavior.chunk_delay, started: false, end, shutdown };
    Body::from_stream(stream::unfold(delivery, |mut delivery| async move {
        if let Some(chunk) = delivery.chunks.pop_front() {
            if delivery.started
                && let Some(delay) = delivery.chunk_delay
            {
                tokio::time::sleep(delay).await;
            }
            delivery.started = true;
            return Some((Ok(chunk), delivery));
        }
        match std::mem::replace(&mut delivery.end, End::Complete) {
            End::Complete => None,
            End::Abort => {
                tokio::time::sleep(FLUSH_GRACE).await;
                Some((Err(broken("reset_after")), delivery))
            }
            End::Stall => {
                wait_shutdown(delivery.shutdown.clone()).await;
                Some((Err(broken("stall_after")), delivery))
            }
        }
    }))
}

/// The number of leading frames to keep so that exactly `limit` content frames are sent, or
/// `None` when the stream has fewer content frames than that.
fn cut_after(frames: &[Frame], limit: usize) -> Option<usize> {
    if limit == 0 {
        return Some(0);
    }
    frames.iter().enumerate().filter(|(_, frame)| frame.content).nth(limit - 1).map(|(index, _)| index + 1)
}

fn broken(option: &str) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, format!("mock: stream broken by {option}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> Vec<Frame> {
        vec![Frame::meta("start"), Frame::content("a"), Frame::meta("ping"), Frame::content("b"), Frame::meta("stop")]
    }

    #[test]
    fn cut_keeps_everything_up_to_the_nth_content_frame() {
        assert_eq!(cut_after(&frames(), 0), Some(0));
        assert_eq!(cut_after(&frames(), 1), Some(2));
        assert_eq!(cut_after(&frames(), 2), Some(4));
        assert_eq!(cut_after(&frames(), 3), None);
    }

    #[test]
    fn sse_frames_are_newline_terminated() {
        let data = serde_json::json!({ "a": 1 });
        assert_eq!(&sse(None, &data)[..], b"data: {\"a\":1}\n\n");
        assert_eq!(&sse(Some("ping"), &data)[..], b"event: ping\ndata: {\"a\":1}\n\n");
    }
}
