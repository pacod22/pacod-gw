//! Anthropic's Files and Message Batches APIs, with the state they need.
//!
//! Files are kept in memory with their content. A batch computes its results when it is
//! created, as `/v1/messages` would answer each item, and reports `in_progress` until it has
//! been retrieved twice; then it has ended. A `custom_id` that starts with `error` gives an
//! errored item, one that starts with `expire` an expired item. A batch cancelled before it
//! ends cancels the items, and can be deleted only once it has ended.

use std::collections::BTreeMap;

use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::{Value, json};

use super::anthropic;
use super::behavior::Behavior;
use super::reply::fnv1a;
use super::server::{ApiError, Call, Reply};

/// How many retrievals a batch stays `in_progress`.
const POLLS_UNTIL_ENDED: u32 = 2;

pub(crate) struct Objects {
    /// Makes ids unique across mock processes: a gateway keeps the ids it has seen, and a
    /// restarted mock must not hand out one of them again.
    instance: String,
    next: u64,
    files: BTreeMap<String, (Value, Bytes)>,
    batches: BTreeMap<String, Batch>,
}

impl Default for Objects {
    fn default() -> Self {
        Self {
            instance: uuid::Uuid::new_v4().simple().to_string()[..8].to_owned(),
            next: 0,
            files: BTreeMap::new(),
            batches: BTreeMap::new(),
        }
    }
}

struct Batch {
    object: Value,
    /// One result per item, as the results file lists them.
    results: Vec<Value>,
    polls: u32,
}

fn timestamp(n: u64) -> String {
    format!("2026-01-01T{:02}:{:02}:{:02}Z", (n / 3600) % 24, (n / 60) % 60, n % 60)
}

/// A list in the shape both APIs use, newest first.
fn list<'a>(items: impl DoubleEndedIterator<Item = &'a Value>) -> Value {
    let data: Vec<Value> = items.rev().cloned().collect();
    let first = data.first().map(|v| v["id"].clone()).unwrap_or(Value::Null);
    let last = data.last().map(|v| v["id"].clone()).unwrap_or(Value::Null);
    json!({ "data": data, "has_more": false, "first_id": first, "last_id": last, "next_page": null })
}

fn not_found(what: &str, id: &str) -> ApiError {
    ApiError::new(404, format!("mock: the {what} '{id}' does not exist"))
}

/// One part of a multipart body: its name, its file name, its type and its bytes.
pub(crate) struct FormPart {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: String,
    pub body: Bytes,
}

/// The parts of a multipart body, read loosely: what the gateway sends is well formed.
pub(crate) fn form_parts(headers: &HeaderMap, body: &Bytes) -> Result<Vec<FormPart>, ApiError> {
    let content_type = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let boundary = content_type
        .split(';')
        .find_map(|p| p.trim().strip_prefix("boundary="))
        .map(|b| b.trim_matches('"').to_owned())
        .ok_or_else(|| ApiError::bad_request("a multipart/form-data body is required"))?;
    let delimiter = format!("--{boundary}");
    let text = body.as_ref();
    let mut parts = Vec::new();
    let mut at = 0;
    while let Some(start) = find(&text[at..], delimiter.as_bytes()).map(|i| i + at) {
        let part_start = start + delimiter.len() + 2;
        let Some(head_end) = find(&text[part_start.min(text.len())..], b"\r\n\r\n").map(|i| i + part_start) else {
            break;
        };
        let head = String::from_utf8_lossy(&text[part_start..head_end]).to_string();
        let data_start = head_end + 4;
        let Some(next) = find(&text[data_start..], format!("\r\n{delimiter}").as_bytes()).map(|i| i + data_start)
        else {
            break;
        };
        let quoted =
            |key: &str| head.split(&format!("{key}=\"")).nth(1).and_then(|r| r.split('"').next()).map(str::to_owned);
        parts.push(FormPart {
            name: quoted("; name").or_else(|| quoted(" name")).unwrap_or_default(),
            filename: quoted("filename"),
            content_type: head
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-type:").map(|v| v.trim().to_owned()))
                .unwrap_or_else(|| "application/octet-stream".into()),
            body: body.slice(data_start..next),
        });
        at = next + 2;
    }
    Ok(parts)
}

/// The first part of a multipart body named `file`: its file name, its type and its bytes.
fn file_part(headers: &HeaderMap, body: &Bytes) -> Result<(String, String, Bytes), ApiError> {
    let part = form_parts(headers, body)?
        .into_iter()
        .find(|p| p.name == "file")
        .ok_or_else(|| ApiError::bad_request("file: Field required"))?;
    Ok((part.filename.unwrap_or_else(|| "upload".into()), part.content_type, part.body))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

impl Objects {
    pub(crate) fn route(
        &mut self,
        method: &str,
        headers: &HeaderMap,
        call: &Call<'_>,
    ) -> Option<Result<Reply, ApiError>> {
        let path = call.path;
        let reply = match (method, path) {
            ("POST", "/v1/files") => self.upload(headers, call),
            ("GET", "/v1/files") => Ok(Reply::Json(list(self.files.values().map(|(o, _)| o)))),
            ("POST", "/v1/messages/batches") => self.create_batch(call),
            ("GET", "/v1/messages/batches") => Ok(Reply::Json(list(self.batches.values().map(|b| &b.object)))),
            (_, p) if p.starts_with("/v1/files/") => self.file(method, &p["/v1/files/".len()..]),
            (_, p) if p.starts_with("/v1/messages/batches/") => self.batch(method, &p["/v1/messages/batches/".len()..]),
            _ => return None,
        };
        Some(reply)
    }

    fn upload(&mut self, headers: &HeaderMap, call: &Call<'_>) -> Result<Reply, ApiError> {
        let (filename, mime_type, data) = file_part(headers, &Bytes::copy_from_slice(call.body))?;
        self.next += 1;
        let id = format!("file_mock{}{:08}", self.instance, self.next);
        let object = json!({
            "id": id, "type": "file", "filename": filename, "mime_type": mime_type,
            "size_bytes": data.len(), "created_at": timestamp(self.next), "downloadable": false, "expires_at": null
        });
        self.files.insert(id, (object.clone(), data));
        Ok(Reply::Json(object))
    }

    fn file(&mut self, method: &str, rest: &str) -> Result<Reply, ApiError> {
        let (id, content) = match rest.strip_suffix("/content") {
            Some(id) => (id, true),
            None => (rest, false),
        };
        let Some((object, data)) = self.files.get(id) else { return Err(not_found("file", id)) };
        match (method, content) {
            ("GET", false) => Ok(Reply::Json(object.clone())),
            ("GET", true) => Ok(Reply::Bytes { content_type: "application/octet-stream", body: data.clone() }),
            ("DELETE", false) => {
                self.files.remove(id);
                Ok(Reply::Json(json!({ "id": id, "type": "file_deleted" })))
            }
            _ => Err(ApiError::new(405, "mock: method not allowed")),
        }
    }

    fn create_batch(&mut self, call: &Call<'_>) -> Result<Reply, ApiError> {
        let request = call.json_object()?;
        let items = request["requests"]
            .as_array()
            .filter(|r| !r.is_empty())
            .ok_or_else(|| ApiError::bad_request("requests: at least one request is required"))?;
        let mut results = Vec::new();
        for item in items {
            let custom_id = item["custom_id"].as_str().unwrap_or_default().to_owned();
            if item["params"].get("stream").and_then(Value::as_bool) == Some(true) {
                return Err(ApiError::bad_request("requests: stream is not supported in a batch"));
            }
            let result = if custom_id.starts_with("error") {
                json!({ "type": "errored", "error": { "type": "error", "error": { "type": "invalid_request_error", "message": "mock: errored item" } } })
            } else if custom_id.starts_with("expire") {
                json!({ "type": "expired" })
            } else {
                let params = Bytes::from(item["params"].to_string());
                let behavior = Behavior::default();
                let inner = Call {
                    path: "/v1/messages",
                    query: "",
                    body: &params,
                    behavior: &behavior,
                    seed: fnv1a(&[&params]),
                };
                match anthropic::messages(&inner)? {
                    Reply::Json(message) => json!({ "type": "succeeded", "message": message }),
                    _ => return Err(ApiError::bad_request("mock: an item gave no message")),
                }
            };
            results.push(json!({ "custom_id": custom_id, "result": result }));
        }
        self.next += 1;
        let id = format!("msgbatch_mock{}{:08}", self.instance, self.next);
        let object = json!({
            "id": id, "type": "message_batch", "processing_status": "in_progress",
            "request_counts": { "processing": results.len(), "succeeded": 0, "errored": 0, "canceled": 0, "expired": 0 },
            "created_at": timestamp(self.next), "expires_at": timestamp(self.next + 86_400), "ended_at": null,
            "cancel_initiated_at": null, "archived_at": null, "results_url": null
        });
        self.batches.insert(id, Batch { object: object.clone(), results, polls: 0 });
        Ok(Reply::Json(object))
    }

    fn batch(&mut self, method: &str, rest: &str) -> Result<Reply, ApiError> {
        let (id, action) = rest.split_once('/').map_or((rest, ""), |(id, a)| (id, a));
        let Some(batch) = self.batches.get_mut(id) else { return Err(not_found("batch", id)) };
        match (method, action) {
            ("GET", "") => {
                batch.polls += 1;
                if batch.polls >= POLLS_UNTIL_ENDED || batch.object["processing_status"] == "canceling" {
                    batch.end(id);
                }
                Ok(Reply::Json(batch.object.clone()))
            }
            ("POST", "cancel") => {
                if batch.object["processing_status"] == "in_progress" {
                    batch.object["processing_status"] = json!("canceling");
                    batch.object["cancel_initiated_at"] = json!(timestamp(self.next + 60));
                    for r in &mut batch.results {
                        r["result"] = json!({ "type": "canceled" });
                    }
                }
                Ok(Reply::Json(batch.object.clone()))
            }
            ("GET", "results") => {
                if batch.object["processing_status"] != "ended" {
                    return Err(ApiError::bad_request("mock: the batch has not ended"));
                }
                let mut lines = String::new();
                for r in &batch.results {
                    lines.push_str(&r.to_string());
                    lines.push('\n');
                }
                Ok(Reply::Bytes { content_type: "application/binary", body: Bytes::from(lines) })
            }
            ("DELETE", "") => {
                if batch.object["processing_status"] != "ended" {
                    return Err(ApiError::bad_request(
                        "mock: a batch that has not ended cannot be deleted; cancel it first",
                    ));
                }
                self.batches.remove(id);
                Ok(Reply::Json(json!({ "id": id, "type": "message_batch_deleted" })))
            }
            _ => Err(ApiError::new(405, "mock: method not allowed")),
        }
    }
}

impl Batch {
    fn end(&mut self, id: &str) {
        if self.object["processing_status"] == "ended" {
            return;
        }
        let mut counts = json!({ "processing": 0, "succeeded": 0, "errored": 0, "canceled": 0, "expired": 0 });
        for r in &self.results {
            let kind = r["result"]["type"].as_str().unwrap_or("errored");
            counts[kind] = json!(counts[kind].as_u64().unwrap_or(0) + 1);
        }
        self.object["processing_status"] = json!("ended");
        self.object["request_counts"] = counts;
        self.object["ended_at"] = self.object["created_at"].clone();
        self.object["results_url"] = json!(format!("https://api.anthropic.com/v1/messages/batches/{id}/results"));
    }
}
