//! One strict pass over a JSON object that records where each top-level member lies.
//!
//! The whole body is validated against RFC 8259, so the gateway and the provider cannot
//! disagree about what a body means: comments, single quotes, trailing commas, `NaN`, a byte
//! order mark and trailing bytes are all rejected, as is a repeated top-level key.

use std::ops::Range;

use memchr::memchr2;

/// Nesting deeper than this is rejected.
const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

/// One top-level member of the scanned object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The key with escapes decoded.
    pub key: String,
    /// Offset of the key's opening quote.
    pub key_start: usize,
    /// The bytes of the value.
    pub value: Range<usize>,
    pub kind: Kind,
}

/// The top-level layout of a JSON object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopLevel {
    pub entries: Vec<Entry>,
    /// Offset of the opening brace.
    pub open: usize,
    /// Offset of the closing brace.
    pub close: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScanError {
    #[error("body is not valid UTF-8")]
    NotUtf8,
    #[error("body must be a JSON object")]
    NotAnObject,
    #[error("invalid JSON at byte {0}")]
    Invalid(usize),
    #[error("JSON is nested too deeply")]
    TooDeep,
    #[error("the key \"{0}\" appears more than once")]
    DuplicateKey(String),
}

impl TopLevel {
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// The raw bytes of a member's value.
    pub fn raw<'a>(&self, body: &'a [u8], key: &str) -> Option<&'a [u8]> {
        self.get(key).map(|e| &body[e.value.clone()])
    }

    /// A string member, decoded. `None` when absent or not a string.
    pub fn str(&self, body: &[u8], key: &str) -> Option<String> {
        let e = self.get(key)?;
        (e.kind == Kind::String).then(|| serde_json::from_slice::<String>(&body[e.value.clone()]).ok()).flatten()
    }

    /// A boolean member. `None` when absent or not a boolean.
    pub fn bool(&self, body: &[u8], key: &str) -> Option<bool> {
        let e = self.get(key)?;
        match &body[e.value.clone()] {
            b"true" => Some(true),
            b"false" => Some(false),
            _ => None,
        }
    }

    /// A non-negative integer member. `None` when absent, not a number, negative or fractional.
    pub fn u64(&self, body: &[u8], key: &str) -> Option<u64> {
        let e = self.get(key)?;
        (e.kind == Kind::Number)
            .then(|| std::str::from_utf8(&body[e.value.clone()]).ok()?.parse::<u64>().ok())
            .flatten()
    }

    /// A numeric member as a float. `None` when absent or not a number.
    pub fn f64(&self, body: &[u8], key: &str) -> Option<f64> {
        let e = self.get(key)?;
        (e.kind == Kind::Number)
            .then(|| std::str::from_utf8(&body[e.value.clone()]).ok()?.parse::<f64>().ok())
            .flatten()
    }
}

/// Scans `body`, which must be exactly one JSON object with optional surrounding whitespace.
pub fn scan(body: &[u8]) -> Result<TopLevel, ScanError> {
    if std::str::from_utf8(body).is_err() {
        return Err(ScanError::NotUtf8);
    }
    let mut i = skip_ws(body, 0);
    if body.get(i) != Some(&b'{') {
        return Err(if i < body.len() && is_value_start(body[i]) {
            ScanError::NotAnObject
        } else {
            ScanError::Invalid(i)
        });
    }
    let open = i;
    i += 1;
    let mut entries: Vec<Entry> = Vec::new();

    i = skip_ws(body, i);
    if body.get(i) == Some(&b'}') {
        let close = i;
        expect_end(body, i + 1)?;
        return Ok(TopLevel { entries, open, close });
    }

    loop {
        // key
        if body.get(i) != Some(&b'"') {
            return Err(ScanError::Invalid(i));
        }
        let key_start = i;
        let (key_end, has_escape) = string_end(body, i)?;
        let key = if has_escape {
            serde_json::from_slice::<String>(&body[key_start..key_end]).map_err(|_| ScanError::Invalid(key_start))?
        } else {
            // Validated as UTF-8 above, and free of escapes: the bytes between the quotes are the key.
            String::from_utf8_lossy(&body[key_start + 1..key_end - 1]).into_owned()
        };
        if entries.iter().any(|e| e.key == key) {
            return Err(ScanError::DuplicateKey(key));
        }

        // colon
        i = skip_ws(body, key_end);
        if body.get(i) != Some(&b':') {
            return Err(ScanError::Invalid(i));
        }
        i = skip_ws(body, i + 1);

        // value
        let value_start = i;
        let (value_end, kind) = value(body, i, 1)?;
        entries.push(Entry { key, key_start, value: value_start..value_end, kind });

        // separator
        i = skip_ws(body, value_end);
        match body.get(i) {
            Some(b',') => {
                i = skip_ws(body, i + 1);
            }
            Some(b'}') => {
                let close = i;
                expect_end(body, i + 1)?;
                return Ok(TopLevel { entries, open, close });
            }
            _ => return Err(ScanError::Invalid(i)),
        }
    }
}

fn expect_end(body: &[u8], i: usize) -> Result<(), ScanError> {
    let i = skip_ws(body, i);
    if i == body.len() { Ok(()) } else { Err(ScanError::Invalid(i)) }
}

#[inline]
fn skip_ws(body: &[u8], mut i: usize) -> usize {
    while let Some(b) = body.get(i) {
        match b {
            b' ' | b'\n' | b'\r' | b'\t' => i += 1,
            _ => break,
        }
    }
    i
}

#[inline]
fn is_value_start(b: u8) -> bool {
    matches!(b, b'{' | b'[' | b'"' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n')
}

/// Validates the value starting at `i` and returns the offset just past it.
fn value(body: &[u8], i: usize, depth: usize) -> Result<(usize, Kind), ScanError> {
    if depth > MAX_DEPTH {
        return Err(ScanError::TooDeep);
    }
    match body.get(i) {
        Some(b'"') => Ok((string_end(body, i)?.0, Kind::String)),
        Some(b'{') => Ok((object_end(body, i, depth)?, Kind::Object)),
        Some(b'[') => Ok((array_end(body, i, depth)?, Kind::Array)),
        Some(b't') => literal(body, i, b"true").map(|e| (e, Kind::Bool)),
        Some(b'f') => literal(body, i, b"false").map(|e| (e, Kind::Bool)),
        Some(b'n') => literal(body, i, b"null").map(|e| (e, Kind::Null)),
        Some(b'-' | b'0'..=b'9') => Ok((number_end(body, i)?, Kind::Number)),
        _ => Err(ScanError::Invalid(i)),
    }
}

fn literal(body: &[u8], i: usize, word: &[u8]) -> Result<usize, ScanError> {
    if body.len() >= i + word.len() && &body[i..i + word.len()] == word {
        Ok(i + word.len())
    } else {
        Err(ScanError::Invalid(i))
    }
}

/// `i` points at the opening quote. Returns the offset just past the closing quote and
/// whether the string contains an escape.
fn string_end(body: &[u8], i: usize) -> Result<(usize, bool), ScanError> {
    let mut j = i + 1;
    let mut has_escape = false;
    loop {
        let Some(off) = memchr2(b'"', b'\\', &body[j..]) else {
            return Err(ScanError::Invalid(body.len()));
        };
        // Control characters must be escaped inside strings.
        if let Some(bad) = body[j..j + off].iter().position(|&b| b < 0x20) {
            return Err(ScanError::Invalid(j + bad));
        }
        j += off;
        if body[j] == b'"' {
            return Ok((j + 1, has_escape));
        }
        // backslash
        has_escape = true;
        match body.get(j + 1) {
            Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => j += 2,
            Some(b'u') => {
                let hex = body.get(j + 2..j + 6).ok_or(ScanError::Invalid(j))?;
                if !hex.iter().all(u8::is_ascii_hexdigit) {
                    return Err(ScanError::Invalid(j));
                }
                j += 6;
            }
            _ => return Err(ScanError::Invalid(j)),
        }
    }
}

fn number_end(body: &[u8], i: usize) -> Result<usize, ScanError> {
    let mut j = i;
    if body.get(j) == Some(&b'-') {
        j += 1;
    }
    match body.get(j) {
        Some(b'0') => j += 1,
        Some(b'1'..=b'9') => {
            j += 1;
            while matches!(body.get(j), Some(b'0'..=b'9')) {
                j += 1;
            }
        }
        _ => return Err(ScanError::Invalid(j)),
    }
    if body.get(j) == Some(&b'.') {
        j += 1;
        let start = j;
        while matches!(body.get(j), Some(b'0'..=b'9')) {
            j += 1;
        }
        if j == start {
            return Err(ScanError::Invalid(j));
        }
    }
    if matches!(body.get(j), Some(b'e' | b'E')) {
        j += 1;
        if matches!(body.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let start = j;
        while matches!(body.get(j), Some(b'0'..=b'9')) {
            j += 1;
        }
        if j == start {
            return Err(ScanError::Invalid(j));
        }
    }
    Ok(j)
}

fn object_end(body: &[u8], i: usize, depth: usize) -> Result<usize, ScanError> {
    let mut j = skip_ws(body, i + 1);
    if body.get(j) == Some(&b'}') {
        return Ok(j + 1);
    }
    loop {
        if body.get(j) != Some(&b'"') {
            return Err(ScanError::Invalid(j));
        }
        j = skip_ws(body, string_end(body, j)?.0);
        if body.get(j) != Some(&b':') {
            return Err(ScanError::Invalid(j));
        }
        j = skip_ws(body, j + 1);
        j = skip_ws(body, value(body, j, depth + 1)?.0);
        match body.get(j) {
            Some(b',') => j = skip_ws(body, j + 1),
            Some(b'}') => return Ok(j + 1),
            _ => return Err(ScanError::Invalid(j)),
        }
    }
}

fn array_end(body: &[u8], i: usize, depth: usize) -> Result<usize, ScanError> {
    let mut j = skip_ws(body, i + 1);
    if body.get(j) == Some(&b']') {
        return Ok(j + 1);
    }
    loop {
        j = skip_ws(body, value(body, j, depth + 1)?.0);
        match body.get(j) {
            Some(b',') => j = skip_ws(body, j + 1),
            Some(b']') => return Ok(j + 1),
            _ => return Err(ScanError::Invalid(j)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(body: &str) -> Vec<String> {
        scan(body.as_bytes()).unwrap().entries.into_iter().map(|e| e.key).collect()
    }

    #[test]
    fn locates_top_level_members() {
        let body =
            br#" {"model":"m-1", "stream" : true, "max_tokens":400,"messages":[{"role":"user","content":"hi"}]} "#;
        let top = scan(body).unwrap();
        assert_eq!(top.entries.len(), 4);
        assert_eq!(top.str(body, "model").as_deref(), Some("m-1"));
        assert_eq!(top.bool(body, "stream"), Some(true));
        assert_eq!(top.u64(body, "max_tokens"), Some(400));
        assert_eq!(top.raw(body, "messages").unwrap(), br#"[{"role":"user","content":"hi"}]"#);
        assert_eq!(top.get("messages").unwrap().kind, Kind::Array);
        assert_eq!(body[top.open], b'{');
        assert_eq!(body[top.close], b'}');
    }

    #[test]
    fn empty_object() {
        let top = scan(b"{}").unwrap();
        assert!(top.entries.is_empty());
        assert_eq!((top.open, top.close), (0, 1));
        assert!(scan(b" { } \n").unwrap().entries.is_empty());
    }

    #[test]
    fn governed_key_inside_content_is_not_top_level() {
        // A "model" key nested in messages must not be mistaken for the top-level one.
        let body = br#"{"messages":[{"model":"evil","stream":true}],"model":"real"}"#;
        let top = scan(body).unwrap();
        assert_eq!(top.str(body, "model").as_deref(), Some("real"));
        assert_eq!(top.entries.len(), 2);
    }

    #[test]
    fn braces_and_quotes_inside_strings() {
        let body = br#"{"a":"}\"{[,]","b":"x\\","c":"y\\\"z","model":"m"}"#;
        assert_eq!(keys(std::str::from_utf8(body).unwrap()), ["a", "b", "c", "model"]);
        let top = scan(body).unwrap();
        assert_eq!(top.str(body, "b").as_deref(), Some("x\\"));
        assert_eq!(top.str(body, "c").as_deref(), Some("y\\\"z"));
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        assert_eq!(scan(br#"{"model":"a","model":"b"}"#), Err(ScanError::DuplicateKey("model".into())));
    }

    #[test]
    fn duplicate_through_escape_is_rejected() {
        // The second key spells "model" with a unicode escape for the "e".
        let esc = "\\u";
        let dup = format!(r#"{{"model":"a","mod{esc}0065l":"b"}}"#);
        assert!(dup.contains("\\u0065"), "the test input must carry the escape, not the letter");
        assert_eq!(scan(dup.as_bytes()), Err(ScanError::DuplicateKey("model".into())));
        let single = format!(r#"{{"mod{esc}0065l":"b"}}"#);
        assert_eq!(keys(&single), ["model"]);
    }

    #[test]
    fn rejects_what_a_lenient_parser_might_accept() {
        for bad in [
            &br#"{"a":1,}"#[..],        // trailing comma
            br#"{"a":1} x"#,            // trailing bytes
            br#"{"a":NaN}"#,            // not a JSON number
            br#"{"a":01}"#,             // leading zero
            br#"{"a":1.}"#,             // empty fraction
            br#"{"a":+1}"#,             // plus sign
            br#"{'a':1}"#,              // single quotes
            br#"{"a":1 /* c */}"#,      // comment
            br#"{"a":"x"#,              // unterminated string
            br#"{"a":"\x"}"#,           // bad escape
            br#"{"a":"\u12G4"}"#,       // bad unicode escape
            b"{\"a\":\"line\nbreak\"}", // raw control character in a string
            b"\xEF\xBB\xBF{}",          // byte order mark
            br#"{"a":tru}"#,
            br#"{"a"}"#,
            br#"{"a":}"#,
            br#"{,}"#,
            b"",
            b"   ",
        ] {
            assert!(
                matches!(scan(bad), Err(ScanError::Invalid(_))),
                "should reject {:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn rejects_non_objects_and_bad_utf8() {
        assert_eq!(scan(b"[1,2]"), Err(ScanError::NotAnObject));
        assert_eq!(scan(b"\"x\""), Err(ScanError::NotAnObject));
        assert_eq!(scan(b"{\"a\":\"\xff\"}"), Err(ScanError::NotUtf8));
    }

    #[test]
    fn depth_is_capped() {
        let deep = format!("{{\"a\":{}{}}}", "[".repeat(200), "]".repeat(200));
        assert_eq!(scan(deep.as_bytes()), Err(ScanError::TooDeep));
        let ok = format!("{{\"a\":{}{}}}", "[".repeat(100), "]".repeat(100));
        assert!(scan(ok.as_bytes()).is_ok());
    }

    #[test]
    fn typed_reads_reject_wrong_types() {
        let body = br#"{"n":-3,"f":1.5,"s":"7","t":null,"big":18446744073709551615}"#;
        let top = scan(body).unwrap();
        assert_eq!(top.u64(body, "n"), None); // negative
        assert_eq!(top.u64(body, "f"), None); // fractional
        assert_eq!(top.u64(body, "s"), None); // a string, not a number
        assert_eq!(top.f64(body, "f"), Some(1.5));
        assert_eq!(top.bool(body, "t"), None);
        assert_eq!(top.u64(body, "big"), Some(u64::MAX));
        assert_eq!(top.str(body, "missing"), None);
    }

    #[test]
    fn large_string_values_are_skipped() {
        let blob = "A".repeat(10 * 1024 * 1024);
        let body = format!(r#"{{"messages":[{{"data":"{blob}"}}],"model":"m"}}"#);
        let top = scan(body.as_bytes()).unwrap();
        assert_eq!(top.str(body.as_bytes(), "model").as_deref(), Some("m"));
    }

    #[test]
    fn unicode_content() {
        let body = "{\"model\":\"m\",\"messages\":[{\"content\":\"Здравей 🌍 \\ud83d\\ude00\"}]}";
        assert_eq!(keys(body), ["model", "messages"]);
    }
}
