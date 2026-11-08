//! Ids that keep each workspace's tasks apart (catalogue MCP-15, runbook section 21.4).
//!
//! A remote agent makes its own task ids, and every caller reaches it with the organisation's
//! credential: to the agent they are one client, so any caller that learned another's task id
//! could read or cancel that task. The gateway therefore never shows a caller an agent's id.
//! It seals each one for the workspace (`gwt1.…`) and opens it again on the way in; an id of
//! another workspace, or one made up, does not open and is answered as an unknown task.
//!
//! Sealing is deterministic, so that one task keeps one id in every answer: AES-256-GCM with a
//! nonce derived from the plaintext (an HMAC), under keys derived from the key-encryption key
//! for the agent and the workspace. Opening checks that the nonce is the plaintext's, so a
//! sealed id cannot be altered into another.
//!
//! A context id the caller chose itself is sealed the other way (`gwc1.…`) before it reaches the
//! agent and opened in the answers, so that two workspaces choosing the same context id never
//! share a context at the agent, and each caller sees its own string.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::{hkdf, hmac};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::crypto::envelope::Kek;

/// What the keys are derived for. Raised with the form of the ids.
const SALT: &[u8] = b"pacod-gw a2a ids v1";
/// An id the agent made, sealed for one workspace.
pub const AGENT_ID: &str = "gwt1.";
/// A context id a caller chose, sealed for its workspace.
pub const CALLER_ID: &str = "gwc1.";

struct Keys {
    enc: LessSafeKey,
    mac: hmac::Key,
}

/// Seals and opens ids for one agent and one workspace.
pub struct Sealer {
    /// One set per key-encryption key, the current one first: ids are sealed with it and opened
    /// with any.
    keys: Vec<Keys>,
    aad: Vec<u8>,
}

impl Sealer {
    pub fn new(keks: &[Kek], agent_id: &str, workspace_id: &str) -> Option<Self> {
        let derive = |kek: &Kek, what: &[u8]| {
            let mut key = Zeroizing::new([0u8; 32]);
            kek.prk(SALT)
                .expand(&[what, agent_id.as_bytes(), b"/", workspace_id.as_bytes()], hkdf::HKDF_SHA256)
                .and_then(|okm| okm.fill(key.as_mut_slice()))
                .expect("32 bytes are within what HKDF-SHA256 gives");
            key
        };
        let keys: Vec<Keys> = keks
            .iter()
            .map(|kek| Keys {
                enc: LessSafeKey::new(
                    UnboundKey::new(&AES_256_GCM, derive(kek, b"enc").as_slice()).expect("32-byte key"),
                ),
                mac: hmac::Key::new(hmac::HMAC_SHA256, derive(kek, b"mac").as_slice()),
            })
            .collect();
        if keys.is_empty() {
            return None;
        }
        Some(Self { keys, aad: format!("{agent_id}/{workspace_id}").into_bytes() })
    }

    fn nonce(keys: &Keys, prefix: &str, plain: &str) -> [u8; 12] {
        let tag = hmac::sign(&keys.mac, &[prefix.as_bytes(), plain.as_bytes()].concat());
        tag.as_ref()[..12].try_into().expect("an HMAC-SHA256 tag has 32 bytes")
    }

    pub fn seal(&self, prefix: &str, plain: &str) -> String {
        let keys = &self.keys[0];
        let nonce = Self::nonce(keys, prefix, plain);
        let mut buf = plain.as_bytes().to_vec();
        keys.enc
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from([prefix.as_bytes(), &self.aad].concat()),
                &mut buf,
            )
            .expect("sealing in memory cannot fail");
        format!("{prefix}{}", URL_SAFE_NO_PAD.encode([nonce.as_slice(), &buf].concat()))
    }

    pub fn open(&self, prefix: &str, sealed: &str) -> Option<String> {
        let raw = URL_SAFE_NO_PAD.decode(sealed.strip_prefix(prefix)?).ok()?;
        if raw.len() < 12 + AES_256_GCM.tag_len() {
            return None;
        }
        let (nonce, ct) = raw.split_at(12);
        let nonce: [u8; 12] = nonce.try_into().ok()?;
        self.keys.iter().find_map(|keys| {
            let mut buf = ct.to_vec();
            let aad = Aad::from([prefix.as_bytes(), &self.aad].concat());
            let plain = keys.enc.open_in_place(Nonce::assume_unique_for_key(nonce), aad, &mut buf).ok()?;
            let plain = String::from_utf8(plain.to_vec()).ok()?;
            (Self::nonce(keys, prefix, &plain) == nonce).then_some(plain)
        })
    }
}

/// An id a caller sent that no workspace of its own was given.
#[derive(Debug, PartialEq, Eq)]
pub struct Unknown;

/// The ids of a request on their way to the agent: the agent's own, opened, and a caller's own
/// context, sealed. A task id that does not open is an unknown task.
pub fn inbound(method: &str, params: &mut Value, s: &Sealer) -> Result<(), Unknown> {
    let open = |v: &mut Value| -> Result<(), Unknown> {
        let text = v.as_str().ok_or(Unknown)?;
        *v = Value::String(s.open(AGENT_ID, text).ok_or(Unknown)?);
        Ok(())
    };
    match method {
        "GetTask" | "CancelTask" | "SubscribeToTask" => open(params.get_mut("id").ok_or(Unknown)?),
        _ => {
            // Fields are looked up, never indexed into: indexing would add the ones absent.
            let Some(message) = params.get_mut("message").and_then(Value::as_object_mut) else { return Ok(()) };
            if let Some(task) = message.get_mut("taskId").filter(|v| !v.is_null()) {
                open(task)?;
            }
            if let Some(context) = message.get("contextId").and_then(Value::as_str).map(str::to_owned) {
                let rewritten = match context.strip_prefix(AGENT_ID) {
                    Some(_) => s.open(AGENT_ID, &context).ok_or(Unknown)?,
                    None => s.seal(CALLER_ID, &context),
                };
                message.insert("contextId".into(), Value::String(rewritten));
            }
            if let Some(ids) = message.get_mut("referenceTaskIds").and_then(Value::as_array_mut) {
                for id in ids {
                    open(id)?;
                }
            }
            Ok(())
        }
    }
}

/// The ids of an answer on their way to the caller: the agent's own, sealed, and the caller's
/// own context, opened. Every `taskId`, `contextId` and `referenceTaskIds`, and the `id` of every
/// task, wherever they are: results, histories, the events of a stream. `GetTask` and
/// `CancelTask` answer with a task itself rather than one wrapped in `task`.
pub fn outbound(method: &str, value: &mut Value, s: &Sealer) {
    walk(value, s, matches!(method, "GetTask" | "CancelTask"));
}

fn walk(value: &mut Value, s: &Sealer, in_task: bool) {
    match value {
        Value::Object(members) => {
            for (key, v) in members.iter_mut() {
                match (key.as_str(), &*v) {
                    ("taskId", Value::String(id)) => *v = Value::String(s.seal(AGENT_ID, id)),
                    ("id", Value::String(id)) if in_task => *v = Value::String(s.seal(AGENT_ID, id)),
                    ("contextId", Value::String(id)) => {
                        *v = Value::String(match s.open(CALLER_ID, id) {
                            Some(own) => own,
                            None => s.seal(AGENT_ID, id),
                        });
                    }
                    ("referenceTaskIds", Value::Array(_)) => {
                        if let Value::Array(ids) = v {
                            for id in ids.iter_mut() {
                                if let Value::String(text) = id {
                                    *id = Value::String(s.seal(AGENT_ID, text));
                                }
                            }
                        }
                    }
                    ("task", Value::Object(_)) => walk(v, s, true),
                    _ => walk(v, s, false),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| walk(v, s, false)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sealer(ws: &str) -> Sealer {
        Sealer::new(&[Kek::new("k1", [3; 32])], "rag_1", ws).unwrap()
    }

    #[test]
    fn an_id_opens_for_its_workspace_only_and_keeps_one_form() {
        let a = sealer("ws_a");
        let sealed = a.seal(AGENT_ID, "task-9");
        assert_eq!(sealed, a.seal(AGENT_ID, "task-9"), "one task, one id");
        assert_eq!(a.open(AGENT_ID, &sealed).as_deref(), Some("task-9"));
        assert_eq!(sealer("ws_b").open(AGENT_ID, &sealed), None, "not in another workspace");
        assert_eq!(a.open(CALLER_ID, &sealed), None, "not as the other kind");
        let mut altered = sealed.clone();
        altered.pop();
        altered.push(if sealed.ends_with('A') { 'B' } else { 'A' });
        assert_eq!(a.open(AGENT_ID, &altered), None);
        assert_eq!(a.open(AGENT_ID, "task-9"), None, "a raw id is not taken");
        // After a rotation, ids sealed under the old key still open.
        let rotated = Sealer::new(&[Kek::new("k2", [4; 32]), Kek::new("k1", [3; 32])], "rag_1", "ws_a").unwrap();
        assert_eq!(rotated.open(AGENT_ID, &sealed).as_deref(), Some("task-9"));
    }

    #[test]
    fn ids_are_sealed_on_the_way_out_and_opened_on_the_way_in() {
        let s = sealer("ws_a");
        // The caller starts a conversation of its own.
        let mut params =
            json!({ "message": { "messageId": "m1", "role": "ROLE_USER", "contextId": "mine", "parts": [] } });
        inbound("SendMessage", &mut params, &s).unwrap();
        let at_agent = params["message"]["contextId"].as_str().unwrap().to_owned();
        assert!(at_agent.starts_with(CALLER_ID));
        // The agent answers with a task in that context.
        let mut answer = json!({ "task": { "id": "t-1", "contextId": at_agent, "status": { "state": "TASK_STATE_WORKING" },
                                           "history": [{ "messageId": "m1", "taskId": "t-1", "contextId": at_agent }] } });
        outbound("SendMessage", &mut answer, &s);
        let task_id = answer["task"]["id"].as_str().unwrap().to_owned();
        assert!(task_id.starts_with(AGENT_ID));
        assert_eq!(answer["task"]["contextId"], "mine", "the caller sees its own context");
        assert_eq!(answer["task"]["history"][0]["taskId"], task_id.as_str());
        // A later call names the task.
        let mut get = json!({ "id": task_id });
        inbound("GetTask", &mut get, &s).unwrap();
        assert_eq!(get["id"], "t-1");
        // Its answer is the task itself.
        let mut task = json!({ "id": "t-1", "contextId": "agent-ctx", "status": {} });
        outbound("GetTask", &mut task, &s);
        assert_eq!(task["id"], task_id.as_str());
        // A message without the optional fields keeps without them.
        let mut plain = json!({ "message": { "messageId": "m2", "role": "ROLE_USER", "parts": [] } });
        inbound("SendMessage", &mut plain, &s).unwrap();
        assert_eq!(plain, json!({ "message": { "messageId": "m2", "role": "ROLE_USER", "parts": [] } }));
        let mut forged = json!({ "id": "t-1" });
        assert_eq!(inbound("GetTask", &mut forged, &s), Err(Unknown));
        let mut theirs = json!({ "id": task_id });
        assert_eq!(inbound("GetTask", &mut theirs, &sealer("ws_b")), Err(Unknown));
        // Events of a stream are rewritten alike.
        let mut event = json!({ "statusUpdate": { "taskId": "t-1", "contextId": "agent-ctx", "status": {} } });
        outbound("SendStreamingMessage", &mut event, &s);
        assert_eq!(event["statusUpdate"]["taskId"], task_id.as_str());
        assert!(event["statusUpdate"]["contextId"].as_str().unwrap().starts_with(AGENT_ID));
    }
}
