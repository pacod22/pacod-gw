//! A small server-sent-events reader for the watch stream: `event:` and `data:` lines, events
//! ended by a blank line, comments (`:`, the keep-alives) ignored.

use crate::client::{Error, Result};
use crate::types::WatchEvent;

#[derive(Default)]
pub(crate) struct Parser {
    buf: Vec<u8>,
    event: String,
    data: String,
}

impl Parser {
    /// Feeds bytes; returns the events they completed, as (event, data).
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<(String, String)> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() || !self.event.is_empty() {
                    out.push((std::mem::take(&mut self.event), std::mem::take(&mut self.data)));
                }
            } else if let Some(v) = line.strip_prefix("event:") {
                self.event = v.trim_start().to_string();
            } else if let Some(v) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(v.strip_prefix(' ').unwrap_or(v));
            }
        }
        out
    }
}

/// The watch event an SSE event carries; `None` for kinds the client does not know.
pub(crate) fn to_event(event: &str, data: &str) -> Option<Result<WatchEvent>> {
    let v: serde_json::Value = serde_json::from_str(data).unwrap_or(serde_json::Value::Null);
    let parsed = match event {
        "hello" => Ok(WatchEvent::Hello { generation: v["generation"].as_u64().unwrap_or(0) }),
        "resync" => Ok(WatchEvent::Resync),
        "upserted" | "removed" | "provider" | "generation" => {
            serde_json::from_value::<WatchEvent>(v).map_err(|e| Error::Decode(format!("watch event {event}: {e}")))
        }
        _ => return None,
    };
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_across_chunks() {
        let mut p = Parser::default();
        assert!(p.push(b"event: hello\nda").is_empty());
        let out = p.push(b"ta: {\"generation\": 4}\n\n:keep-alive\n\nevent: upserted\ndata: {\"type\":\"upserted\",\"resource_id\":\"r\",\"provider_id\":\"p\"}\n\n");
        assert_eq!(out.len(), 2);
        assert_eq!(to_event(&out[0].0, &out[0].1).unwrap().unwrap(), WatchEvent::Hello { generation: 4 });
        assert_eq!(
            to_event(&out[1].0, &out[1].1).unwrap().unwrap(),
            WatchEvent::Upserted { resource_id: "r".into(), provider_id: "p".into() }
        );
        assert!(to_event("something-new", "{}").is_none());
    }
}
