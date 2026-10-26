//! Stored responses of OpenAI's Responses API (`POST /v1/responses`), in a workspace whose
//! setting `responses.store` is on.
//!
//! A call that continues a stored response (`previous_response_id`) is expanded into the call
//! its caller could have sent without one: the stored turns' input and output items in order,
//! then its own input. Everything after that (routing, limits, the response cache, shaping,
//! translation, failover) serves it as a stateless call, and the provider keeps nothing.
//!
//! A call that names a conversation (`conversation`) is given the conversation's items first,
//! in the same way, and its input and answer are added to the conversation once it has
//! answered, whether or not its response is kept, as at OpenAI.
//!
//! The gateway mints the answer's id, `resp_<the call's request id>`, and keeps the turn before
//! the caller can name it: before a whole answer is returned, and before a stream's last event
//! is sent (the recorder's gate). A next turn sent right after the answer therefore finds it,
//! on whichever instance it lands.

use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Utc};
use serde_json::Value;

use super::Gw;
use super::auth::Caller;
use super::errors::GwError;
use crate::json::{Kind, Op, Patch, apply, scan};
use crate::model::settings::ResolvedSettings;
use crate::relay::{CallRecorder, Transform};
use crate::sse::{SseFramer, write_event};
use crate::store::conversations::{ConversationItem, Items};
use crate::store::objects::ObjectStore;
use crate::store::responses::{Chain, StoredResponse};
use crate::telemetry::metrics;

/// How long the write of a turn may take before the answer goes out without it.
const KEEP_TIMEOUT: Duration = Duration::from_secs(10);

/// The statuses of a response that is kept. A failed one has nothing to continue.
const KEPT_STATUSES: [&str; 2] = ["completed", "incomplete"];

/// A call that keeps its response, continues one, or both: what it carries from its request to
/// its answer.
pub(super) struct Turn {
    /// `resp_<the call's request id>`.
    pub(super) id: String,
    /// The response this call continues.
    pub(super) previous: Option<String>,
    /// The conversation this call is a turn of.
    pub(super) conversation: Option<String>,
    /// Whether the response is kept: `store` true or left out.
    pub(super) store: bool,
    /// The model as the caller named it, set once the call is planned.
    pub(super) model: String,
    /// This call's own input, as items.
    input: Vec<Value>,
    expires_at: DateTime<Utc>,
    objects: Arc<dyn ObjectStore>,
    max_bytes: usize,
}

/// Why a workspace that keeps no responses refuses a member that needs them.
pub(super) fn not_kept(member: &str, settings: &ResolvedSettings) -> GwError {
    let why = if settings.keeps_nothing() {
        "the audit rule forbid keeps nothing of this workspace's calls"
    } else {
        "this workspace keeps no responses: its setting responses.store is off"
    };
    GwError::invalid_request(format!("{member}: {why}; send the conversation in input instead"))
}

/// Reads a request for what it asks of the stored responses, and expands one that continues a
/// stored response. Returns the body the call is served with and, where a response is kept or
/// continued, the turn, whose id is made of the call's (`call_id`). A gateway that keeps no
/// responses (the minimal one) returns the body as it is: planning refuses the members that
/// need them.
pub(super) async fn prepare(
    gw: &Gw,
    caller: &Caller,
    bytes: Bytes,
    call_id: uuid::Uuid,
) -> Result<(Bytes, Option<Turn>), GwError> {
    let Some(objects) = gw.objects.clone().filter(|o| o.keeps_responses()) else { return Ok((bytes, None)) };
    // A body that is not JSON is refused by planning, with its reason.
    let Ok(top) = scan(&bytes) else { return Ok((bytes, None)) };
    let store = match top.raw(&bytes, "store") {
        None | Some(b"null") => None,
        Some(b"true") => Some(true),
        Some(b"false") => Some(false),
        Some(_) => return Err(GwError::invalid_request("store must be true or false")),
    };
    let previous = match top.get("previous_response_id") {
        None => None,
        Some(e) if e.kind == Kind::Null => None,
        Some(e) if e.kind == Kind::String => top.str(&bytes, "previous_response_id").filter(|p| !p.is_empty()),
        Some(_) => return Err(GwError::invalid_request("previous_response_id must be the id of a response")),
    };
    // The conversation's id, or an object that holds it.
    let conversation = match top.raw(&bytes, "conversation").map(serde_json::from_slice::<Value>) {
        None | Some(Ok(Value::Null)) => None,
        Some(Ok(Value::String(id))) => Some(id),
        Some(Ok(Value::Object(o))) if o.get("id").is_some_and(Value::is_string) => {
            o.get("id").and_then(Value::as_str).map(str::to_owned)
        }
        Some(_) => return Err(GwError::invalid_request("conversation must be a conversation's id, or {\"id\": …}")),
    };
    let settings = &caller.workspace.settings;
    if !settings.keeps_responses() {
        for (member, asked) in [
            ("store", store == Some(true)),
            ("previous_response_id", previous.is_some()),
            ("conversation", conversation.is_some()),
        ] {
            if asked {
                return Err(not_kept(member, settings));
            }
        }
        return Ok((bytes, None));
    }
    if previous.is_some() && conversation.is_some() {
        return Err(GwError::invalid_request(
            "previous_response_id and conversation cannot both be given: a conversation holds its turns itself",
        ));
    }
    // As at OpenAI, a response is kept unless the call says otherwise.
    let store = store.unwrap_or(true);
    if !store && previous.is_none() && conversation.is_none() {
        return Ok((bytes, None));
    }
    let Some(input) = top.raw(&bytes, "input").and_then(|raw| serde_json::from_slice::<Value>(raw).ok()) else {
        // Planning refuses a call without input.
        return Ok((bytes, None));
    };
    let Some(own) = pacod_gw_dialects::responses::input_items(&input) else { return Ok((bytes, None)) };

    let max_bytes = gw.boot.max_body_bytes;
    let mut patch = Patch::new();
    // Planning refuses `store: true`: here it is served, and the provider keeps nothing anyway.
    patch.remove("store");
    if let Some(prev) = &previous {
        patch.remove("previous_response_id");
        let turns = match objects.response_chain(&caller.key.workspace_id, prev, max_bytes as i64).await {
            Ok(Chain::Found(turns)) => turns,
            Ok(Chain::Missing(id)) => return Err(previous_not_found(prev, &id)),
            Ok(Chain::TooLarge) => return Err(too_long(max_bytes)),
            Err(e) => {
                tracing::warn!(error = %e, "reading a stored response failed");
                return Err(GwError::unavailable(
                    "store_unavailable",
                    "the stored responses cannot be read now; try again",
                ));
            }
        };
        // A turn of a conversation is continued with the conversation: from the turn alone, the
        // conversation's other items would be missing.
        if let Some(conversation) = turns.iter().find_map(|t| t.conversation_id.as_deref()) {
            return Err(GwError::invalid_request(format!(
                "previous_response_id: '{prev}' belongs to the conversation '{conversation}'; continue it with conversation"
            )));
        }
        let mut items: Vec<Value> = Vec::new();
        for turn in &turns {
            items.extend(stored_items(&turn.input, None));
            items.extend(stored_items(&turn.response, Some("output")));
        }
        items.extend(own.iter().cloned());
        patch.push("input", Op::Set(Bytes::from(serde_json::to_vec(&Value::Array(items)).map_err(GwError::internal)?)));
    }
    if let Some(conv) = &conversation {
        patch.remove("conversation");
        let kept = match objects.conversation_replay(&caller.key.workspace_id, conv, max_bytes as i64).await {
            Ok(Items::Found(items)) => items,
            Ok(Items::Missing) => {
                return Err(GwError::not_found(
                    "not_found",
                    format!("conversation: no conversation with id '{conv}' is kept in this workspace"),
                ));
            }
            Ok(Items::TooLarge) => return Err(too_long(max_bytes)),
            Err(e) => {
                tracing::warn!(error = %e, "reading a conversation failed");
                return Err(GwError::unavailable(
                    "store_unavailable",
                    "the conversation cannot be read now; try again",
                ));
            }
        };
        let mut items: Vec<Value> = kept.iter().filter_map(|i| serde_json::from_slice(i).ok()).collect();
        items.extend(own.iter().cloned());
        patch.push("input", Op::Set(Bytes::from(serde_json::to_vec(&Value::Array(items)).map_err(GwError::internal)?)));
    }
    let expanded = apply(&bytes, &top, &patch)
        .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;
    if expanded.len() > max_bytes {
        return Err(too_long(max_bytes));
    }
    let id = format!("resp_{}", call_id.simple());
    let expires_at = Utc::now() + chrono::Duration::days(settings.responses_retention_days.into());
    let turn =
        Turn { id, previous, conversation, store, model: String::new(), input: own, expires_at, objects, max_bytes };
    Ok((expanded.to_bytes(), Some(turn)))
}

/// The items kept in a stored body: an input array as it is, or the `output` of a response.
fn stored_items(body: &[u8], member: Option<&str>) -> Vec<Value> {
    let Ok(v) = serde_json::from_slice::<Value>(body) else { return Vec::new() };
    let items = match member {
        Some(m) => v.get(m).cloned(),
        None => Some(v),
    };
    match items {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

fn previous_not_found(named: &str, missing: &str) -> GwError {
    let message = if named == missing {
        format!("previous_response_id: no response with id '{named}' is kept in this workspace")
    } else {
        format!("previous_response_id: the response '{named}' continues '{missing}', which is no longer kept")
    };
    GwError::previous_response_not_found(message)
}

fn too_long(max_bytes: usize) -> GwError {
    GwError::invalid_request(format!("the conversation with its earlier turns exceeds the limit of {max_bytes} bytes"))
}

impl Turn {
    /// Puts the gateway's id, `store`, `previous_response_id` and `conversation` on a response
    /// object.
    fn stamp(&self, response: &mut Value, kept: bool) {
        pacod_gw_dialects::responses::stamp(
            response,
            &self.id,
            kept,
            self.previous.as_deref(),
            self.conversation.as_deref(),
        );
    }

    /// What this turn writes with the response the caller is given: the response where it is
    /// kept, and the turn's items where it belongs to a conversation. `None` when nothing is
    /// written: a response not finished, one too large, or one neither kept nor in a
    /// conversation.
    fn write(&self, response: &Value, owner: &Owner) -> Option<Write> {
        let status = response.get("status").and_then(Value::as_str).unwrap_or_default();
        if !KEPT_STATUSES.contains(&status) || (!self.store && self.conversation.is_none()) {
            return None;
        }
        let input = serde_json::to_vec(&self.input).ok()?;
        let body = serde_json::to_vec(response).ok()?;
        if input.len() + body.len() > self.max_bytes {
            metrics::record_stored_response("too_large");
            return None;
        }
        let items = self.conversation.as_ref().map(|c| {
            let output = response.get("output").and_then(Value::as_array).into_iter().flatten();
            let items = self.input.iter().chain(output);
            let items = items.map(|i| conversation_item(i, &owner.end_user, &owner.user_id, Some(&self.id))).collect();
            (c.clone(), items)
        });
        let response = self.store.then(|| StoredResponse {
            id: self.id.clone(),
            organisation_id: owner.organisation_id.clone(),
            workspace_id: owner.workspace_id.clone(),
            key_id: owner.key_id.clone(),
            end_user: owner.end_user.clone(),
            user_id: owner.user_id.clone(),
            previous_id: self.previous.clone(),
            conversation_id: self.conversation.clone(),
            model: self.model.clone(),
            status: status.to_owned(),
            input,
            response: body,
            created_at: Utc::now(),
            expires_at: self.expires_at,
        });
        Some(Write {
            id: self.id.clone(),
            workspace_id: owner.workspace_id.clone(),
            response,
            conversation: items,
            expires_at: self.expires_at,
        })
    }

    /// Stamps a whole answer and keeps the turn before the caller has it. A stream the response
    /// cache answers is whole too (`sse`): its events are stamped one by one.
    pub(super) async fn keep_whole(&self, body: Bytes, sse: bool, owner: Owner) -> Bytes {
        if !sse {
            let Ok(mut response) = serde_json::from_slice::<Value>(&body) else { return body };
            let kept = match self.write(&self.stamped(&response), &owner) {
                Some(w) => keep(self.objects.clone(), w).await,
                None => false,
            };
            self.stamp(&mut response, kept);
            return Bytes::from(serde_json::to_vec(&response).unwrap_or_default());
        }
        let mut out = BytesMut::with_capacity(body.len());
        let mut framer = SseFramer::new();
        let mut terminal = None;
        let mut after = BytesMut::new();
        framer.feed_raw(&body, |raw, ev| self.pass(raw, ev, &mut out, &mut terminal, &mut after));
        let rest = framer.take_pending();
        if let Some(held) = terminal {
            let kept = match self.write(&self.stamped(&held.data["response"]), &owner) {
                Some(w) => keep(self.objects.clone(), w).await,
                None => false,
            };
            self.release(held, kept, &mut out);
            out.extend_from_slice(&after);
        }
        out.extend_from_slice(&rest);
        out.freeze()
    }

    /// Writes the held terminal event, its response stamped as kept or not.
    fn release(&self, mut held: Held, kept: bool, out: &mut BytesMut) {
        if let Some(response) = held.data.get_mut("response") {
            self.stamp(response, kept);
        }
        write_event(out, &held.event, &serde_json::to_vec(&held.data).unwrap_or_default());
    }

    /// A copy of the response as it will be given when it is kept, for its record.
    fn stamped(&self, response: &Value) -> Value {
        let mut r = response.clone();
        self.stamp(&mut r, true);
        r
    }

    /// One event of a stream on its way to the caller: the response object of the events that
    /// carry one is stamped; the terminal event is held in `terminal`, and what follows it in
    /// `after`.
    fn pass(
        &self,
        raw: Bytes,
        ev: Option<crate::sse::SseEvent>,
        out: &mut BytesMut,
        terminal: &mut Option<Held>,
        after: &mut BytesMut,
    ) {
        if terminal.is_some() {
            after.extend_from_slice(&raw);
            return;
        }
        let Some(ev) = ev.filter(|e| e.event.starts_with("response.")) else {
            out.extend_from_slice(&raw);
            return;
        };
        let Some(mut data) =
            serde_json::from_slice::<Value>(&ev.data).ok().filter(|d| d.get("response").is_some_and(Value::is_object))
        else {
            out.extend_from_slice(&raw);
            return;
        };
        if matches!(ev.event.as_str(), "response.completed" | "response.incomplete") {
            *terminal = Some(Held { event: ev.event, data });
            return;
        }
        if let Some(response) = data.get_mut("response") {
            self.stamp(response, self.store);
        }
        write_event(out, &ev.event, &serde_json::to_vec(&data).unwrap_or_default());
    }
}

/// Whose a kept turn is, as the call's event names it.
pub(super) struct Owner {
    organisation_id: String,
    workspace_id: String,
    key_id: String,
    end_user: String,
    user_id: String,
}

impl Owner {
    pub(super) fn of(rec: &CallRecorder) -> Self {
        let e = &rec.event;
        Self {
            organisation_id: e.organisation_id.clone(),
            workspace_id: e.workspace_id.clone(),
            key_id: e.key_id.clone(),
            end_user: e.end_user.clone(),
            user_id: e.user_id.clone(),
        }
    }
}

/// The last event of a stream, held until its turn is written.
struct Held {
    event: String,
    data: Value,
}

/// What a finished turn writes (see [`Turn::write`]).
struct Write {
    id: String,
    workspace_id: String,
    response: Option<StoredResponse>,
    /// The conversation, and the turn's input and answer as its items.
    conversation: Option<(String, Vec<ConversationItem>)>,
    expires_at: DateTime<Utc>,
}

/// An item of a conversation, with its own id or one made for it now. The id is kept beside the
/// item, not written into it: a provider is given the item as it was sent or answered.
pub(super) fn conversation_item(
    item: &Value,
    end_user: &str,
    user_id: &str,
    response_id: Option<&str>,
) -> ConversationItem {
    let item_id = item.get("id").and_then(Value::as_str).filter(|i| !i.is_empty()).map_or_else(
        || format!("{}_{}", pacod_gw_dialects::responses::item_prefix(item), uuid::Uuid::now_v7().simple()),
        str::to_owned,
    );
    ConversationItem {
        item_id,
        end_user: end_user.to_owned(),
        user_id: user_id.to_owned(),
        response_id: response_id.map(str::to_owned),
        item: serde_json::to_vec(item).unwrap_or_default(),
    }
}

/// Writes a turn, within [`KEEP_TIMEOUT`]: its response and its conversation's new items in one
/// transaction. Returns whether the response is kept.
async fn keep(objects: Arc<dyn ObjectStore>, w: Write) -> bool {
    let written = match &w.conversation {
        Some((conversation, items)) => {
            objects.conversation_turn(w.response.as_ref(), &w.workspace_id, conversation, items, w.expires_at)
        }
        None => match &w.response {
            Some(r) => objects.response_keep(r),
            None => return false,
        },
    };
    match tokio::time::timeout(KEEP_TIMEOUT, written).await {
        Ok(Ok(true)) => {
            metrics::record_stored_response("kept");
            w.response.is_some()
        }
        Ok(Ok(false)) => {
            if let Some((conversation, _)) = &w.conversation {
                tracing::warn!(response = %w.id, %conversation, "the conversation was deleted while it was answered; nothing was kept");
            }
            false
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, response = %w.id, "keeping a response failed; it was answered with store: false");
            metrics::record_stored_response("failed");
            false
        }
        Err(_) => {
            tracing::warn!(response = %w.id, "keeping a response took too long; it was answered with store: false");
            metrics::record_stored_response("failed");
            false
        }
    }
}

/// A stream of the Responses API on its way to a caller whose response is kept: the events that
/// carry the response object are stamped, and the last one waits behind the recorder's gate
/// until the turn is written.
pub(super) struct Stored {
    inner: Box<dyn Transform>,
    turn: Arc<Turn>,
    framer: SseFramer,
}

impl Stored {
    pub(super) fn new(inner: Box<dyn Transform>, turn: Arc<Turn>) -> Self {
        Self { inner, turn, framer: SseFramer::new() }
    }

    fn pass(&mut self, out: Bytes, rec: &mut CallRecorder) -> Bytes {
        let mut now = BytesMut::with_capacity(out.len());
        let mut terminal = None;
        let mut after = BytesMut::new();
        let turn = self.turn.clone();
        self.framer.feed_raw(&out, |raw, ev| turn.pass(raw, ev, &mut now, &mut terminal, &mut after));
        if let Some(held) = terminal {
            let write = turn.write(&turn.stamped(&held.data["response"]), &Owner::of(rec));
            // Spawned, so that a caller that goes away does not stop the write.
            let task = write.map(|w| tokio::spawn(keep(turn.objects.clone(), w)));
            rec.gate = Some(Box::pin(async move {
                let kept = match task {
                    Some(t) => t.await.unwrap_or(false),
                    None => false,
                };
                let mut out = BytesMut::new();
                turn.release(held, kept, &mut out);
                out.extend_from_slice(&after);
                out.freeze()
            }));
        }
        now.freeze()
    }
}

impl Transform for Stored {
    fn chunk(&mut self, chunk: &Bytes, rec: &mut CallRecorder) -> Bytes {
        let out = self.inner.chunk(chunk, rec);
        self.pass(out, rec)
    }

    fn end(&mut self, rec: &mut CallRecorder) -> Bytes {
        let out = self.inner.end(rec);
        let mut now = BytesMut::from(&self.pass(out, rec)[..]);
        now.extend_from_slice(&self.framer.take_pending());
        now.freeze()
    }

    fn error(&mut self, kind: &str, message: &str, rec: &mut CallRecorder) -> Bytes {
        // A broken stream keeps nothing: the error event goes out as it is.
        let mut now = BytesMut::from(&self.framer.take_pending()[..]);
        now.extend_from_slice(&self.inner.error(kind, message, rec));
        now.freeze()
    }
}
