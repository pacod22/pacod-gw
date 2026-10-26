//! OpenAI's Files and Batches APIs, with the state they need.
//!
//! Files are kept in memory with their content, and every file's content can be read back. A
//! batch is `validating` when it is made and has run when it is first retrieved: each request
//! of its input file is answered as the endpoint would answer it alone, into an output file,
//! except that a `custom_id` starting with `error` goes to an error file instead.

use std::collections::BTreeMap;

use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::{Value, json};

use super::behavior::Behavior;
use super::flavour::Flavour;
use super::objects::form_parts;
use super::reply::{CREATED, fnv1a};
use super::server::{ApiError, Call, Reply};

pub(crate) struct OpenAiObjects {
    /// Makes ids unique across mock processes, as for Anthropic's objects.
    instance: String,
    next: u64,
    files: BTreeMap<String, (Value, Bytes)>,
    batches: BTreeMap<String, Value>,
}

impl Default for OpenAiObjects {
    fn default() -> Self {
        Self {
            instance: uuid::Uuid::new_v4().simple().to_string()[..8].to_owned(),
            next: 0,
            files: BTreeMap::new(),
            batches: BTreeMap::new(),
        }
    }
}

fn not_found(what: &str, id: &str) -> ApiError {
    ApiError::new(404, format!("No such {what}: '{id}'"))
}

fn list<'a>(items: impl DoubleEndedIterator<Item = &'a Value>) -> Value {
    let data: Vec<Value> = items.rev().cloned().collect();
    let first = data.first().map(|v| v["id"].clone()).unwrap_or(Value::Null);
    let last = data.last().map(|v| v["id"].clone()).unwrap_or(Value::Null);
    json!({ "object": "list", "data": data, "first_id": first, "last_id": last, "has_more": false })
}

impl OpenAiObjects {
    pub(crate) fn route(
        &mut self,
        method: &str,
        headers: &HeaderMap,
        call: &Call<'_>,
    ) -> Option<Result<Reply, ApiError>> {
        let path = call.path;
        Some(match (method, path) {
            ("POST", "/v1/files") => self.upload(headers, call),
            ("GET", "/v1/files") => Ok(Reply::Json(list(self.files.values().map(|(o, _)| o)))),
            ("POST", "/v1/batches") => self.create_batch(call),
            ("GET", "/v1/batches") => Ok(Reply::Json(list(self.batches.values()))),
            (_, p) if p.starts_with("/v1/files/") => self.file(method, &p["/v1/files/".len()..]),
            (_, p) if p.starts_with("/v1/batches/") => self.batch(method, &p["/v1/batches/".len()..]),
            _ => return None,
        })
    }

    fn add_file(&mut self, filename: &str, purpose: &str, data: Bytes) -> Value {
        self.next += 1;
        let id = format!("file-mock{}{:08}", self.instance, self.next);
        let object = json!({
            "id": id, "object": "file", "bytes": data.len(), "created_at": CREATED + self.next,
            "filename": filename, "purpose": purpose, "status": "processed"
        });
        self.files.insert(id, (object.clone(), data));
        object
    }

    fn upload(&mut self, headers: &HeaderMap, call: &Call<'_>) -> Result<Reply, ApiError> {
        let parts = form_parts(headers, &Bytes::copy_from_slice(call.body))?;
        let purpose = parts
            .iter()
            .find(|p| p.name == "purpose")
            .map(|p| String::from_utf8_lossy(&p.body).trim().to_owned())
            .ok_or_else(|| ApiError::bad_request("purpose: Field required"))?;
        let file = parts
            .into_iter()
            .find(|p| p.name == "file")
            .ok_or_else(|| ApiError::bad_request("file: Field required"))?;
        Ok(Reply::Json(self.add_file(file.filename.as_deref().unwrap_or("upload"), &purpose, file.body)))
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
                Ok(Reply::Json(json!({ "id": id, "object": "file", "deleted": true })))
            }
            _ => Err(ApiError::new(405, "mock: method not allowed")),
        }
    }

    fn create_batch(&mut self, call: &Call<'_>) -> Result<Reply, ApiError> {
        let request = call.json_object()?;
        let input =
            request["input_file_id"].as_str().ok_or_else(|| ApiError::bad_request("input_file_id is required"))?;
        if !self.files.contains_key(input) {
            return Err(not_found("file", input));
        }
        self.next += 1;
        let id = format!("batch_mock{}{:08}", self.instance, self.next);
        let batch = json!({
            "id": id, "object": "batch", "endpoint": request["endpoint"], "errors": null,
            "input_file_id": input, "completion_window": request["completion_window"],
            "status": "validating", "output_file_id": null, "error_file_id": null,
            "created_at": CREATED + self.next, "metadata": request["metadata"],
            "request_counts": { "total": 0, "completed": 0, "failed": 0 }
        });
        self.batches.insert(id, batch.clone());
        Ok(Reply::Json(batch))
    }

    fn batch(&mut self, method: &str, rest: &str) -> Result<Reply, ApiError> {
        let (id, cancel) = match rest.strip_suffix("/cancel") {
            Some(id) => (id.to_owned(), true),
            None => (rest.to_owned(), false),
        };
        let Some(batch) = self.batches.get(&id).cloned() else { return Err(not_found("batch", &id)) };
        let ended = matches!(batch["status"].as_str(), Some("completed" | "cancelled" | "failed" | "expired"));
        let batch = match (method, cancel) {
            ("POST", true) if !ended => {
                let mut b = batch;
                b["status"] = json!("cancelled");
                b["cancelled_at"] = json!(CREATED + 100);
                b
            }
            ("POST", true) => batch,
            ("GET", false) if !ended => self.run(batch)?,
            ("GET", false) => batch,
            _ => return Err(ApiError::new(405, "mock: method not allowed")),
        };
        self.batches.insert(id, batch.clone());
        Ok(Reply::Json(batch))
    }

    /// Answers every request of the batch's input file, as its endpoint would.
    fn run(&mut self, mut batch: Value) -> Result<Value, ApiError> {
        let input = batch["input_file_id"].as_str().unwrap_or_default().to_owned();
        let data = self.files.get(&input).map(|(_, d)| d.clone()).unwrap_or_default();
        let (mut output, mut errors) = (Vec::new(), Vec::new());
        let behavior = Behavior::default();
        for (n, raw) in data.split(|b| *b == b'\n').filter(|l| !l.trim_ascii().is_empty()).enumerate() {
            let line: Value =
                serde_json::from_slice(raw).map_err(|e| ApiError::bad_request(format!("line {n}: {e}")))?;
            let custom_id = line["custom_id"].as_str().unwrap_or_default();
            let request_id = format!("batch_req_mock{n:04}");
            if custom_id.starts_with("error") {
                errors.push(json!({ "id": request_id, "custom_id": custom_id, "response": null,
                                    "error": { "code": "invalid_request", "message": "mock: an erroring request" } }));
                continue;
            }
            let url = line["url"].as_str().unwrap_or_default();
            let body = Bytes::from(line["body"].to_string());
            let call =
                Call { path: url, query: "", body: &body, behavior: &behavior, seed: fnv1a(&[url.as_bytes(), &body]) };
            let reply = match url {
                "/v1/responses" => super::responses::create(&call),
                "/v1/embeddings" => super::openai::embeddings(&call),
                _ => super::openai::chat_completions(&call, Flavour::OpenAi),
            };
            match reply {
                Ok(Reply::Json(v)) => output.push(json!({ "id": request_id, "custom_id": custom_id,
                    "response": { "status_code": 200, "request_id": request_id, "body": v }, "error": null })),
                Ok(_) => return Err(ApiError::bad_request(format!("line {n}: a stream in a batch"))),
                Err(e) => errors.push(json!({ "id": request_id, "custom_id": custom_id, "response": null,
                                               "error": { "code": "invalid_request", "message": e.message } })),
            }
        }
        let jsonl = |lines: &[Value]| Bytes::from(lines.iter().map(|l| format!("{l}\n")).collect::<String>());
        let total = output.len() + errors.len();
        if !output.is_empty() {
            batch["output_file_id"] = self.add_file("batch_output.jsonl", "batch_output", jsonl(&output))["id"].clone();
        }
        if !errors.is_empty() {
            batch["error_file_id"] = self.add_file("batch_errors.jsonl", "batch_output", jsonl(&errors))["id"].clone();
        }
        batch["status"] = json!("completed");
        batch["completed_at"] = json!(CREATED + 100);
        batch["request_counts"] = json!({ "total": total, "completed": output.len(), "failed": errors.len() });
        Ok(batch)
    }
}
