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
    strings_under(body, &["file_id"])
}

/// Every string under one of `keys` in a JSON body, read as [`file_ids`] reads them.
pub fn strings_under(body: &[u8], keys: &[&str]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    // Most bodies name none: a plain key is found by its bytes, an escaped one needs `\u`.
    let named = keys.iter().any(|k| memchr::memmem::find(body, format!("\"{k}\"").as_bytes()).is_some());
    if !named && memchr::memmem::find(body, b"\\u").is_none() {
        return out;
    }
    let mut de = serde_json::Deserializer::from_slice(body);
    // A body that is not JSON goes nowhere: the provider refuses it.
    let _ = Collect(&mut out, keys).deserialize(&mut de);
    out
}

/// Gemini's own objects a body can name: uploaded files (`fileData.fileUri`) and context
/// caches (`cachedContent`), in both spellings the REST API accepts.
const GEMINI_REFERENCES: &[&str] =
    &["fileUri", "file_uri", "cachedContent", "cached_content", "fileSearchStoreNames", "file_search_store_names"];

/// Vertex AI's ways to reach a project's data beyond the prompt: retrieval from Vertex AI
/// Search or a RAG corpus, as tools or tool configuration, in both spellings.
const VERTEX_RETRIEVAL: &[&str] = &[
    "retrieval",
    "retrievalConfig",
    "retrieval_config",
    "vertexAiSearch",
    "vertex_ai_search",
    "vertexRagStore",
    "vertex_rag_store",
];

/// Refuses a body for Gemini that names a file or a context cache of the Gemini API on the
/// installation's shared credential, where it would reach other workspaces' objects (concept
/// section 8.3). A workspace's or organisation's own credential reaches only its own. On
/// Vertex AI (`vertex`), whose token opens a whole project, also Cloud Storage objects
/// (`gs://`), any `projects/…` resource, and retrieval from the project's data stores; and on
/// any credential, model routing (`routingConfig`), which would serve a model the path does not
/// name.
pub fn check_gemini(owner: CredentialOwner, body: &[u8], vertex: bool) -> Result<(), GwError> {
    // Vertex AI's model routing picks a model by itself, past the gateway's grants and prices.
    if vertex && let Some(key) = ["routingConfig", "routing_config"].into_iter().find(|k| has_key(body, k)) {
        return Err(GwError::not_allowed(format!(
            "\"{key}\" lets Vertex AI choose the model: name the model in the path instead"
        )));
    }
    if owner != CredentialOwner::Shared {
        return Ok(());
    }
    if vertex && let Some(key) = VERTEX_RETRIEVAL.iter().find(|k| has_key(body, k)) {
        return Err(GwError::own_credential_required(format!(
            "\"{key}\" reaches the data stores of the installation's Google Cloud project: it is used only with a \
             credential of this workspace or organisation"
        )));
    }
    let named = strings_under(body, GEMINI_REFERENCES);
    let object = named.iter().find(|s| {
        let s = s.trim();
        s.starts_with("files/")
            || s.starts_with("cachedContents/")
            || s.starts_with("fileSearchStores/")
            || (s.contains("generativelanguage.googleapis.com/")
                && (s.contains("/files/") || s.contains("/cachedContents/") || s.contains("/fileSearchStores/")))
            || (vertex
                && (s.starts_with("gs://")
                    || s.starts_with("projects/")
                    || (s.contains("googleapis.com/") && s.contains("/projects/"))))
    });
    match object {
        Some(name) => Err(GwError::own_credential_required(format!(
            "\"{name}\" is a file, context cache or file search store of the Gemini API: it is used only with a \
             credential of this workspace or organisation, not the installation's shared one"
        ))),
        None => Ok(()),
    }
}

/// Whether a JSON body has a member named `key` anywhere, escaped spellings included.
fn has_key(body: &[u8], key: &str) -> bool {
    if memchr::memmem::find(body, key.as_bytes()).is_none() && memchr::memmem::find(body, b"\\u").is_none() {
        return false;
    }
    fn walk(v: &serde_json::Value, key: &str) -> bool {
        match v {
            serde_json::Value::Object(m) => m.iter().any(|(k, v)| k == key || walk(v, key)),
            serde_json::Value::Array(a) => a.iter().any(|v| walk(v, key)),
            _ => false,
        }
    }
    serde_json::from_slice::<serde_json::Value>(body).is_ok_and(|v| walk(&v, key))
}

/// Walks any JSON value, collecting the strings under the given keys.
struct Collect<'a>(&'a mut BTreeSet<String>, &'a [&'a str]);

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
        let (out, keys) = (self.0, self.1);
        while seq.next_element_seed(Collect(&mut *out, keys))?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let (out, keys) = (self.0, self.1);
        while let Some(key) = map.next_key::<String>()? {
            if keys.contains(&key.as_str()) {
                match map.next_value::<Value>()? {
                    Value::String(id) => {
                        out.insert(id);
                    }
                    // A list of names (`fileSearchStoreNames`).
                    Value::Array(items) => {
                        for item in items {
                            match item {
                                Value::String(id) => drop(out.insert(id)),
                                other => walk(&other, out, keys),
                            }
                        }
                    }
                    other => walk(&other, out, keys),
                }
            } else {
                map.next_value_seed(Collect(&mut *out, keys))?;
            }
        }
        Ok(())
    }
}

/// The same walk over a value already parsed (one under a key that is not a string).
fn walk(v: &Value, out: &mut BTreeSet<String>, keys: &[&str]) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                match v {
                    Value::String(id) if keys.contains(&k.as_str()) => {
                        out.insert(id.clone());
                    }
                    Value::Array(items) if keys.contains(&k.as_str()) => {
                        for item in items {
                            match item {
                                Value::String(id) => drop(out.insert(id.clone())),
                                other => walk(other, out, keys),
                            }
                        }
                    }
                    _ => walk(v, out, keys),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| walk(i, out, keys)),
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
    // Without a database there is no record of who uploaded a file: the provider's own
    // credential may use whatever it holds, the shared one nothing it cannot account for.
    let Some(store) = gw.config.store() else {
        return match (owner, ids.iter().next()) {
            (CredentialOwner::Shared, Some(id)) => Err(not_found(id)),
            _ => Ok(()),
        };
    };
    let list: Vec<String> = ids.iter().cloned().collect();
    let owners = store.object_owners(&list).await.map_err(|_| {
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
    fn gemini_files_and_caches_need_an_own_credential() {
        let file = br#"{"contents":[{"parts":[{"fileData":{"mimeType":"application/pdf","fileUri":"https://generativelanguage.googleapis.com/v1beta/files/abc123"}}]}]}"#;
        let cache = br#"{"cachedContent":"cachedContents/xyz","contents":[]}"#;
        let snake = br#"{"contents":[{"parts":[{"file_data":{"file_uri":"files/abc"}}]}]}"#;
        let escaped = br#"{"cached\u0043ontent":"cachedContents/xyz"}"#;
        let store = br#"{"contents":[],"tools":[{"fileSearch":{"fileSearchStoreNames":["fileSearchStores/theirs"]}}]}"#;
        for body in [&file[..], cache, snake, escaped, store] {
            let e = check_gemini(CredentialOwner::Shared, body, false).unwrap_err();
            assert_eq!(e.kind, "own_credential_required", "{}", String::from_utf8_lossy(body));
            assert!(check_gemini(CredentialOwner::Own, body, false).is_ok());
        }
        // A public URL is no object of the API.
        let video = br#"{"contents":[{"parts":[{"fileData":{"fileUri":"https://www.youtube.com/watch?v=x"}}]}]}"#;
        assert!(check_gemini(CredentialOwner::Shared, video, false).is_ok());
        assert!(check_gemini(CredentialOwner::Shared, br#"{"contents":[]}"#, false).is_ok());
    }

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

    #[test]
    fn vertex_keeps_the_projects_data_to_own_credentials() {
        let shared = |body: &[u8]| check_gemini(CredentialOwner::Shared, body, true);
        for body in [
            &br#"{"contents":[{"parts":[{"fileData":{"fileUri":"gs://bucket/secret.pdf"}}]}]}"#[..],
            br#"{"contents":[],"cached_content":"projects/p/locations/l/cachedContents/c"}"#,
            br#"{"systemInstruction":{"parts":[{"file_data":{"file_uri":"https://storage.googleapis.com/x/projects/p/o"}}]}}"#,
            br#"{"tools":[{"retrieval":{"vertexAiSearch":{"datastore":"d"}}}]}"#,
            br#"{"toolConfig":{"retrievalConfig":{}}}"#,
            br#"{"tools":[{"\u0072etrieval":{}}]}"#,
        ] {
            assert!(shared(body).is_err(), "{}", String::from_utf8_lossy(body));
            assert!(check_gemini(CredentialOwner::Own, body, true).is_ok());
        }
        // A public document and a plain prompt pass; so does the same on AI Studio.
        assert!(
            shared(br#"{"contents":[{"parts":[{"fileData":{"fileUri":"https://example.com/projects/report.pdf"}}]}]}"#)
                .is_ok()
        );
        assert!(shared(br#"{"contents":[{"parts":[{"text":"the retrieval of projects/x"}]}]}"#).is_ok());
        assert!(
            check_gemini(CredentialOwner::Shared, br#"{"contents":[],"cachedContent":"projects/p"}"#, false).is_ok()
        );
        // Model routing chooses a model the path does not name: refused on any credential.
        for owner in [CredentialOwner::Shared, CredentialOwner::Own] {
            let routed =
                br#"{"contents":[],"generationConfig":{"routing_config":{"manualMode":{"modelName":"gemini-pro"}}}}"#;
            assert_eq!(check_gemini(owner, routed, true).unwrap_err().kind, "not_allowed");
        }
    }
}
