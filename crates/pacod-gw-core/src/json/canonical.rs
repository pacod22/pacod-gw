//! Canonical JSON: one value always gives the same bytes, whatever order its members came in.
//!
//! The workspace's `serde_json` keeps the order of object members (`preserve_order`), so writing
//! a value out gives back the order its sender chose. A hash of a tool definition or of a call's
//! arguments must not depend on that order: here object members are sorted by key, at every
//! depth, and nothing is written between tokens. Strings and numbers are written as `serde_json`
//! writes them.
//!
//! Keys are sorted by their UTF-8 bytes, which is not RFC 8785's order for characters outside
//! the Basic Multilingual Plane. It need not be: these bytes are only hashed, and the hashes are
//! compared only with hashes the gateway made itself.

use serde_json::Value;

/// The canonical bytes of `value`.
pub fn canonical(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

fn write(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Object(members) => {
            let mut keys: Vec<&String> = members.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                scalar(&Value::String(key.clone()), out);
                out.push(b':');
                write(&members[key], out);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write(item, out);
            }
            out.push(b']');
        }
        other => scalar(other, out),
    }
}

fn scalar(value: &Value, out: &mut Vec<u8>) {
    serde_json::to_writer(out, value).expect("a JSON scalar is written to memory");
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn member_order_does_not_matter_at_any_depth() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"y":[{"q":2,"p":1}],"x":"é"}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"x":"é","y":[{"p":1,"q":2}]},"b":1}"#).unwrap();
        assert_eq!(canonical(&a), canonical(&b));
        assert_eq!(String::from_utf8(canonical(&a)).unwrap(), r#"{"a":{"x":"é","y":[{"p":1,"q":2}]},"b":1}"#);
    }

    #[test]
    fn array_order_and_values_do_matter() {
        assert_ne!(canonical(&json!([1, 2])), canonical(&json!([2, 1])));
        assert_ne!(canonical(&json!({ "a": "1" })), canonical(&json!({ "a": 1 })));
        assert_eq!(canonical(&json!("quote \" and \\")), br#""quote \" and \\""#);
    }
}
