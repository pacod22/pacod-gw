//! OpenAI's Conversations API (`/v1/conversations`), on the conversations a workspace keeps
//! (`store::conversations`): created with items, read, given metadata and deleted, and their
//! items listed, added, read and deleted. A call to `/v1/responses` that names a conversation
//! is given its items first and adds its own input and answer (`state.rs`).
//!
//! A workspace keeps conversations where it keeps responses (`responses.store`). Reading and
//! deleting stay open after the setting is switched off, so that what was kept can be cleared.

use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{Map, Value, json};

use super::Gw;
use super::auth::Caller;
use super::client_ip::ClientIp;
use super::common::GatewayParams;
use super::errors::GwError;
use super::objects::{ObjectCall, begin_unrecorded, fail, relay_whole, start_audit};
use super::state::conversation_item;
use crate::store::StoreError;
use crate::store::conversations::{Conversation, ConversationItem};
use crate::store::objects::ObjectStore;

/// The most items one call adds, and the most metadata pairs, as at OpenAI.
const MAX_ITEMS: usize = 20;
const MAX_METADATA: usize = 16;
/// The largest body of a call that creates a conversation or adds items.
const MAX_BODY: usize = 8 << 20;

/// `POST /v1/conversations`.
pub async fn create(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    serve(gw, client, req, Vec::new()).await
}

/// The paths under `/v1/conversations/`.
pub async fn path(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(rest): Path<String>,
    req: Request,
) -> Response {
    let segments = rest.split('/').map(str::to_owned).collect();
    serve(gw, client, req, segments).await
}

/// What a call asks of a conversation.
enum Action {
    Create,
    Retrieve(String),
    Update(String),
    Delete(String),
    ListItems(String),
    AddItems(String),
    Item(String, String),
    DeleteItem(String, String),
}

impl Action {
    fn of(method: &Method, segments: &[String]) -> Option<(&'static str, Self)> {
        let s: Vec<&str> = segments.iter().map(String::as_str).collect();
        Some(match (method.clone(), s.as_slice()) {
            (Method::POST, []) => ("/v1/conversations", Action::Create),
            (Method::GET, [id]) => ("/v1/conversations/{id}", Action::Retrieve(id.to_string())),
            (Method::POST, [id]) => ("/v1/conversations/{id}", Action::Update(id.to_string())),
            (Method::DELETE, [id]) => ("/v1/conversations/{id}", Action::Delete(id.to_string())),
            (Method::GET, [id, "items"]) => ("/v1/conversations/{id}/items", Action::ListItems(id.to_string())),
            (Method::POST, [id, "items"]) => ("/v1/conversations/{id}/items", Action::AddItems(id.to_string())),
            (Method::GET, [id, "items", item]) => {
                ("/v1/conversations/{id}/items/{item_id}", Action::Item(id.to_string(), item.to_string()))
            }
            (Method::DELETE, [id, "items", item]) => {
                ("/v1/conversations/{id}/items/{item_id}", Action::DeleteItem(id.to_string(), item.to_string()))
            }
            _ => return None,
        })
    }

    /// Whether the call brings content to keep.
    fn writes(&self) -> bool {
        matches!(self, Action::Create | Action::Update(_) | Action::AddItems(_))
    }
}

async fn serve(gw: Arc<Gw>, client: std::net::IpAddr, req: Request, segments: Vec<String>) -> Response {
    let Some((path, action)) = Action::of(req.method(), &segments) else {
        return GwError::not_found("not_found", "no such path of the Conversations API")
            .response(super::errors::Envelope::OpenAi, "");
    };
    let (parts, body) = req.into_parts();
    let ObjectCall { caller, rec } = match begin_unrecorded(&gw, client, &parts, path, "conversations") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    // What a call brings is recorded where audit mode applies, as a call to /v1/responses is.
    let mut rec = if action.writes() {
        match start_audit(&gw, &caller, &parts, path, rec) {
            Ok(rec) => rec,
            Err(r) => return *r,
        }
    } else {
        rec
    };
    let bytes = if action.writes() {
        match http_body_util::Limited::new(body, MAX_BODY).collect().await {
            Ok(b) => b.to_bytes(),
            Err(_) => return fail(GwError::too_large(MAX_BODY), rec),
        }
    } else {
        drop(body);
        Bytes::new()
    };
    if let Some(a) = rec.audit.as_mut() {
        a.request_body(&bytes, &parts.headers);
    }
    let Some(store) = gw.objects.clone() else { return fail(GwError::internal("no record of conversations"), rec) };
    let query = parts.uri.query().unwrap_or_default().to_owned();
    let params = GatewayParams::from_headers(&parts.headers).unwrap_or_default();
    let result = act(&*store, &caller, &params, action, &bytes, &query).await;
    match result {
        Ok(body) => relay_whole(StatusCode::OK, "application/json", None, body, rec),
        Err(e) => fail(e, rec),
    }
}

fn render(v: &Value) -> Bytes {
    Bytes::from(serde_json::to_vec(v).expect("a JSON value always serialises"))
}

fn unavailable(e: StoreError) -> GwError {
    tracing::warn!(error = %e, "the record of conversations failed");
    GwError::unavailable("store_unavailable", "the conversations cannot be reached now; try again")
}

fn missing(id: &str) -> GwError {
    GwError::not_found("not_found", format!("no conversation with id '{id}' is kept in this workspace"))
}

/// A conversation as OpenAI writes one.
fn object_of(c: &Conversation) -> Value {
    json!({ "id": c.id, "object": "conversation", "created_at": c.created_at.timestamp(), "metadata": c.metadata })
}

/// An item as the caller reads it: with its id.
fn shown(i: &ConversationItem) -> Value {
    let item = serde_json::from_slice::<Value>(&i.item).unwrap_or(Value::Null);
    pacod_gw_dialects::responses::listed_item(item, &i.item_id)
}

fn list(items: Vec<Value>, has_more: bool) -> Value {
    json!({
        "object": "list",
        "first_id": items.first().map(|i| i["id"].clone()),
        "last_id": items.last().map(|i| i["id"].clone()),
        "has_more": has_more,
        "data": items,
    })
}

fn query_value(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes()).find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

/// A request body's member, which must be an object when present.
fn body_of(bytes: &[u8]) -> Result<Map<String, Value>, GwError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(o)) => Ok(o),
        _ => Err(GwError::invalid_request("the request body must be a JSON object")),
    }
}

/// Metadata as OpenAI takes it: up to 16 pairs of strings, keys up to 64 characters and values
/// up to 512.
fn metadata_of(v: Option<&Value>) -> Result<Value, GwError> {
    match v {
        None | Some(Value::Null) => Ok(json!({})),
        Some(Value::Object(m))
            if m.len() <= MAX_METADATA
                && m.iter()
                    .all(|(k, v)| k.chars().count() <= 64 && v.as_str().is_some_and(|s| s.chars().count() <= 512)) =>
        {
            Ok(Value::Object(m.clone()))
        }
        Some(_) => Err(GwError::invalid_request(
            "metadata holds up to 16 pairs of strings: keys up to 64 characters, values up to 512",
        )),
    }
}

/// The items a call adds, each with its id (its own, or one made now), for `caller`.
fn items_of(v: Option<&Value>, params: &GatewayParams, required: bool) -> Result<Vec<ConversationItem>, GwError> {
    let items = match v {
        None | Some(Value::Null) if !required => return Ok(Vec::new()),
        Some(Value::Array(items)) if !items.is_empty() || !required => items,
        _ => return Err(GwError::invalid_request("items must be a list of input items")),
    };
    if items.len() > MAX_ITEMS {
        return Err(GwError::invalid_request(format!("items: at most {MAX_ITEMS} items at a time")));
    }
    items
        .iter()
        .map(|item| {
            if !item.is_object() {
                return Err(GwError::invalid_request("items must be a list of input items"));
            }
            Ok(conversation_item(
                item,
                params.end_user.as_deref().unwrap_or_default(),
                params.user_id.as_deref().unwrap_or_default(),
                None,
            ))
        })
        .collect()
}

async fn act(
    store: &dyn ObjectStore,
    caller: &Caller,
    params: &GatewayParams,
    action: Action,
    bytes: &[u8],
    query: &str,
) -> Result<Bytes, GwError> {
    let ws = &caller.key.workspace_id;
    let settings = &caller.workspace.settings;
    if action.writes() && !settings.keeps_responses() {
        return Err(super::state::not_kept("conversation", settings));
    }
    let expires_at = Utc::now() + chrono::Duration::days(settings.responses_retention_days.into());
    match action {
        Action::Create => {
            let body = body_of(bytes)?;
            let metadata = metadata_of(body.get("metadata"))?;
            let items = items_of(body.get("items"), params, false)?;
            let now = Utc::now();
            let c = Conversation {
                id: format!("conv_{}", uuid::Uuid::now_v7().simple()),
                organisation_id: caller.key.organisation_id.clone(),
                workspace_id: ws.clone(),
                key_id: caller.key.id.clone(),
                end_user: params.end_user.clone().unwrap_or_default(),
                user_id: params.user_id.clone().unwrap_or_default(),
                metadata,
                created_at: now,
                used_at: now,
                expires_at,
            };
            if !store.conversation_create(&c, &items).await.map_err(unavailable)? {
                return Err(GwError::internal("this record keeps no conversations"));
            }
            Ok(render(&object_of(&c)))
        }
        Action::Retrieve(id) => {
            let c = store.conversation(ws, &id).await.map_err(unavailable)?.ok_or_else(|| missing(&id))?;
            Ok(render(&object_of(&c)))
        }
        Action::Update(id) => {
            let body = body_of(bytes)?;
            if !body.contains_key("metadata") {
                return Err(GwError::invalid_request("metadata is required"));
            }
            let metadata = metadata_of(body.get("metadata"))?;
            let c = store
                .conversation_update(ws, &id, &metadata)
                .await
                .map_err(unavailable)?
                .ok_or_else(|| missing(&id))?;
            Ok(render(&object_of(&c)))
        }
        Action::Delete(id) => {
            if !store.conversation_delete(ws, &id).await.map_err(unavailable)? {
                return Err(missing(&id));
            }
            Ok(render(&json!({ "id": id, "object": "conversation.deleted", "deleted": true })))
        }
        Action::ListItems(id) => {
            let limit = match query_value(query, "limit") {
                None => 20,
                Some(l) => l
                    .parse::<i64>()
                    .ok()
                    .filter(|l| (1..=100).contains(l))
                    .ok_or_else(|| GwError::invalid_request("limit must be a number from 1 to 100"))?,
            };
            let ascending = match query_value(query, "order").as_deref() {
                None | Some("desc") => false,
                Some("asc") => true,
                Some(_) => return Err(GwError::invalid_request("order must be asc or desc")),
            };
            let after = query_value(query, "after");
            let page = store
                .conversation_items(ws, &id, after.as_deref(), limit, ascending)
                .await
                .map_err(unavailable)?
                .ok_or_else(|| missing(&id))?;
            Ok(render(&list(page.items.iter().map(shown).collect(), page.has_more)))
        }
        Action::AddItems(id) => {
            let body = body_of(bytes)?;
            let items = items_of(body.get("items"), params, true)?;
            if !store.conversation_add(ws, &id, &items, expires_at).await.map_err(unavailable)? {
                return Err(missing(&id));
            }
            Ok(render(&list(items.iter().map(shown).collect(), false)))
        }
        Action::Item(id, item_id) => {
            let item = store.conversation_item(ws, &id, &item_id).await.map_err(unavailable)?.ok_or_else(|| {
                GwError::not_found("not_found", format!("no item with id '{item_id}' in the conversation '{id}'"))
            })?;
            Ok(render(&shown(&item)))
        }
        Action::DeleteItem(id, item_id) => {
            let c = store.conversation(ws, &id).await.map_err(unavailable)?.ok_or_else(|| missing(&id))?;
            if !store.conversation_item_delete(ws, &id, &item_id).await.map_err(unavailable)? {
                return Err(GwError::not_found(
                    "not_found",
                    format!("no item with id '{item_id}' in the conversation '{id}'"),
                ));
            }
            Ok(render(&object_of(&c)))
        }
    }
}

/// The paths of the Conversations API, for a gateway that keeps no conversations.
pub async fn not_here() -> Response {
    GwError::not_implemented(
        "not_available",
        "this gateway keeps no conversations: send the conversation in input instead",
    )
    .response(super::errors::Envelope::OpenAi, "")
}
