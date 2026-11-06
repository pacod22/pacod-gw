//! The HTTP server: request capture, script resolution, routing and response assembly.

use std::{
    collections::{HashMap, VecDeque},
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, RETRY_AFTER},
        response::Builder,
    },
    response::Response,
    serve::ListenerExt,
};
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use parking_lot::Mutex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};
use tokio::{
    net::{TcpListener, ToSocketAddrs},
    sync::watch,
    task::JoinHandle,
};
use uuid::Uuid;

use super::{
    anthropic,
    behavior::{Behavior, BehaviorError},
    flavour::Flavour,
    gemini, ollama, openai,
    reply::fnv1a,
    stream::{self, Frame},
};

/// The request header that selects scripted behaviour for one request.
const X_MOCK: &str = "x-mock";

/// A request the mock received, exactly as it arrived.
///
/// Serialises to the JSON of `GET /__mock/requests`: `headers` as an array of `[name, value]`
/// pairs and the body as base64 in `body_base64`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedRequest {
    /// The method, upper case.
    pub method: String,
    /// The path with the query string, as sent.
    pub path: String,
    /// The headers in the order received. Names are lower case, as the HTTP parser hands
    /// them over; values that are not UTF-8 are converted lossily. A repeated name has one
    /// entry per value, all at the position of its first occurrence.
    pub headers: Vec<(String, String)>,
    /// The exact body bytes.
    #[serde(rename = "body_base64", with = "base64_body")]
    pub body: Bytes,
}

impl CapturedRequest {
    /// The first value of the header `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(candidate, _)| candidate.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    /// The body parsed as JSON.
    pub fn json(&self) -> serde_json::Result<Value> {
        serde_json::from_slice(&self.body)
    }
}

mod base64_body {
    use super::{Bytes, Deserialize, Deserializer, STANDARD, Serializer};
    use base64::Engine as _;

    pub fn serialize<S: Serializer>(body: &Bytes, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(body))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Bytes, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD.decode(text).map(Bytes::from).map_err(serde::de::Error::custom)
    }
}

/// A queued behaviour and how many of its `fail` responses were already sent.
struct Queued {
    behavior: Behavior,
    failed: u32,
}

/// State shared by the handle and the request handlers.
pub(crate) struct Shared {
    captured: Mutex<VecDeque<CapturedRequest>>,
    /// How many captured requests are kept; older ones are dropped.
    capture_limit: AtomicUsize,
    script: Mutex<VecDeque<Queued>>,
    /// Failures already served for a `fail` option that came in the `x-mock` header, keyed by
    /// the request (method, path, body) and the option.
    header_fails: Mutex<HashMap<u64, u32>>,
    /// Anthropic's files and message batches.
    objects: Mutex<super::objects::Objects>,
    /// OpenAI's files and batches.
    openai_objects: Mutex<super::openai_objects::OpenAiObjects>,
    /// The access tokens the emulated Google token endpoint issued.
    pub(crate) google: Mutex<super::google::Tokens>,
    /// Which version of the price tracker's pricing page the mock's site serves (1 or 2).
    pricing_variant: std::sync::atomic::AtomicU8,
    shutdown: watch::Receiver<bool>,
}

impl Shared {
    pub(crate) fn capture(&self, request: impl FnOnce() -> CapturedRequest) {
        let limit = self.capture_limit.load(Ordering::Relaxed);
        if limit == 0 {
            return;
        }
        let request = request();
        let mut captured = self.captured.lock();
        while captured.len() >= limit {
            captured.pop_front();
        }
        captured.push_back(request);
    }

    fn captured(&self) -> Vec<CapturedRequest> {
        self.captured.lock().iter().cloned().collect()
    }

    fn set_script(&self, script: impl IntoIterator<Item = Behavior>) {
        *self.script.lock() = script.into_iter().map(|behavior| Queued { behavior, failed: 0 }).collect();
        self.header_fails.lock().clear();
    }

    /// The behaviour for one provider request: the head of the script queue, overlaid by the
    /// `x-mock` header. The second value is the error status to answer with, if any.
    fn resolve(&self, header: Option<Behavior>, request_key: u64) -> (Behavior, Option<u16>) {
        let header_fail = header.as_ref().and_then(|behavior| behavior.fail);
        let mut failing = None;
        let mut behavior = Behavior::default();
        {
            let mut script = self.script.lock();
            if let Some(head) = script.front_mut() {
                // An entry with `fail` stays at the head until its failures are used up, then
                // serves one more request normally.
                let fail = head.behavior.fail.filter(|fail| header_fail.is_none() && head.failed < fail.times);
                if let Some(fail) = fail {
                    head.failed += 1;
                    failing = Some(fail.status);
                    behavior = head.behavior.clone();
                } else if let Some(head) = script.pop_front() {
                    behavior = head.behavior;
                }
            }
        }
        if let Some(header) = &header {
            behavior = behavior.overlaid(header);
        }
        if let Some(fail) = header_fail {
            let key = fnv1a(&[&request_key.to_be_bytes(), &fail.status.to_be_bytes(), &fail.times.to_be_bytes()]);
            let mut fails = self.header_fails.lock();
            let failed = fails.entry(key).or_insert(0);
            if *failed < fail.times {
                *failed += 1;
                failing = Some(fail.status);
            } else {
                fails.remove(&key);
            }
        }
        let status = behavior.status.or(failing);
        (behavior, status)
    }
}

/// A scripted mock of the OpenAI-compatible, Anthropic, Gemini and Ollama APIs on one port.
///
/// The server runs in the background and stops when the handle is dropped. See the crate
/// README for the emulated endpoints, the `x-mock` options and the control endpoints.
pub struct MockProvider {
    addr: SocketAddr,
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl MockProvider {
    /// Starts a mock on `127.0.0.1` with a free port.
    ///
    /// # Panics
    ///
    /// Panics if no local port can be bound.
    pub async fn spawn() -> Self {
        Self::bind("127.0.0.1:0").await.expect("mock provider could not bind a local port")
    }

    /// Starts a mock on `addr`.
    pub async fn bind(addr: impl ToSocketAddrs) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let (shutdown, signal) = watch::channel(false);
        let shared = Arc::new(Shared {
            captured: Mutex::default(),
            capture_limit: AtomicUsize::new(usize::MAX),
            script: Mutex::default(),
            header_fails: Mutex::default(),
            objects: Mutex::default(),
            openai_objects: Mutex::default(),
            google: Mutex::default(),
            pricing_variant: std::sync::atomic::AtomicU8::new(1),
            shutdown: signal.clone(),
        });
        let app = Router::new()
            .route("/__ws/echo", axum::routing::get(super::websocket::echo))
            .route("/v1/realtime", axum::routing::get(super::realtime::connect))
            .fallback(handle)
            .with_state(shared.clone());
        // Without Nagle's algorithm every stream chunk leaves as soon as it is written.
        let listener = listener.tap_io(|tcp| {
            let _ = tcp.set_nodelay(true);
        });
        let task = tokio::spawn(async move {
            let served = axum::serve(listener, app).with_graceful_shutdown(wait_shutdown(signal)).await;
            if let Err(error) = served {
                tracing::error!(%error, "mock provider stopped serving");
            }
        });
        Ok(Self { addr, shared, shutdown, task: Some(task) })
    }

    /// The address the mock listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://<addr>`, without a trailing slash.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every provider request received so far, oldest first. Requests to `/healthz` and
    /// `/__mock/*` are not recorded.
    pub fn captured(&self) -> Vec<CapturedRequest> {
        self.shared.captured()
    }

    /// Keeps only the `limit` most recent captured requests from now on; `0` captures
    /// nothing. The default keeps every request, which a long-running mock under load should
    /// not do.
    pub fn set_capture_limit(&self, limit: usize) {
        self.shared.capture_limit.store(limit, Ordering::Relaxed);
    }

    /// Forgets the captured requests. The script queue is left alone.
    pub fn clear(&self) {
        self.shared.captured.lock().clear();
    }

    /// Replaces the script queue: the n-th provider request from now on is answered with the
    /// n-th behaviour, and requests after the queue is used up are answered normally. An entry
    /// with `fail=<code>x<n>` covers `n + 1` requests. Also forgets the state of `fail` options
    /// that came in `x-mock` headers.
    pub fn set_script(&self, script: impl IntoIterator<Item = Behavior>) {
        self.shared.set_script(script);
    }

    /// The number of queued behaviours that no request has used up yet.
    pub fn pending_script(&self) -> usize {
        self.shared.script.lock().len()
    }

    /// Stops the server and waits until it has released its port. Dropping the handle stops
    /// the server too, without waiting.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(mut task) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(5), &mut task).await.is_err()
        {
            task.abort();
        }
    }
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// Resolves when the mock shuts down or its handle is dropped.
pub(crate) async fn wait_shutdown(mut signal: watch::Receiver<bool>) {
    while !*signal.borrow_and_update() {
        if signal.changed().await.is_err() {
            return;
        }
    }
}

/// The provider whose API a request addresses; it decides the error envelope and the headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    OpenAi,
    Anthropic,
    Gemini,
    Ollama,
}

impl Family {
    fn of(path: &str, headers: &HeaderMap) -> Self {
        if path.starts_with("/v1beta/") {
            Self::Gemini
        } else if path.starts_with("/api/") {
            Self::Ollama
        } else if path.starts_with("/v1/messages")
            // `GET /v1/models` and `/v1/files` exist in both APIs; Anthropic's headers tell them apart.
            || ((path == "/v1/models" || path.starts_with("/v1/models/") || path == "/v1/files" || path.starts_with("/v1/files/"))
                && (headers.contains_key("anthropic-version") || headers.contains_key("x-api-key")))
        {
            Self::Anthropic
        } else {
            Self::OpenAi
        }
    }

    fn json_content_type(self) -> &'static str {
        match self {
            Self::OpenAi | Self::Anthropic => "application/json",
            Self::Gemini => "application/json; charset=UTF-8",
            Self::Ollama => "application/json; charset=utf-8",
        }
    }
}

/// One provider request, as the emulations see it.
pub(crate) struct Call<'a> {
    pub path: &'a str,
    pub query: &'a str,
    pub body: &'a Bytes,
    pub behavior: &'a Behavior,
    /// A hash of the path, the query and the body.
    pub seed: u64,
}

impl Call<'_> {
    /// The body as a JSON object, or the 400 a provider answers to anything else.
    pub fn json_object(&self) -> Result<Value, ApiError> {
        match serde_json::from_slice::<Value>(self.body) {
            Ok(value) if value.is_object() => Ok(value),
            Ok(_) => Err(ApiError::bad_request("request body must be a JSON object")),
            Err(error) => Err(ApiError::bad_request(format!("request body is not valid JSON: {error}"))),
        }
    }
}

/// An error a provider reports; the server wraps it in the provider's envelope.
pub(crate) struct ApiError {
    pub status: u16,
    pub message: String,
}

impl ApiError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }
}

/// A successful answer of an emulation.
pub(crate) enum Reply {
    Json(Value),
    Bytes { content_type: &'static str, body: Bytes },
    Stream { content_type: &'static str, frames: Vec<Frame> },
}

async fn handle(State(shared): State<Arc<Shared>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(body) => body,
        Err(error) => {
            return mock_error(StatusCode::BAD_REQUEST, &format!("could not read the request body: {error}"));
        }
    };
    let path = parts.uri.path();
    if path == "/healthz" || path.starts_with("/__mock/") || path == "/robots.txt" || path == "/sitemap.xml" {
        // The price tracker's site names its pages by the address it is reached at.
        let host = parts.headers.get(axum::http::header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
        return control(&shared, &parts.method, path, &body, host);
    }

    let path_and_query = parts.uri.path_and_query().map_or(path, |target| target.as_str());
    shared.capture(|| CapturedRequest {
        method: parts.method.to_string(),
        path: path_and_query.to_owned(),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
            .collect(),
        body: body.clone(),
    });

    let (in_path, path) = match path_behavior(path) {
        Ok(split) => split,
        Err(error) => {
            return mock_error(StatusCode::BAD_REQUEST, &format!("invalid {X_MOCK} options in the path: {error}"));
        }
    };
    let header = match header_behavior(&parts.headers) {
        Ok(header) => header,
        Err(error) => {
            return mock_error(StatusCode::BAD_REQUEST, &format!("invalid {X_MOCK} header: {error}"));
        }
    };
    // Options in the path count as a header the real header overrides.
    let header = match (in_path, header) {
        (Some(in_path), Some(header)) => Some(in_path.overlaid(&header)),
        (in_path, header) => header.or(in_path),
    };
    let seed = fnv1a(&[path_and_query.as_bytes(), &body]);
    let request_key = fnv1a(&[parts.method.as_str().as_bytes(), &seed.to_be_bytes()]);
    let (behavior, error_status) = shared.resolve(header, request_key);
    tracing::debug!(method = %parts.method, path = path_and_query, ?behavior, "mock provider request");

    if behavior.hang {
        // Never answer. The future is dropped when the client gives up; at shutdown the
        // connection is closed with a response nobody is expected to read.
        wait_shutdown(shared.shutdown.clone()).await;
        return mock_error(StatusCode::SERVICE_UNAVAILABLE, "shutting down");
    }
    if let Some(delay) = behavior.delay {
        tokio::time::sleep(delay).await;
    }

    // An emulation prefix picks the provider; the rest of the path is routed as usual.
    let (flavour, path) = Flavour::strip(path);
    let family = match flavour {
        Flavour::OpenAi => Family::of(path, &parts.headers),
        Flavour::Gemini | Flavour::Vertex => Family::Gemini,
        Flavour::DeepSeek | Flavour::Glm | Flavour::Kimi | Flavour::GeminiOpenai | Flavour::Ollama | Flavour::Xai => {
            Family::OpenAi
        }
    };
    let request_id = behavior.request_id.clone().unwrap_or_else(|| format!("req_{}", Uuid::new_v4().simple()));
    let call = Call { path, query: parts.uri.query().unwrap_or(""), body: &body, behavior: &behavior, seed };
    let outcome = match error_status {
        Some(status) => Err(ApiError::new(status, format!("mock: scripted error {status}"))),
        None => route(&shared, family, flavour, &parts.method, &parts.headers, &call),
    };

    let builder = provider_headers(Response::builder(), family, &request_id);
    let response = match outcome {
        Ok(Reply::Json(value)) => {
            builder.header(CONTENT_TYPE, family.json_content_type()).body(Body::from(value.to_string()))
        }
        Ok(Reply::Bytes { content_type, body }) => builder.header(CONTENT_TYPE, content_type).body(Body::from(body)),
        Ok(Reply::Stream { content_type, frames }) => builder
            .header(CONTENT_TYPE, content_type)
            .header(CACHE_CONTROL, "no-cache")
            .body(stream::body(frames, &behavior, shared.shutdown.clone())),
        Err(error) => {
            let envelope =
                flavour.error_body(error.status, &error.message, behavior.code.as_deref()).unwrap_or_else(|| {
                    match family {
                        Family::OpenAi => openai::error_body(error.status, &error.message),
                        Family::Anthropic => anthropic::error_body(error.status, &error.message, &request_id),
                        Family::Gemini => gemini::error_body(error.status, &error.message),
                        Family::Ollama => ollama::error_body(&error.message),
                    }
                });
            let mut builder = builder.status(error.status).header(CONTENT_TYPE, family.json_content_type());
            // Only scripted errors carry `retry-after`; the option has no other effect.
            if error_status.is_some()
                && let Some(retry_after) = &behavior.retry_after
            {
                builder = builder.header(RETRY_AFTER, retry_after);
            }
            builder.body(Body::from(envelope.to_string()))
        }
    };
    response.unwrap_or_else(|error| {
        mock_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("could not build the response: {error}"))
    })
}

fn route(
    shared: &Shared,
    family: Family,
    flavour: Flavour,
    method: &Method,
    headers: &HeaderMap,
    call: &Call<'_>,
) -> Result<Reply, ApiError> {
    match flavour {
        Flavour::OpenAi => {}
        // Native Gemini with Gemini 3's checks.
        Flavour::Gemini => {
            return match (method.as_str(), call.path) {
                ("POST", path) if path.starts_with("/v1beta/models/") => gemini::post(call, gemini::Site::Studio),
                ("GET", "/v1beta/models") => Ok(gemini::models()),
                ("GET", path) if path.starts_with("/v1beta/models/") => {
                    let id = &path["/v1beta/models/".len()..];
                    if id.is_empty() || id.starts_with("unknown") {
                        return Err(ApiError::new(404, format!("models/{id} is not found for API version v1beta")));
                    }
                    Ok(gemini::model(id))
                }
                _ => Err(ApiError::new(404, format!("mock: no route for {method} {}", call.path))),
            };
        }
        // Gemini on Vertex AI.
        Flavour::Vertex => return super::google::vertex(shared, method, headers, call),
        // An emulated OpenAI-compatible API: matched by the end of the path, below whatever
        // base path the provider has.
        _ => {
            return match (method.as_str(), call.path) {
                ("POST", path) if path.ends_with("/chat/completions") => openai::chat_completions(call, flavour),
                ("POST", path) if path.ends_with("/responses") => super::responses::create(call),
                ("POST", path) if path.ends_with("/audio/transcriptions") => {
                    super::audio::transcribe(call, headers, false)
                }
                ("POST", path) if path.ends_with("/audio/translations") => {
                    super::audio::transcribe(call, headers, true)
                }
                ("GET", path) if path.ends_with("/models") => Ok(openai::models(flavour)),
                ("GET", path) if path.contains("/models/") => {
                    let id = &path[path.rfind("/models/").map_or(0, |i| i + "/models/".len())..];
                    if id.is_empty() || id.starts_with("unknown") {
                        return Err(ApiError::new(404, format!("mock: the model '{id}' does not exist")));
                    }
                    Ok(openai::model(id))
                }
                _ => Err(ApiError::new(404, format!("mock: no route for {method} {}", call.path))),
            };
        }
    }
    if family == Family::Anthropic {
        anthropic::authorize(headers)?;
        if let Some(reply) = shared.objects.lock().route(method.as_str(), headers, call) {
            return reply;
        }
    } else if let Some(reply) = shared.openai_objects.lock().route(method.as_str(), headers, call) {
        return reply;
    }
    match (method.as_str(), call.path) {
        ("POST", "/v1/chat/completions") => openai::chat_completions(call, Flavour::OpenAi),
        ("POST", "/v1/responses") => super::responses::create(call),
        ("POST", "/v1/images/generations") => super::images::generations(call),
        ("POST", "/v1/images/edits") => super::images::edits(call, headers),
        ("POST", "/v1/audio/transcriptions") => super::audio::transcribe(call, headers, false),
        ("POST", "/v1/audio/translations") => super::audio::transcribe(call, headers, true),
        ("POST", path) if path.ends_with("/rerank") => super::rerank::rerank(call),
        ("POST", "/emulate/google-oauth/token") => super::google::token(shared, call),
        ("POST", "/v1/embeddings") => openai::embeddings(call),
        ("POST", "/v1/audio/speech") => openai::speech(call),
        ("GET", "/v1/models") if family == Family::Anthropic => Ok(anthropic::models()),
        ("GET", "/v1/models") => Ok(openai::models(Flavour::OpenAi)),
        ("GET", path) if path.starts_with("/v1/models/") => {
            // Every model id is known, except those that say they are not.
            let id = &path["/v1/models/".len()..];
            if id.is_empty() || id.starts_with("unknown") {
                return Err(ApiError::new(404, format!("mock: the model '{id}' does not exist")));
            }
            Ok(if family == Family::Anthropic { anthropic::model(id) } else { openai::model(id) })
        }
        ("POST", "/v1/messages") => anthropic::messages(call),
        ("POST", "/v1/messages/count_tokens") => anthropic::count_message_tokens(call),
        ("POST", path) if path.starts_with("/v1beta/models/") => gemini::post(call, gemini::Site::Plain),
        ("POST", path) if path.ends_with("/systemone") => super::typesafe::systemone(call),
        ("POST", "/api/chat") => ollama::chat(call),
        ("GET", "/api/tags") => Ok(ollama::tags()),
        ("POST", "/api/show") => ollama::show(call),
        ("GET", "/api/version") => Ok(ollama::version()),
        _ => Err(ApiError::new(404, format!("mock: no route for {method} {}", call.path))),
    }
}

/// The response headers a provider sends with every answer, errors included.
fn provider_headers(mut builder: Builder, family: Family, request_id: &str) -> Builder {
    let (id_header, fixed) = match family {
        Family::OpenAi => ("x-request-id", openai::HEADERS),
        Family::Anthropic => ("request-id", anthropic::HEADERS),
        // Neither API sends a request id or rate-limit headers.
        Family::Gemini | Family::Ollama => return builder,
    };
    builder = builder.header(id_header, request_id);
    for (name, value) in fixed {
        builder = builder.header(*name, *value);
    }
    builder
}

/// The `x-mock` header, if present. Repeated headers are read as one list.
/// The prefix of a path that carries options: `/x-mock/delay=200,chunks=8/v1/chat/completions`.
const PATH_OPTIONS: &str = "/x-mock/";

/// Splits `/x-mock/<options>/<rest>` into the options and `/<rest>`; any other path is
/// returned as it is. The full gateway forwards no `x-mock` header in managed mode, so a load
/// test puts the options into the provider's base URL instead, which the gateway keeps in
/// front of every path it calls.
fn path_behavior(path: &str) -> Result<(Option<Behavior>, &str), BehaviorError> {
    let Some(rest) = path.strip_prefix(PATH_OPTIONS) else { return Ok((None, path)) };
    let (spec, tail) = match rest.find('/') {
        Some(slash) => (&rest[..slash], &rest[slash..]),
        None => (rest, "/"),
    };
    Ok((Some(Behavior::parse(spec)?), tail))
}

fn header_behavior(headers: &HeaderMap) -> Result<Option<Behavior>, BehaviorError> {
    let mut spec = String::new();
    for value in headers.get_all(X_MOCK) {
        spec.push_str(&String::from_utf8_lossy(value.as_bytes()));
        spec.push(',');
    }
    if spec.is_empty() { Ok(None) } else { Behavior::parse(&spec).map(Some) }
}

/// `/healthz` and `/__mock/*`: the endpoints that steer the mock rather than emulate a
/// provider. They are never captured and never consume the script.
fn control(shared: &Shared, method: &Method, path: &str, body: &Bytes, host: &str) -> Response {
    match (method.as_str(), path) {
        ("GET", "/healthz") => json_response(StatusCode::OK, &json!({ "status": "ok" })),
        ("GET", "/__mock/requests") => match serde_json::to_value(shared.captured()) {
            Ok(captured) => json_response(StatusCode::OK, &captured),
            Err(error) => mock_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
        },
        ("DELETE", "/__mock/requests") => {
            shared.captured.lock().clear();
            empty_response(StatusCode::NO_CONTENT)
        }
        ("POST", "/__mock/script") => {
            let specs: Vec<String> = match serde_json::from_slice(body) {
                Ok(specs) => specs,
                Err(error) => {
                    return mock_error(
                        StatusCode::BAD_REQUEST,
                        &format!("the script must be a JSON array of option lists: {error}"),
                    );
                }
            };
            match Behavior::parse_all(&specs) {
                Ok(script) => {
                    shared.set_script(script);
                    json_response(StatusCode::OK, &json!({ "queued": specs.len() }))
                }
                Err(error) => mock_error(StatusCode::BAD_REQUEST, &error.to_string()),
            }
        }
        ("DELETE", "/__mock/script") => {
            shared.set_script([]);
            empty_response(StatusCode::NO_CONTENT)
        }
        // How many access tokens the emulated Google token endpoint issued.
        ("GET", "/__mock/google") => json_response(StatusCode::OK, &json!({ "minted": shared.google.lock().minted })),
        // A price list in the gateway's feed format, for the price tracker. The rates are
        // fixed: `tracked-steady` is what an importer would have entered plus ten per cent,
        // `tracked-jump` is double.
        ("GET", "/__mock/prices.json") => json_response(
            StatusCode::OK,
            &json!({ "currency": "USD", "models": {
                "tracked-steady": { "input": 2.2, "output": 10.0 },
                "tracked-jump": { "input": 4.0, "output": 20.0, "cache_read": 0.4 }
            } }),
        ),
        // The same figures as a pricing page, for extraction.
        ("GET", "/__mock/pricing.html") => {
            let mut response = Response::new(Body::from(
                "<html><head><title>Pricing</title><script>var x = 1;</script></head><body><h1>Pricing</h1>\
                 <table><tr><td>tracked-steady</td><td>$2.20 / 1M input tokens</td><td>$10.00 / 1M output tokens</td></tr>\
                 <tr><td>tracked-jump</td><td>$4.00 / 1M input tokens</td><td>$20.00 / 1M output tokens</td></tr></table></body></html>",
            ));
            response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
            response
        }
        // A provider's site for the price tracker: the pages its scan finds and its check reads.
        ("GET", "/robots.txt") => {
            text_response("text/plain", "User-agent: *\nDisallow: /__mock/site/private/\n".into())
        }
        ("GET", "/sitemap.xml") => {
            let urls: String = ["about", "pricing", "models", "news", "private/pricing"]
                .iter()
                .map(|p| format!("<url><loc>http://{host}/__mock/site/{p}</loc></url>"))
                .collect();
            text_response("application/xml", format!("<?xml version=\"1.0\"?><urlset>{urls}</urlset>"))
        }
        // The same models as sources of model data list them. `/__mock/catalog` is also an
        // address for PGW_AGGREGATOR_URL: the gateway reads `/v1/catalog` below it.
        ("GET", p) if p.starts_with("/__mock/catalog/") => {
            let variant = shared.pricing_variant.load(Ordering::Relaxed);
            match catalog_feed(&p["/__mock/catalog/".len()..], variant) {
                Some(list) => json_response(StatusCode::OK, &list),
                None => mock_error(StatusCode::NOT_FOUND, &format!("no list {path}")),
            }
        }
        ("POST", "/__mock/pricing") => {
            let variant = serde_json::from_slice::<Value>(body).ok().and_then(|v| v["variant"].as_u64()).unwrap_or(1);
            shared.pricing_variant.store(if variant == 2 { 2 } else { 1 }, Ordering::Relaxed);
            json_response(StatusCode::OK, &json!({ "variant": variant }))
        }
        ("GET", p) if p.starts_with("/__mock/site/") => {
            let variant = shared.pricing_variant.load(Ordering::Relaxed);
            match site_page(&p["/__mock/site/".len()..], variant) {
                Some(html) => text_response("text/html; charset=utf-8", html),
                None => mock_error(StatusCode::NOT_FOUND, &format!("no page {path}")),
            }
        }
        _ => mock_error(StatusCode::NOT_FOUND, &format!("no control endpoint {method} {path}")),
    }
}

fn text_response(content_type: &'static str, body: String) -> Response {
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

/// The figures of the mock's site as a source of model data lists them, in the shape of `format`:
/// pacod-aggregator's catalogue, LiteLLM's list or OpenRouter's. The models are the vendor
/// `openai`'s. As on the pricing page, variant 2 changes `tracked-steady`'s price and lists
/// `tracked-new`.
fn catalog_feed(format: &str, variant: u8) -> Option<Value> {
    struct Listed {
        id: &'static str,
        input: f64,
        output: f64,
        cached: f64,
        context: u64,
        max_output: u64,
        vision: bool,
    }
    let steady = if variant == 2 { 2.4 } else { 2.2 };
    let mut models = vec![
        Listed {
            id: "tracked-steady",
            input: steady,
            output: 10.0,
            cached: 0.22,
            context: 128_000,
            max_output: 16_384,
            vision: true,
        },
        Listed {
            id: "tracked-jump",
            input: 4.0,
            output: 20.0,
            cached: 0.4,
            context: 200_000,
            max_output: 32_768,
            vision: false,
        },
    ];
    if variant == 2 {
        models.push(Listed {
            id: "tracked-new",
            input: 1.0,
            output: 3.0,
            cached: 0.1,
            context: 64_000,
            max_output: 8_192,
            vision: false,
        });
    }
    // Per token, as LiteLLM and OpenRouter write it: as text, so that no float rounding shows.
    let per_token = |rate: f64| format!("{:.10}", rate / 1_000_000.0);
    match format {
        "v1.json" | "v1/catalog" => Some(json!({
            "schema": "pacod.catalog/v1",
            "source": { "kind": "pacod_aggregator" },
            "vendors": [{ "id": "openai", "display_name": "OpenAI", "currency": "USD", "complete": true }],
            "models": models.iter().map(|m| json!({
                "vendor": "openai", "api_id": m.id, "api_id_confirmed": true, "kind": "chat",
                "properties": { "context_window": m.context, "max_output_tokens": m.max_output, "vision": m.vision },
                "price": { "currency": "USD",
                           "rates": { "input": m.input, "output": m.output, "cache_read": m.cached } },
            })).collect::<Vec<_>>(),
        })),
        "litellm.json" => Some(Value::Object(
            models
                .iter()
                .map(|m| {
                    (
                        m.id.to_owned(),
                        json!({
                            "litellm_provider": "openai", "mode": "chat",
                            "input_cost_per_token": per_token(m.input).parse::<f64>().unwrap_or_default(),
                            "output_cost_per_token": per_token(m.output).parse::<f64>().unwrap_or_default(),
                            "cache_read_input_token_cost": per_token(m.cached).parse::<f64>().unwrap_or_default(),
                            "max_input_tokens": m.context, "max_output_tokens": m.max_output,
                            "supports_vision": m.vision,
                        }),
                    )
                })
                .collect(),
        )),
        "openrouter.json" => Some(json!({ "data": models.iter().map(|m| {
            let input = if m.vision { json!(["text", "image"]) } else { json!(["text"]) };
            json!({
                "id": format!("openai/{}", m.id), "context_length": m.context,
                "architecture": { "input_modalities": input, "output_modalities": ["text"] },
                "pricing": { "prompt": per_token(m.input), "completion": per_token(m.output),
                             "input_cache_read": per_token(m.cached) },
                "top_provider": { "max_completion_tokens": m.max_output },
            })
        }).collect::<Vec<_>>() })),
        _ => None,
    }
}

/// The pages of the mock's site. Variant 2 of the pricing page changes `tracked-steady`'s price
/// and lists `tracked-new`, which no repository has.
fn site_page(page: &str, variant: u8) -> Option<String> {
    let html = |title: &str, body: &str| {
        format!("<html><head><title>{title}</title></head><body><h1>{title}</h1>{body}</body></html>")
    };
    Some(match page {
        "about" => html(
            "About the mock",
            "<p>The mock provider serves models. See <a href=\"/__mock/site/pricing\">pricing</a> and \
             <a href=\"/__mock/site/models\">models</a>.</p>",
        ),
        "pricing" => {
            let (steady, new) = match variant {
                2 => (
                    "$2.40",
                    "<tr><td>tracked-new</td><td>$1.00 / 1M tokens</td><td>$3.00 / 1M tokens</td><td>$0.10 / 1M tokens</td></tr>",
                ),
                _ => ("$2.20", ""),
            };
            html(
                "Pricing",
                &format!(
                    "<p>Prices per one million tokens.</p><table><tr><th>Model</th><th>Input</th><th>Output</th>\
                     <th>Cached input</th></tr><tr><td>tracked-steady</td><td>{steady} / 1M tokens</td>\
                     <td>$10.00 / 1M tokens</td><td>$0.22 / 1M tokens</td></tr><tr><td>tracked-jump</td>\
                     <td>$4.00 / 1M tokens</td><td>$20.00 / 1M tokens</td><td>$0.40 / 1M tokens</td></tr>{new}</table>"
                ),
            )
        }
        "models" => html(
            "Models",
            "<table><tr><th>Model</th><th>Context</th><th>Max output</th><th>Vision</th></tr>\
             <tr><td>tracked-steady</td><td>128K</td><td>16384</td><td>yes</td></tr>\
             <tr><td>tracked-jump</td><td>200K</td><td>32768</td><td>no</td></tr></table>",
        ),
        "news" => html("News", "<p>The mock was released.</p>"),
        "private/pricing" => html("Private pricing", "<p>tracked-steady $0.01 / 1M tokens</p>"),
        _ => return None,
    })
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn empty_response(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

/// An error of the mock itself (misuse of its options or control endpoints), as opposed to an
/// emulated provider error.
fn mock_error(status: StatusCode, message: &str) -> Response {
    json_response(status, &json!({ "error": { "type": "mock_error", "message": format!("mock: {message}") } }))
}
