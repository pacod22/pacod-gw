//! Attachments of audit records (concept section 14.6): images, documents and audio in the
//! bodies of a record kept in full are stored once per workspace and content, in
//! `audit_attachments`, and the record refers to them by hash. A conversation that sends the
//! same image with every turn stores it once.
//!
//! The work is done on the text of the bodies, not on parsed JSON, so that a record reads back
//! byte for byte: a long base64 payload inside a string literal is replaced by a marker that
//! names its hash, and the marker by the payload again when the record is read.

use std::collections::HashMap;

/// A string literal at least this long is looked at.
const MIN_LITERAL: usize = 2048;

/// What stands in a body in place of an attachment: the marker and 64 hex digits.
const MARKER: &str = "pgw-attachment:sha256:";

/// One attachment taken out of a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// SHA-256 of the payload, in hex.
    pub hash: String,
    pub content: String,
}

fn looks_encoded(payload: &str) -> bool {
    !payload.is_empty()
        && payload.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'))
}

/// Takes the long base64 payloads out of the string literals of `body`, each replaced by a
/// marker naming its hash, and adds them to `out`. `None` when there is nothing to take out.
pub fn extract(body: &str, out: &mut Vec<Attachment>) -> Option<String> {
    let bytes = body.as_bytes();
    let mut result = String::with_capacity(body.len());
    let mut copied = 0;
    let mut i = 0;
    let mut changed = false;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        // A string literal from here to the next unescaped quote.
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() && bytes[j] != b'"' {
            j += if bytes[j] == b'\\' { 2 } else { 1 };
        }
        let end = j.min(bytes.len());
        if end - start >= MIN_LITERAL {
            let literal = &body[start..end];
            // A data URL keeps its prefix; only the payload goes.
            let offset = literal.find(";base64,").map_or(0, |p| p + ";base64,".len());
            let payload = &literal[offset..];
            if looks_encoded(payload) {
                let hash = hex::encode(ring::digest::digest(&ring::digest::SHA256, payload.as_bytes()));
                result.push_str(&body[copied..start + offset]);
                result.push_str(MARKER);
                result.push_str(&hash);
                copied = end;
                changed = true;
                if !out.iter().any(|a| a.hash == hash) {
                    out.push(Attachment { hash, content: payload.to_owned() });
                }
            }
        }
        i = end + 1;
    }
    if !changed {
        return None;
    }
    result.push_str(&body[copied..]);
    Some(result)
}

/// Puts the payloads back in place of their markers. A marker whose attachment is missing
/// stays as it is.
pub fn restore(body: &str, found: &HashMap<String, String>) -> String {
    if !body.contains(MARKER) {
        return body.to_owned();
    }
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find(MARKER) {
        out.push_str(&rest[..at]);
        let after = &rest[at + MARKER.len()..];
        let hash = after.get(..64).unwrap_or_default();
        match found.get(hash) {
            Some(content) => {
                out.push_str(content);
                rest = &after[hash.len()..];
            }
            None => {
                out.push_str(MARKER);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_reads_back_byte_for_byte() {
        let image = "iVBORw0KGgo".repeat(300);
        let body = format!(
            "{{\"messages\": [{{\"type\":\"image\",\"source\":{{\"data\":\"{image}\"}}}},\n  {{\"url\":\"data:image/png;base64,{image}\"}}, \"short \\\" quote\"]}}"
        );
        let mut found = Vec::new();
        let stored = extract(&body, &mut found).expect("taken out");
        assert_eq!(found.len(), 1, "the same image twice is one attachment");
        assert!(stored.len() < 400, "{stored}");
        assert!(stored.contains("data:image/png;base64,pgw-attachment:sha256:"));
        let back: HashMap<String, String> = found.into_iter().map(|a| (a.hash, a.content)).collect();
        assert_eq!(restore(&stored, &back), body);
    }

    #[test]
    fn short_strings_and_text_are_left_alone() {
        let mut found = Vec::new();
        assert_eq!(extract(r#"{"a":"short","b":"not base64 at all, with spaces"}"#, &mut found), None);
        let prose = format!("{{\"text\":\"{}\"}}", "words and more words ".repeat(200));
        assert_eq!(extract(&prose, &mut found), None);
        assert!(found.is_empty());
        assert_eq!(restore("pgw-attachment:sha256:unknown", &HashMap::new()), "pgw-attachment:sha256:unknown");
    }
}
