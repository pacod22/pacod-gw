//! Incremental framing of server-sent events and newline-delimited JSON.
//!
//! The relay hands every provider chunk to the client unchanged. These framers work on a copy:
//! they find event boundaries across arbitrary chunk splits so that usage extractors and stream
//! translators see whole events.

use bytes::{Bytes, BytesMut};
use memchr::memchr;

/// One server-sent event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, empty when absent.
    pub event: String,
    /// The `data:` lines joined with newlines.
    pub data: Bytes,
}

/// Splits a byte stream into server-sent events.
#[derive(Debug, Default)]
pub struct SseFramer {
    buf: BytesMut,
}

impl SseFramer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk and calls `on_event` for every event it completes.
    pub fn feed(&mut self, chunk: &[u8], mut on_event: impl FnMut(SseEvent)) {
        self.buf.extend_from_slice(chunk);
        while let Some((end, sep_len)) = find_event_end(&self.buf) {
            let raw = self.buf.split_to(end + sep_len);
            if let Some(ev) = parse_event(&raw[..end]) {
                on_event(ev);
            }
        }
    }

    /// Like [`feed`](Self::feed), and also hands over each event's own bytes, including the
    /// blank line that ends it, so that a transformer can pass an event on untouched.
    pub fn feed_raw(&mut self, chunk: &[u8], mut on_event: impl FnMut(Bytes, Option<SseEvent>)) {
        self.buf.extend_from_slice(chunk);
        while let Some((end, sep_len)) = find_event_end(&self.buf) {
            let raw = self.buf.split_to(end + sep_len).freeze();
            let ev = parse_event(&raw[..end]);
            on_event(raw, ev);
        }
    }

    /// Takes what was received after the last complete event.
    pub fn take_pending(&mut self) -> Bytes {
        std::mem::take(&mut self.buf).freeze()
    }

    /// Bytes received after the last complete event. Not empty at the end of a stream means
    /// the stream was cut inside an event.
    pub fn pending(&self) -> &[u8] {
        &self.buf
    }

    /// Flushes a final event that was not terminated by a blank line.
    pub fn finish(&mut self, mut on_event: impl FnMut(SseEvent)) {
        if self.buf.iter().any(|b| !b.is_ascii_whitespace()) {
            let raw = std::mem::take(&mut self.buf);
            if let Some(ev) = parse_event(&raw) {
                on_event(ev);
            }
        }
        self.buf.clear();
    }
}

/// Finds the first blank line. Returns the offset where the event text ends and the length of
/// the separator (`\n\n`, `\r\n\r\n`, or a mix).
fn find_event_end(buf: &[u8]) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(off) = memchr(b'\n', &buf[from..]) {
        let nl = from + off;
        // The line ending that starts at `line_end` is followed by another line ending.
        match &buf[nl + 1..] {
            [b'\n', ..] => {
                let end = if nl > 0 && buf[nl - 1] == b'\r' { nl - 1 } else { nl };
                return Some((end, nl + 2 - end));
            }
            [b'\r', b'\n', ..] => {
                let end = if nl > 0 && buf[nl - 1] == b'\r' { nl - 1 } else { nl };
                return Some((end, nl + 3 - end));
            }
            _ => from = nl + 1,
        }
    }
    None
}

fn parse_event(raw: &[u8]) -> Option<SseEvent> {
    let mut event = String::new();
    let mut data = BytesMut::new();
    let mut has_data = false;
    for line in raw.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line[0] == b':' {
            continue; // blank or comment
        }
        let (field, value) = match memchr(b':', line) {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(b" ").unwrap_or(v))
            }
            None => (line, &b""[..]),
        };
        match field {
            b"event" => event = String::from_utf8_lossy(value).into_owned(),
            b"data" => {
                if has_data {
                    data.extend_from_slice(b"\n");
                }
                has_data = true;
                data.extend_from_slice(value);
            }
            _ => {}
        }
    }
    (has_data || !event.is_empty()).then(|| SseEvent { event, data: data.freeze() })
}

/// Splits a byte stream into lines, for newline-delimited JSON.
#[derive(Debug, Default)]
pub struct LineFramer {
    buf: BytesMut,
}

impl LineFramer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, chunk: &[u8], mut on_line: impl FnMut(&[u8])) {
        self.buf.extend_from_slice(chunk);
        while let Some(nl) = memchr(b'\n', &self.buf) {
            let line = self.buf.split_to(nl + 1);
            let line = &line[..nl];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if !line.is_empty() {
                on_line(line);
            }
        }
    }

    pub fn finish(&mut self, mut on_line: impl FnMut(&[u8])) {
        let rest = std::mem::take(&mut self.buf);
        let rest = rest.strip_suffix(b"\r").unwrap_or(&rest);
        if !rest.is_empty() {
            on_line(rest);
        }
    }
}

/// Writes one server-sent event.
pub fn write_event(out: &mut BytesMut, event: &str, data: &[u8]) {
    if !event.is_empty() {
        out.extend_from_slice(b"event: ");
        out.extend_from_slice(event.as_bytes());
        out.extend_from_slice(b"\n");
    }
    for line in data.split(|&b| b == b'\n') {
        out.extend_from_slice(b"data: ");
        out.extend_from_slice(line);
        out.extend_from_slice(b"\n");
    }
    out.extend_from_slice(b"\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut f = SseFramer::new();
        let mut out = Vec::new();
        for c in chunks {
            f.feed(c, |e| out.push(e));
        }
        f.finish(|e| out.push(e));
        out
    }

    const STREAM: &[u8] = b"event: message_start\ndata: {\"a\":1}\n\nevent: ping\ndata: {}\n\ndata: [DONE]\n\n";

    #[test]
    fn frames_whole_events() {
        let ev = collect(&[STREAM]);
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[0], SseEvent { event: "message_start".into(), data: Bytes::from_static(b"{\"a\":1}") });
        assert_eq!(ev[1].event, "ping");
        assert_eq!(&ev[2].data[..], b"[DONE]");
    }

    #[test]
    fn every_split_point_gives_the_same_events() {
        let whole = collect(&[STREAM]);
        for i in 0..=STREAM.len() {
            assert_eq!(collect(&[&STREAM[..i], &STREAM[i..]]), whole, "split at {i}");
        }
        // One byte at a time.
        let bytes: Vec<&[u8]> = STREAM.chunks(1).collect();
        assert_eq!(collect(&bytes), whole);
    }

    #[test]
    fn crlf_and_mixed_line_endings() {
        let s = b"event: a\r\ndata: 1\r\n\r\ndata: 2\n\r\ndata: 3\r\n\ndata: 4\n\n";
        let ev = collect(&[s]);
        assert_eq!(ev.iter().map(|e| e.data.to_vec()).collect::<Vec<_>>(), [b"1", b"2", b"3", b"4"]);
        assert_eq!(ev[0].event, "a");
    }

    #[test]
    fn multi_line_data_comments_and_no_space() {
        let s = b": keep-alive\n\ndata:first\ndata: second\n\n";
        let ev = collect(&[s]);
        assert_eq!(ev.len(), 1);
        assert_eq!(&ev[0].data[..], b"first\nsecond");
    }

    #[test]
    fn unterminated_final_event_is_flushed_and_reported() {
        let mut f = SseFramer::new();
        let mut out = Vec::new();
        f.feed(b"data: 1\n\ndata: 2", |e| out.push(e));
        assert_eq!(out.len(), 1);
        assert_eq!(f.pending(), b"data: 2");
        f.finish(|e| out.push(e));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn lines() {
        let mut f = LineFramer::new();
        let mut out: Vec<Vec<u8>> = Vec::new();
        for c in [&b"{\"a\":1}\n{\"b\""[..], b":2}\r\n", b"{\"c\":3}"] {
            f.feed(c, |l| out.push(l.to_vec()));
        }
        f.finish(|l| out.push(l.to_vec()));
        assert_eq!(out, [b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec(), b"{\"c\":3}".to_vec()]);
    }

    #[test]
    fn write_then_frame_round_trips() {
        let mut b = BytesMut::new();
        write_event(&mut b, "x", b"line1\nline2");
        write_event(&mut b, "", b"{}");
        let ev = collect(&[&b]);
        assert_eq!(ev[0], SseEvent { event: "x".into(), data: Bytes::from_static(b"line1\nline2") });
        assert_eq!(ev[1], SseEvent { event: String::new(), data: Bytes::from_static(b"{}") });
    }
}
