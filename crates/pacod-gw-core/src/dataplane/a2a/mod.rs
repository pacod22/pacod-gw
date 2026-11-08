//! `/a2a/{name}`: remote agents through the gateway (catalogue ING-14, MCP-15; runbook section
//! 21.4), A2A 1.0 over JSON-RPC.
//!
//! A caller with a gateway key reaches an agent its key's bundles grant (`a2a:<name>`), of the
//! organisation, open to its workspace, whose card is approved and not held. The gateway serves
//! the approved card rewritten to itself (`GET /a2a/{name}/.well-known/agent-card.json`) and
//! passes messages and task requests to the agent's JSON-RPC interface with the organisation's
//! credential. The caller's key never reaches the agent.
//!
//! Each workspace's tasks are kept apart by the ids it is shown (`ids.rs`). Listing tasks,
//! push notifications (the agent would call an address of the caller's choosing) and the
//! extended card are refused: none of them can be passed on without crossing workspaces or
//! reaching out of the gateway. Every request leaves a record of a tool call (protocol `a2a`),
//! a stream when it ends, also when the caller goes away; messages count towards limits of
//! tool calls.

mod ids;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use pacod_gw_events::ToolCallRecord;
use pacod_gw_events::tool::{protocol, verdict};
use pacod_gw_events::usage::status;
use serde_json::{Value, json};

use super::Gw;
use super::auth::{Caller, identify};
use super::client_ip::ClientIp;
use super::common::GatewayParams;
use super::errors::Envelope;
use crate::crypto::digest::arguments_hmac;
use crate::limits::Subjects;
use crate::model::AgentCardPin;
use crate::snapshot::RemoteAgentRt;
use crate::sse::{SseEvent, SseFramer};
use crate::tools::card::{VERSION, is_spoken, served_card};
use crate::tools::grant::{Callee, grants_callee};

/// A2A's error codes (its section 5.4), and JSON-RPC's.
const TASK_NOT_FOUND: i64 = -32001;
const PUSH_NOT_SUPPORTED: i64 = -32003;
const UNSUPPORTED_OPERATION: i64 = -32004;
const EXTENDED_CARD_NOT_CONFIGURED: i64 = -32007;
const VERSION_NOT_SUPPORTED: i64 = -32009;
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INTERNAL_ERROR: i64 = -32603;
/// A request a limit refused: an implementation-defined server error, as on `/mcp`.
const LIMITED: i64 = -32000;

/// The methods passed on to the agent; the first two send messages, the streams are marked.
const PASSED: [(&str, bool); 5] = [
    ("SendMessage", false),
    ("SendStreamingMessage", true),
    ("GetTask", false),
    ("CancelTask", false),
    ("SubscribeToTask", true),
];
const PUSH_METHODS: [&str; 4] = [
    "CreateTaskPushNotificationConfig",
    "GetTaskPushNotificationConfig",
    "ListTaskPushNotificationConfigs",
    "DeleteTaskPushNotificationConfig",
];

fn rpc_error(id: &Value, code: i64, message: &str, http: StatusCode) -> (Response, usize) {
    let body = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
    let bytes = body.to_string().len();
    ((http, Json(body)).into_response(), bytes)
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "no such agent for this key" }))).into_response()
}

/// The agent a caller may reach by `name`, with its approved card: one of the organisation's,
/// open to the workspace, granted by one of the key's bundles, its card approved and not held.
/// Unknown and not granted read alike.
fn reachable(caller: &Caller, name: &str) -> Option<(Arc<RemoteAgentRt>, AgentCardPin)> {
    let org = caller.snapshot.organisations.get(&caller.workspace.organisation_id)?;
    let agent = org.remote_agents.get(name)?.clone();
    let ws = &caller.workspace;
    let granted = caller
        .key
        .doc
        .tool_bundles
        .iter()
        .filter_map(|b| ws.tool_bundles.get(b))
        .any(|b| grants_callee(b, Callee::Agent(name)));
    if !granted || !agent.open_to(&ws.id) {
        return None;
    }
    let card = agent.card.clone()?;
    Some((agent, card))
}

/// Where the caller reaches the agent: the gateway's address as the request names it.
fn endpoint(headers: &HeaderMap, name: &str) -> String {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("localhost");
    let scheme = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).unwrap_or("http");
    format!("{scheme}://{host}/a2a/{name}")
}

/// `GET /a2a/{name}/.well-known/agent-card.json` (and `GET /a2a/{name}`): the approved card,
/// rewritten to the gateway. A gateway key is needed: agents are an organisation's, and which
/// one a name means depends on the key.
pub async fn card(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let caller = match identify(&gw, &headers, Some(client)) {
        Ok(c) => c,
        Err(e) => return e.response(Envelope::Gateway, ""),
    };
    match reachable(&caller, &name) {
        Some((_, pin)) => Json(served_card(&pin.card, &endpoint(&headers, &name))).into_response(),
        None => not_found(),
    }
}

/// The record of an A2A request, written however it ends: when the caller goes away first, the
/// record says `cancelled`. Owned, so that a stream can carry it to its end.
struct Recording {
    gw: Arc<Gw>,
    record: Option<ToolCallRecord>,
    started: Instant,
}

impl Recording {
    fn new(gw: Arc<Gw>, record: ToolCallRecord, started: Instant) -> Self {
        gw.live.started();
        Self { gw, record: Some(record), started }
    }

    fn record(&mut self) -> &mut ToolCallRecord {
        self.record.as_mut().expect("a record is there until it is written")
    }

    fn finish(mut self, response_bytes: usize) {
        if let Some(mut r) = self.record.take() {
            r.response_bytes = response_bytes as u64;
            r.total_ms = self.started.elapsed().as_secs_f32() * 1000.0;
            self.gw.record_tool_call(&r);
        }
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.gw.live.finished();
        if let Some(mut r) = self.record.take() {
            r.status = status::CANCELLED.into();
            r.error_type = "cancelled".into();
            r.total_ms = self.started.elapsed().as_secs_f32() * 1000.0;
            self.gw.record_tool_call(&r);
        }
    }
}

/// Whether an answer's task failed: the call worked and its result is an error.
fn failed(result: &Value) -> bool {
    let state = |v: &Value| v["status"]["state"].as_str().map(str::to_owned);
    [state(&result["task"]), state(&result["statusUpdate"])]
        .iter()
        .flatten()
        .any(|s| s == "TASK_STATE_FAILED" || s == "TASK_STATE_REJECTED")
}

/// `POST /a2a/{name}`: one JSON-RPC request.
pub async fn call(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(name): Path<String>,
    req: Request,
) -> Response {
    let started = Instant::now();
    let (parts, body) = req.into_parts();
    let caller = match identify(&gw, &parts.headers, Some(client)) {
        Ok(c) => c,
        Err(e) => return e.response(Envelope::Gateway, ""),
    };
    let Some(hub) = gw.tools.clone() else { return not_found() };
    let Some((agent, pin)) = reachable(&caller, &name) else { return not_found() };
    let limit = gw.boot.max_body_bytes;
    let bytes = match http_body_util::Limited::new(body, limit).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({ "error": format!("at most {limit} bytes") })))
                .into_response();
        }
    };
    let req: Value = match serde_json::from_slice(&bytes) {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => {
            return rpc_error(&Value::Null, INVALID_REQUEST, "one JSON-RPC request per call", StatusCode::BAD_REQUEST)
                .0;
        }
        Err(_) => return rpc_error(&Value::Null, PARSE_ERROR, "not JSON", StatusCode::BAD_REQUEST).0,
    };
    let id = req["id"].clone();
    let method = req["method"].as_str().unwrap_or_default().to_owned();
    // An absent header is taken as this version: an older client's methods are named otherwise
    // and are refused as unknown.
    if let Some(v) = parts.headers.get("a2a-version").and_then(|v| v.to_str().ok())
        && !is_spoken(v)
    {
        let mut answer = json!({ "jsonrpc": "2.0", "id": id,
                                 "error": { "code": VERSION_NOT_SUPPORTED, "message": format!("A2A {v} is not supported"),
                                            "data": { "supportedVersions": [VERSION] } } });
        answer["error"]["data"]["requested"] = json!(v);
        return (StatusCode::BAD_REQUEST, Json(answer)).into_response();
    }

    let mut record = ToolCallRecord::new(uuid::Uuid::now_v7(), &gw.boot.instance_id);
    record.organisation_id = caller.workspace.organisation_id.clone();
    record.workspace_id = caller.workspace.id.clone();
    record.key_id = caller.key.id.clone();
    GatewayParams::from_headers(&parts.headers).unwrap_or_default().attribute_tool(&caller, &mut record);
    record.protocol = protocol::A2A.into();
    record.protocol_version = VERSION.into();
    record.target = agent.doc.name.clone();
    record.target_id = agent.id.clone();
    record.operation = method.clone();
    record.pin_hash = pin.hash.clone();
    record.request_bytes = bytes.len() as u64;
    let mut params = req["params"].clone();
    if !params.is_null()
        && let Some(kek) = gw.boot.keks.first()
    {
        record.arguments_hmac = arguments_hmac(kek, &record.workspace_id, &params);
    }
    let mut rec = Recording::new(gw.clone(), record, started);
    let refuse = |rec: Recording, verdict: &str, code: i64, why: &str, http: StatusCode| {
        let mut rec = rec;
        let r = rec.record();
        (r.verdict, r.status, r.http_status) = (verdict.into(), status::REJECTED.into(), http.as_u16());
        (r.rpc_error_code, r.error_type, r.error_message) = (code as i32, "not_allowed".into(), why.into());
        let (answer, n) = rpc_error(&id, code, why, http);
        rec.finish(n);
        answer
    };

    let Some(&(_, streaming)) = PASSED.iter().find(|(m, _)| *m == method) else {
        let (code, why) = match method.as_str() {
            "ListTasks" => {
                (UNSUPPORTED_OPERATION, "listing tasks is not offered through the gateway: ask for each by its id")
            }
            m if PUSH_METHODS.contains(&m) => {
                (PUSH_NOT_SUPPORTED, "push notifications are not passed on by the gateway")
            }
            "GetExtendedAgentCard" => (EXTENDED_CARD_NOT_CONFIGURED, "the gateway serves no extended card"),
            _ => (METHOD_NOT_FOUND, "the method is not an A2A 1.0 method the gateway passes on"),
        };
        return refuse(rec, verdict::DENIED, code, why, StatusCode::OK);
    };
    if !params["configuration"]["taskPushNotificationConfig"].is_null() {
        return refuse(
            rec,
            verdict::DENIED,
            PUSH_NOT_SUPPORTED,
            "push notifications are not passed on by the gateway",
            StatusCode::OK,
        );
    }
    let Some(sealer) = ids::Sealer::new(&gw.boot.keks, &agent.id, &caller.workspace.id) else {
        return refuse(rec, verdict::DENIED, INTERNAL_ERROR, "this gateway has no key-encryption key", StatusCode::OK);
    };
    if ids::inbound(&method, &mut params, &sealer).is_err() {
        return refuse(rec, verdict::DENIED, TASK_NOT_FOUND, "no such task", StatusCode::OK);
    }
    // The tenant is the one the approved card declares, whatever the caller said.
    match &pin.tenant {
        Some(t) if params.is_object() => params["tenant"] = json!(t),
        _ => {
            if let Some(p) = params.as_object_mut() {
                p.remove("tenant");
            }
        }
    }
    if method == "SendMessage" || method == "SendStreamingMessage" {
        let subjects = Subjects {
            workspace: &caller.workspace,
            key: &caller.key,
            end_user: &rec.record().end_user.clone(),
            model: "",
            template: "",
            tags: &rec.record().tags.clone(),
            intent: None,
            internal: false,
            tool: Some(Callee::Agent(&agent.doc.name)),
            agent: caller.agent.as_ref().map(|a| a.id.as_str()),
        };
        let mut applicable = gw.limits.applicable(&subjects, chrono::Utc::now());
        if let Err(refusal) = gw.limits.admit(&mut applicable) {
            let why = format!("the limit {} has no room for another request", refusal.limit_name);
            let mut rec = rec;
            rec.record().error_type = "limited".into();
            return refuse(rec, verdict::LIMITED, LIMITED, &why, StatusCode::OK);
        }
    }
    rec.record().verdict = verdict::ALLOWED.into();

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static(if streaming { "text/event-stream" } else { "application/json" }),
    );
    headers.insert("a2a-version", HeaderValue::from_static(VERSION));
    if let Some(secret) = agent.credential_for(&caller.workspace.id)
        && let (Ok(name), Ok(mut value)) = (
            reqwest::header::HeaderName::from_bytes(agent.doc.auth.header.as_bytes()),
            HeaderValue::from_str(&format!("{}{}", agent.doc.auth.prefix, secret.value.as_str())),
        )
    {
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string().into_bytes();
    let timeout = Duration::from_millis(u64::from(agent.doc.timeout_ms));
    let max_bytes = agent.doc.max_bytes as usize;
    let sent = Instant::now();
    let mut resp = match hub.client.post_raw(&pin.interface_url, headers, body, timeout).await {
        Ok(r) => r,
        Err(e) => {
            let r = rec.record();
            r.status = if matches!(e, crate::tools::client::ClientError::Timeout(_)) {
                status::TIMEOUT
            } else {
                status::ERROR
            }
            .into();
            (r.error_type, r.error_message, r.rpc_error_code) = (e.kind().into(), e.to_string(), INTERNAL_ERROR as i32);
            let (answer, n) = rpc_error(&id, INTERNAL_ERROR, &format!("{name}: {e}"), StatusCode::OK);
            rec.finish(n);
            return answer;
        }
    };
    let http = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let is_stream = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));
    rec.record().http_status = http.as_u16();
    if streaming && is_stream && http.is_success() {
        return relay(rec, resp, sealer, max_bytes, sent);
    }
    let answer = match crate::tools::client::read_body(&mut resp, max_bytes).await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
        Err(e) => {
            let r = rec.record();
            (r.status, r.error_type, r.error_message) = (status::ERROR.into(), e.kind().into(), e.to_string());
            r.truncated = matches!(e, crate::tools::client::ClientError::TooLarge(_));
            None
        }
    };
    let r = rec.record();
    r.upstream_ms = sent.elapsed().as_secs_f32() * 1000.0;
    let Some(mut answer) = answer else {
        if r.status.is_empty() {
            (r.status, r.error_type) = (status::ERROR.into(), "invalid_response".into());
        }
        r.rpc_error_code = INTERNAL_ERROR as i32;
        let (answer, n) =
            rpc_error(&id, INTERNAL_ERROR, &format!("{name}: the agent's answer could not be read"), StatusCode::OK);
        rec.finish(n);
        return answer;
    };
    if let Some(code) = answer["error"]["code"].as_i64() {
        (r.status, r.error_type) = (status::ERROR.into(), "upstream_error".into());
        r.rpc_error_code = i32::try_from(code).unwrap_or_default();
        r.error_message = answer["error"]["message"].as_str().unwrap_or_default().to_owned();
    } else {
        r.status = status::OK.into();
        r.result_error = failed(&answer["result"]);
    }
    if let Some(result) = answer.get_mut("result") {
        ids::outbound(&method, result, &sealer);
    }
    answer["id"] = id;
    let text = answer.to_string();
    let n = text.len();
    rec.finish(n);
    (http, [(header::CONTENT_TYPE, "application/json")], text).into_response()
}

/// Relays a stream of server-sent events, each a JSON-RPC response around one event of the task
/// (A2A section 9.4.2), with its ids rewritten for the caller. An event larger than the agent's
/// limit ends the stream with an error. The record is written when the stream ends, or when the
/// caller goes away.
///
/// The framer works on bytes, so that a character cut between two chunks stays whole, and
/// finds events however their lines end (the official SDKs end them with CRLF).
fn relay(rec: Recording, resp: reqwest::Response, sealer: ids::Sealer, max_bytes: usize, sent: Instant) -> Response {
    struct State {
        upstream: futures_util::stream::BoxStream<'static, reqwest::Result<Bytes>>,
        framer: SseFramer,
        events: VecDeque<SseEvent>,
        rec: Option<Recording>,
        sealer: ids::Sealer,
        sent_bytes: usize,
        /// The agent ended the stream; the events still queued are sent before it ends here.
        ended: bool,
        done: bool,
    }
    let state = State {
        upstream: resp.bytes_stream().boxed(),
        framer: SseFramer::new(),
        events: VecDeque::new(),
        rec: Some(rec),
        sealer,
        sent_bytes: 0,
        ended: false,
        done: false,
    };
    let stream = futures_util::stream::unfold(state, move |mut st| async move {
        loop {
            // A complete event: rewrite and send it.
            if let Some(event) = st.events.pop_front() {
                let Ok(mut message) = serde_json::from_slice::<Value>(&event.data) else { continue };
                if let Some(result) = message.get_mut("result") {
                    if failed(result)
                        && let Some(rec) = st.rec.as_mut()
                    {
                        rec.record().result_error = true;
                    }
                    ids::outbound("SendStreamingMessage", result, &st.sealer);
                } else if let (Some(code), Some(rec)) = (message["error"]["code"].as_i64(), st.rec.as_mut()) {
                    let r = rec.record();
                    (r.status, r.error_type) = (status::ERROR.into(), "upstream_error".into());
                    r.rpc_error_code = i32::try_from(code).unwrap_or_default();
                }
                let out = format!("data: {message}\n\n");
                st.sent_bytes += out.len();
                return Some((Ok::<Bytes, std::io::Error>(Bytes::from(out)), st));
            }
            if st.done {
                return None;
            }
            if st.ended {
                st.done = true;
                if let Some(mut rec) = st.rec.take() {
                    let r = rec.record();
                    if r.status.is_empty() {
                        r.status = status::OK.into();
                    }
                    r.upstream_ms = sent.elapsed().as_secs_f32() * 1000.0;
                    rec.finish(st.sent_bytes);
                }
                return None;
            }
            if st.framer.pending().len() > max_bytes {
                st.done = true;
                if let Some(mut rec) = st.rec.take() {
                    let r = rec.record();
                    (r.status, r.error_type, r.truncated) = (status::ERROR.into(), "too_large".into(), true);
                    rec.finish(st.sent_bytes);
                }
                let error = json!({ "jsonrpc": "2.0", "error": { "code": INTERNAL_ERROR,
                                    "message": format!("an event of the agent is larger than {max_bytes} bytes") } });
                return Some((Ok(Bytes::from(format!("data: {error}\n\n"))), st));
            }
            match st.upstream.next().await {
                Some(Ok(chunk)) => st.framer.feed(&chunk, |event| st.events.push_back(event)),
                Some(Err(e)) => {
                    st.done = true;
                    if let Some(mut rec) = st.rec.take() {
                        let r = rec.record();
                        (r.status, r.error_type, r.error_message) =
                            (status::ERROR.into(), "unavailable".into(), e.to_string());
                        r.upstream_ms = sent.elapsed().as_secs_f32() * 1000.0;
                        rec.finish(st.sent_bytes);
                    }
                    return None;
                }
                None => {
                    // An event the agent did not end with a blank line is still sent.
                    st.framer.finish(|event| st.events.push_back(event));
                    st.ended = true;
                }
            }
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
