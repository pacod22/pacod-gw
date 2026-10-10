//! Ownership of provider objects on the request path (concept section 8.3).
//!
//! Anthropic tells files apart by credential only, and credentials are usually shared between
//! workspaces. A request that names a file id is therefore checked against the gateway's
//! record of who created it: an id another workspace created is refused, and so is an id the
//! gateway does not know when the call would go out on the shared credential.
//!
//! This is the one place where the request path reads the database: only for a request whose
//! body names a file id, which is rare. The check runs on what the provider receives, after
//! templates and translation.

use std::collections::BTreeSet;

use serde_json::Value;

use super::Gw;
use super::auth::Caller;
use super::errors::GwError;
use crate::snapshot::CredentialOwner;

/// Every `file_id` value in a JSON body, wherever it is: document, image and container-upload
/// blocks, and anything a newer API puts one in.
pub fn file_ids(body: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if memchr::memmem::find(body, b"\"file_id\"").is_none() {
        return out;
    }
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
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        walk(&v, &mut out);
    }
    out
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
}
