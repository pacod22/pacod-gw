//! `multipart/form-data` bodies (RFC 7578): image edits, transcriptions, file uploads.
//!
//! The gateway reads a form to find its model and options, and changes as little as it can:
//! [`Form::rewrite`] replaces or removes named parts and keeps every other byte, the
//! boundary included, as the same-dialect rule asks of JSON bodies. The body is held whole
//! already (for failover), so the parts are ranges into it rather than copies.

use std::ops::Range;

use bytes::{Bytes, BytesMut};

use crate::error::DialectError;

/// The most parts a form may have.
const MAX_PARTS: usize = 64;
/// The most bytes of headers one part may have.
const MAX_HEAD: usize = 8 << 10;

/// One part of a form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    /// From the part's delimiter line to the next delimiter: what removing the part removes.
    pub span: Range<usize>,
    /// The part's content.
    pub body: Range<usize>,
}

/// A parsed form: its boundary and its parts, as ranges into the body it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub boundary: String,
    pub parts: Vec<Part>,
    /// Where the closing delimiter starts; a part added by [`Form::rewrite`] goes there.
    end: usize,
}

/// A change to a form: a part's new content, or its removal.
#[derive(Debug, Clone, Copy)]
pub enum Edit<'a> {
    Set(&'a [u8]),
    Remove,
}

/// A part of a form the gateway writes itself.
#[derive(Debug, Clone)]
pub struct OwnedPart {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub body: Bytes,
}

/// The boundary a `Content-Type` names, without quotes.
pub fn boundary_of(content_type: &str) -> Option<String> {
    let (kind, params) = content_type.split_once(';')?;
    if !kind.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    param(params, "boundary").filter(|b| !b.is_empty() && b.len() <= 70)
}

/// A parameter of a header value (`name="x"; filename="y"`), unquoted.
fn param(params: &str, key: &str) -> Option<String> {
    let mut rest = params;
    while !rest.is_empty() {
        let (name, after) = rest.split_once('=')?;
        let name = name.trim().trim_start_matches(';').trim();
        let after = after.trim_start();
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            // A quoted value may hold `;`; a backslash escapes the next character.
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, n)) = chars.next() {
                            value.push(n);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => value.push(c),
                }
            }
            (value, &quoted[end..])
        } else {
            let end = after.find(';').unwrap_or(after.len());
            (after[..end].trim().to_owned(), &after[end..])
        };
        if name.eq_ignore_ascii_case(key) {
            return Some(value);
        }
        rest = next.trim_start().trim_start_matches(';');
    }
    None
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    memchr::memmem::find(hay.get(from..)?, needle).map(|i| i + from)
}

impl Form {
    /// Reads a form. `content_type` is the request's header; the body is the whole request.
    pub fn parse(content_type: &str, body: &[u8]) -> Result<Form, DialectError> {
        let boundary = boundary_of(content_type)
            .ok_or_else(|| DialectError::invalid("the request must be multipart/form-data with a boundary"))?;
        let delimiter = format!("--{boundary}");
        let delimiter = delimiter.as_bytes();
        // The first delimiter may open the body or follow a preamble on a line of its own.
        let mut at = if body.starts_with(delimiter) {
            0
        } else {
            find(body, &[b"\r\n", delimiter].concat(), 0)
                .map(|i| i + 2)
                .ok_or_else(|| DialectError::invalid("the form has no part"))?
        };
        let mut parts = Vec::new();
        loop {
            let line_end = at + delimiter.len();
            if body.get(line_end..line_end + 2) == Some(b"--") {
                return Ok(Form { boundary, parts, end: at });
            }
            if body.get(line_end..line_end + 2) != Some(b"\r\n") {
                return Err(DialectError::invalid("a form delimiter is not followed by a line end"));
            }
            if parts.len() == MAX_PARTS {
                return Err(DialectError::invalid(format!("a form may have at most {MAX_PARTS} parts")));
            }
            let head_start = line_end + 2;
            let head_end = find(body, b"\r\n\r\n", head_start)
                .filter(|e| e - head_start <= MAX_HEAD)
                .ok_or_else(|| DialectError::invalid("a part of the form has no end to its headers"))?;
            let content = head_end + 4;
            let next = find(body, &[b"\r\n", delimiter].concat(), content)
                .ok_or_else(|| DialectError::invalid("the form is not closed"))?;
            let head = std::str::from_utf8(&body[head_start..head_end])
                .map_err(|_| DialectError::invalid("the headers of a part are not text"))?;
            let mut part = Part {
                name: String::new(),
                filename: None,
                content_type: None,
                span: at..next + 2,
                body: content..next,
            };
            for line in head.split("\r\n") {
                let Some((name, value)) = line.split_once(':') else { continue };
                if name.trim().eq_ignore_ascii_case("content-disposition") {
                    let params = value.split_once(';').map_or("", |(_, p)| p);
                    part.name = param(params, "name").unwrap_or_default();
                    part.filename = param(params, "filename");
                } else if name.trim().eq_ignore_ascii_case("content-type") {
                    part.content_type = Some(value.trim().to_owned());
                }
            }
            if part.name.is_empty() {
                return Err(DialectError::invalid("a part of the form has no name"));
            }
            parts.push(part);
            at = next + 2;
        }
    }

    /// The first part named `name`.
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name == name)
    }

    /// Every part named `name` (`image[]` holds several images).
    pub fn parts<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Part> + 'a {
        self.parts.iter().filter(move |p| p.name == name)
    }

    /// The content of a part that is text, trimmed.
    pub fn text<'a>(&self, body: &'a [u8], name: &str) -> Option<&'a str> {
        self.part(name).and_then(|p| std::str::from_utf8(&body[p.body.clone()]).ok()).map(str::trim)
    }

    /// The body with `edits` applied: a named part's content replaced (or the part added at
    /// the end, when there is none), or the part removed. Every other byte is kept.
    pub fn rewrite(&self, body: &[u8], edits: &[(&str, Edit<'_>)]) -> Bytes {
        let mut out = BytesMut::with_capacity(body.len() + 64);
        let first = self.parts.first().map_or(self.end, |p| p.span.start);
        out.extend_from_slice(&body[..first]);
        for p in &self.parts {
            match edits.iter().find(|(n, _)| *n == p.name).map(|(_, e)| *e) {
                Some(Edit::Remove) => {}
                Some(Edit::Set(value)) => {
                    out.extend_from_slice(&body[p.span.start..p.body.start]);
                    out.extend_from_slice(value);
                    out.extend_from_slice(&body[p.body.end..p.span.end]);
                }
                None => out.extend_from_slice(&body[p.span.clone()]),
            }
        }
        for (name, edit) in edits {
            if let Edit::Set(value) = edit
                && self.part(name).is_none()
            {
                write_part(
                    &mut out,
                    &self.boundary,
                    &OwnedPart {
                        name: (*name).to_owned(),
                        filename: None,
                        content_type: None,
                        body: Bytes::copy_from_slice(value),
                    },
                );
            }
        }
        out.extend_from_slice(&body[self.end..]);
        out.freeze()
    }
}

fn quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn write_part(out: &mut BytesMut, boundary: &str, part: &OwnedPart) {
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    let mut disposition = format!("Content-Disposition: form-data; name=\"{}\"", quoted(&part.name));
    if let Some(f) = &part.filename {
        disposition.push_str(&format!("; filename=\"{}\"", quoted(f)));
    }
    out.extend_from_slice(disposition.as_bytes());
    out.extend_from_slice(b"\r\n");
    if let Some(t) = &part.content_type {
        out.extend_from_slice(format!("Content-Type: {t}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&part.body);
    out.extend_from_slice(b"\r\n");
}

/// A form of the gateway's own, with `boundary`.
pub fn write(boundary: &str, parts: &[OwnedPart]) -> Bytes {
    let mut out = BytesMut::new();
    for p in parts {
        write_part(&mut out, boundary, p);
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out.freeze()
}

/// The `Content-Type` of a form with `boundary`.
pub fn content_type(boundary: &str) -> String {
    format!("multipart/form-data; boundary={boundary}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CT: &str = "multipart/form-data; boundary=XyZ";

    fn sample() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"preamble\r\n--XyZ\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper\r\n");
        b.extend_from_slice(b"--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a;b.wav\"\r\nContent-Type: audio/wav\r\n\r\n");
        // Binary content with a line end and something like a delimiter that is not one.
        b.extend_from_slice(b"RIFF\r\n--XyQ\0\xff");
        b.extend_from_slice(b"\r\n--XyZ--\r\nepilogue");
        b
    }

    #[test]
    fn a_form_is_read_into_named_parts() {
        let body = sample();
        let f = Form::parse(CT, &body).unwrap();
        assert_eq!(f.boundary, "XyZ");
        assert_eq!(f.parts.len(), 2);
        assert_eq!(f.text(&body, "model"), Some("whisper"));
        let file = f.part("file").unwrap();
        assert_eq!((file.filename.as_deref(), file.content_type.as_deref()), (Some("a;b.wav"), Some("audio/wav")));
        assert_eq!(&body[file.body.clone()], b"RIFF\r\n--XyQ\0\xff");
    }

    #[test]
    fn a_rewrite_changes_only_what_it_names() {
        let body = sample();
        let f = Form::parse(CT, &body).unwrap();
        let out = f.rewrite(&body, &[("model", Edit::Set(b"whisper-1"))]);
        let expected = String::from_utf8_lossy(&body).replace("\r\n\r\nwhisper\r\n", "\r\n\r\nwhisper-1\r\n");
        assert_eq!(String::from_utf8_lossy(&out), expected);
        // Unchanged, the bytes are the same.
        assert_eq!(&f.rewrite(&body, &[])[..], &body[..]);
        // Removed, and added.
        let out = f.rewrite(&body, &[("model", Edit::Remove), ("response_format", Edit::Set(b"verbose_json"))]);
        let g = Form::parse(CT, &out).unwrap();
        assert_eq!(g.parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["file", "response_format"]);
        assert_eq!(g.text(&out, "response_format"), Some("verbose_json"));
        assert_eq!(&out[g.part("file").unwrap().body.clone()], b"RIFF\r\n--XyQ\0\xff");
        assert!(out.ends_with(b"--XyZ--\r\nepilogue"));
    }

    #[test]
    fn a_written_form_reads_back() {
        let parts = vec![
            OwnedPart {
                name: "purpose".into(),
                filename: None,
                content_type: None,
                body: Bytes::from_static(b"batch"),
            },
            OwnedPart {
                name: "file".into(),
                filename: Some("in \"put\".jsonl".into()),
                content_type: Some("application/jsonl".into()),
                body: Bytes::from_static(b"{}\n{}\n"),
            },
        ];
        let out = write("gw-boundary", &parts);
        let f = Form::parse(&content_type("gw-boundary"), &out).unwrap();
        assert_eq!(f.text(&out, "purpose"), Some("batch"));
        let file = f.part("file").unwrap();
        assert_eq!(file.filename.as_deref(), Some("in \"put\".jsonl"));
        assert_eq!(&out[file.body.clone()], b"{}\n{}\n");
    }

    #[test]
    fn broken_forms_are_refused() {
        for (ct, body) in [
            ("application/json", &b"{}"[..]),
            (CT, b"no delimiter at all"),
            (CT, b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nnever closed"),
            (CT, b"--XyZ\r\nContent-Disposition: form-data\r\n\r\nx\r\n--XyZ--"),
            (CT, b"--XyZ\r\nno end of headers"),
        ] {
            assert!(Form::parse(ct, body).is_err(), "{}", String::from_utf8_lossy(body));
        }
        assert_eq!(boundary_of("multipart/form-data; charset=utf-8; boundary=\"a b\""), Some("a b".into()));
        let many: Vec<u8> = (0..65)
            .flat_map(|i| format!("--XyZ\r\nContent-Disposition: form-data; name=\"p{i}\"\r\n\r\nx\r\n").into_bytes())
            .chain(b"--XyZ--".iter().copied())
            .collect();
        assert!(Form::parse(CT, &many).is_err());
    }
}
