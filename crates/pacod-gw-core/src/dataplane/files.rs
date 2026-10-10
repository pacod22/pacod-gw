//! Ownership of provider objects on the request path (concept section 8.3).
//!
//! Anthropic tells files apart by credential only, and credentials are usually shared between
//! workspaces. A request that names a file id is therefore checked against the gateway's
//! record of who created it: an id another workspace created is refused, and so is an id the
//! gateway does not know when the call would go out on the shared credential.
//!
//! Apart from the file and batch endpoints themselves (`objects.rs`), this is the one place
//! where the request path reads the database: only for a body that names a file id, which is
//! rare. The check runs on what the provider receives, after templates and translation.

use std::collections::BTreeSet;

use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use super::Gw;
use super::auth::Caller;
use super::errors::GwError;
use crate::snapshot::CredentialOwner;

/// Every `file_id` value in a JSON body, wherever it is: document, image and container-upload
/// blocks, and anything a newer API puts one in. Keys are read as the provider reads them:
/// escapes decoded (`"file\u005fid"`), and every occurrence of a key that appears twice, since
/// it is not known which one the provider would use.
pub fn file_ids(body: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    // Most bodies name no file: a plain key is found by its bytes, an escaped one needs `\u`.
    if memchr::memmem::find(body, b"\"file_id\"").is_none() && memchr::memmem::find(body, b"\\u").is_none() {
        return out;
    }
    let mut de = serde_json::Deserializer::from_slice(body);
    // A body that is not JSON goes nowhere: the provider refuses it.
    let _ = Collect(&mut out).deserialize(&mut de);
    out
}

/// Walks any JSON value, collecting the strings under `file_id` keys.
struct Collect<'a>(&'a mut BTreeSet<String>);

impl<'de> DeserializeSeed<'de> for Collect<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Collect<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("JSON")
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let out = self.0;
        while seq.next_element_seed(Collect(&mut *out))?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let out = self.0;
        while let Some(key) = map.next_key::<String>()? {
            if key == "file_id" {
                match map.next_value::<Value>()? {
                    Value::String(id) => {
                        out.insert(id);
                    }
                    other => walk(&other, out),
                }
            } else {
                map.next_value_seed(Collect(&mut *out))?;
            }
        }
        Ok(())
    }
}

/// The same walk over a value already parsed (one under a `file_id` key that is not a string).
fn walk(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                match (k.as_str(), v) {
                    ("file_id", Value::String(id)) => {
                        out.insert(id.clone());
                    }
                    _ => walk(v, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
        _ => {}
    }
}

/// Refuses a body that names a file the caller's workspace may not use. `owner` is whose
/// credential the call goes out on.
pub async fn check(gw: &Gw, caller: &Caller, owner: CredentialOwner, body: &[u8]) -> Result<(), GwError> {
    let ids = file_ids(body);
    if ids.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = ids.iter().cloned().collect();
    let owners = gw.config.store().object_owners(&list).await.map_err(|_| {
        GwError::unavailable(
            "file_check_unavailable",
            "the gateway could not check which workspace owns the referenced files",
        )
    })?;
    for id in &ids {
        match owners.get(id) {
            Some(ws) if *ws == caller.workspace.id => {}
            // Said as "not found", so that another workspace's ids are not confirmed.
            Some(_) => return Err(not_found(id)),
            None if owner == CredentialOwner::Shared => return Err(not_found(id)),
            None => {}
        }
    }
    Ok(())
}

fn not_found(id: &str) -> GwError {
    GwError::not_found(
        "file_not_found",
        format!("the file \"{}\" is not one of this workspace's files", id.chars().take(100).collect::<String>()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_ids_are_found_wherever_they_are() {
        let body = br#"{"model":"m","messages":[{"role":"user","content":[
            {"type":"document","source":{"type":"file","file_id":"file_a"}},
            {"type":"image","source":{"type":"file","file_id":"file_b"}},
            {"type":"container_upload","file_id":"file_c"},
            {"type":"text","text":"mentions \"file_id\" in prose"}]}]}"#;
        assert_eq!(file_ids(body).into_iter().collect::<Vec<_>>(), ["file_a", "file_b", "file_c"]);
        assert!(file_ids(br#"{"model":"m","messages":[]}"#).is_empty());
    }

    #[test]
    fn escaped_and_repeated_keys_do_not_hide_a_file() {
        // The key as the provider reads it, whatever its escapes.
        let escaped = br#"{"source":{"type":"file","file\u005fid":"file_a"}}"#;
        assert_eq!(file_ids(escaped).into_iter().collect::<Vec<_>>(), ["file_a"]);
        let all = br#"{"source":{"\u0066\u0069\u006c\u0065\u005f\u0069\u0064":"file_b"}}"#;
        assert_eq!(file_ids(all).into_iter().collect::<Vec<_>>(), ["file_b"]);
        // Both values of a key given twice, as either may be the one used.
        let twice = br#"{"source":{"file_id":"file_theirs","file_id":"file_mine"}}"#;
        assert_eq!(file_ids(twice).into_iter().collect::<Vec<_>>(), ["file_mine", "file_theirs"]);
        // Text with escapes but no file costs a parse and finds nothing.
        assert!(file_ids(br#"{"messages":[{"role":"user","content":"caf\u00e9"}]}"#).is_empty());
    }
}
