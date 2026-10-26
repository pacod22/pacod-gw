//! Audit mode (concept section 14.6): the full record of a call.
//!
//! A capture travels with the call's recorder. It holds the client's request, every provider
//! attempt, the provider's answer and what the client was sent, and is written to the audit
//! spool when the call settles. Nothing on the request path waits for it; what it costs is
//! memory, because the bodies are held until then.
//!
//! Only credentials are left out. The gateway key is replaced by its id, and provider keys
//! and authorization headers are removed in both directions.

use std::time::Instant;

use axum::http::HeaderMap;
use base64::Engine;
use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Utc};
use pacod_gw_events::audit::{AuditPart, AuditRecord};
use serde_json::{Value, json};

use crate::model::{AuditAttachments, AuditGuarantee, AuditMode, AuditRule, AuditSwitch, AuditUnrecordable};
use crate::snapshot::{TemplateRt, WorkspaceRt};

/// Headers whose value is a credential. They are recorded by name only.
const SECRET_HEADERS: &[&str] =
    &["authorization", "proxy-authorization", "x-api-key", "x-goog-api-key", "cookie", "set-cookie"];

/// What was decided for a call that is recorded. A batch keeps it with its record until its
/// items are settled.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AuditPlan {
    pub retention_days: u32,
    pub attachments: AuditAttachments,
    pub guarantee: AuditGuarantee,
    /// The most one body may hold.
    pub max_body_bytes: usize,
}

/// Decides whether a call is recorded. `asked` is what the request said (`gateway.audit` or
/// `x-gw-audit`); `flagged` says a plugin's rule marked the call for review; `draw` is a
/// number in `[0, 1)` for sampling.
pub fn plan(
    ws: &WorkspaceRt,
    template: Option<&TemplateRt>,
    asked: Option<bool>,
    flagged: bool,
    now: DateTime<Utc>,
    draw: f64,
) -> Option<AuditPlan> {
    let s = &ws.settings;
    let a = &s.audit;
    let on = match s.audit_rule {
        // A higher level has decided; nothing below it changes that.
        Some(AuditRule::Forbid) => return None,
        Some(AuditRule::Force) => true,
        // A flagged call is recorded whatever the workspace's switch and sampling say.
        None if flagged => {
            return Some(AuditPlan {
                retention_days: a.retention_days.unwrap_or(14),
                attachments: a.attachments,
                guarantee: a.guarantee,
                max_body_bytes: s.audit_max_body_bytes as usize,
            });
        }
        None => {
            // The workspace's own switch ends at `until`.
            let mode = if a.until.is_some_and(|t| now >= t) { AuditMode::Off } else { a.mode };
            match template.map_or(AuditSwitch::Inherit, |t| t.spec.audit) {
                AuditSwitch::On => true,
                AuditSwitch::Off => false,
                AuditSwitch::Inherit => match mode {
                    AuditMode::On => true,
                    AuditMode::OnRequest => asked == Some(true),
                    AuditMode::Off => false,
                },
            }
        }
    };
    if !on || draw >= a.sample.unwrap_or(1.0) {
        return None;
    }
    Some(AuditPlan {
        retention_days: a.retention_days.unwrap_or(14),
        attachments: a.attachments,
        guarantee: a.guarantee,
        max_body_bytes: s.audit_max_body_bytes as usize,
    })
}

/// What audit mode cannot record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unrecordable {
    /// A WebSocket session relayed in proxy mode, or a Realtime session an ephemeral key or WebRTC
    /// opens at the provider.
    Session,
    /// A batch made in proxy mode: the provider runs its items unseen.
    Batch,
}

/// Whether a call audit mode cannot record is refused. The workspace must record what it is
/// (Realtime sessions, or files and batches) and refuse what it cannot, and audit mode must
/// apply to the call. Sampling is left out: a refusal by chance would only make the caller retry.
pub fn refuses_unrecordable(
    ws: &WorkspaceRt,
    template: Option<&TemplateRt>,
    asked: Option<bool>,
    now: DateTime<Utc>,
    what: Unrecordable,
) -> bool {
    let a = &ws.settings.audit;
    let recorded = match what {
        Unrecordable::Session => a.realtime,
        Unrecordable::Batch => a.objects,
    };
    recorded && a.unrecordable == AuditUnrecordable::Refuse && plan(ws, template, asked, false, now, 0.0).is_some()
}

/// A body as it accumulates, up to the size limit.
#[derive(Debug, Default, Clone)]
struct Buffer {
    bytes: BytesMut,
    /// The size of the whole body, including what was not kept.
    total: u64,
    /// For streams: when each chunk passed, in microseconds since the call began, and its size.
    chunks: Vec<(u64, u32)>,
    /// Something was not kept.
    truncated: bool,
}

/// The timing of the first few thousand chunks is enough to see a stream's rhythm.
const MAX_CHUNKS: usize = 10_000;

impl Buffer {
    fn push(&mut self, chunk: &[u8], at_us: u64, limit: usize, stream: bool) {
        self.total += chunk.len() as u64;
        let room = limit.saturating_sub(self.bytes.len());
        self.truncated |= chunk.len() > room;
        self.bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if stream && self.chunks.len() < MAX_CHUNKS {
            self.chunks.push((at_us, chunk.len() as u32));
        }
    }

    /// A session's frame as one line. `line` is the frame as it is kept (its attachments
    /// referenced) and `original` the size of the frame as it passed. The chunk gives the
    /// line's own length, so that the frames can be told apart even where one holds a newline.
    fn push_line(&mut self, line: &[u8], original: usize, at_us: u64, limit: usize) {
        self.total += original as u64 + 1;
        let room = limit.saturating_sub(self.bytes.len());
        self.truncated |= line.len() + 1 > room;
        self.bytes.extend_from_slice(&line[..line.len().min(room)]);
        if room > line.len() {
            self.bytes.extend_from_slice(b"\n");
        }
        if self.chunks.len() < MAX_CHUNKS {
            self.chunks.push((at_us, (line.len() + 1) as u32));
        }
    }

    /// The body is gone; what is left is how it came.
    fn timing(&mut self) -> Timed {
        Timed { total: self.total, chunks: std::mem::take(&mut self.chunks), truncated: self.truncated }
    }
}

/// How a request body that arrived frame by frame came: its whole size, when each frame passed,
/// and whether anything was not kept.
#[derive(Debug, Default, Clone)]
struct Timed {
    total: u64,
    chunks: Vec<(u64, u32)>,
    truncated: bool,
}

/// One provider attempt.
#[derive(Debug, Clone)]
struct Attempt {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Bytes,
    /// A session's frames: the body's whole size and the time of each frame.
    timed: Option<Timed>,
    status: u16,
    response_headers: Vec<(String, String)>,
}

/// The record of a call as it is being made.
pub struct AuditCapture {
    pub plan: AuditPlan,
    pub id: String,
    start: Instant,
    client_request: AuditPart,
    client_body: Bytes,
    /// A session's frames: the client body's whole size and the time of each frame.
    client_timed: Option<Timed>,
    attempts: Vec<Attempt>,
    upstream: Buffer,
    /// What the client was sent, when that is not the provider's bytes.
    client: Option<Buffer>,
    client_status: u16,
    client_headers: Vec<(String, String)>,
    stream: bool,
    /// The bodies are a session's frames, their attachments already referenced frame by frame.
    framed: bool,
    /// The provider's answer is a file's content kept as its size and hash: the hash so far.
    upstream_digest: Option<ring::digest::Context>,
    /// How the call was resolved; filled by the handler.
    pub resolution: Value,
}

fn new_id() -> String {
    format!("aud_{}", uuid::Uuid::now_v7().simple())
}

/// Headers in order, with credentials named but not shown.
fn headers_of(headers: &HeaderMap, key_id: Option<&str>) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            let n = name.as_str();
            let v = if SECRET_HEADERS.contains(&n) {
                match key_id {
                    // The gateway key is replaced by its id.
                    Some(id) if matches!(n, "authorization" | "x-api-key" | "x-goog-api-key") => {
                        format!("[gateway key {id}]")
                    }
                    _ => "[removed]".to_owned(),
                }
            } else {
                String::from_utf8_lossy(value.as_bytes()).into_owned()
            };
            (n.to_owned(), v)
        })
        .collect()
}

/// What stands in a record in place of an attachment.
fn marker(s: &str) -> String {
    let hash = hex::encode(crate::crypto::sha256(s.as_bytes()));
    format!("[attachment: {} bytes, sha256 {hash}]", s.len())
}

/// Replaces long base64 payloads by their size and hash, and, with `audio`, every string under
/// a member `audio` (a Realtime event's audio).
fn reference_values(v: &mut Value, audio: bool, changed: &mut bool) {
    match v {
        Value::String(s) if s.len() >= 2048 => {
            let payload = s.split_once(";base64,").map_or(s.as_str(), |(_, d)| d);
            let looks_encoded =
                payload.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'));
            if looks_encoded {
                *s = marker(s);
                *changed = true;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| reference_values(i, audio, changed)),
        Value::Object(map) => {
            for (name, item) in map.iter_mut() {
                match item {
                    Value::String(s) if audio && name == "audio" && !s.is_empty() => {
                        *s = marker(s);
                        *changed = true;
                    }
                    _ => reference_values(item, audio, changed),
                }
            }
        }
        _ => {}
    }
}

/// Replaces long base64 payloads in a JSON body by their size and hash.
fn reference_attachments(body: &[u8]) -> Option<Vec<u8>> {
    let mut v: Value = serde_json::from_slice(body).ok()?;
    let mut changed = false;
    reference_values(&mut v, false, &mut changed);
    changed.then(|| serde_json::to_vec(&v).unwrap_or_default())
}

/// A Realtime event with its audio, and any other long base64 payload, replaced by size and
/// hash. Audio goes whatever its length: a browser sends it in frames too short for the
/// payload rule.
fn reference_event(text: &str) -> Option<String> {
    let mut v: Value = serde_json::from_str(text).ok()?;
    let mut changed = false;
    let audio_delta =
        matches!(v.get("type").and_then(Value::as_str), Some("response.output_audio.delta" | "response.audio.delta"));
    if audio_delta && let Some(Value::String(s)) = v.get_mut("delta") {
        *s = marker(s);
        changed = true;
    }
    reference_values(&mut v, true, &mut changed);
    changed.then(|| v.to_string())
}

/// What the record keeps of a form: its text parts, and of each file its name, type, size and
/// hash, not its bytes.
pub fn form_summary(form: &pacod_gw_dialects::multipart::Form, bytes: &[u8]) -> Bytes {
    let parts: Vec<Value> = form
        .parts
        .iter()
        .map(|p| {
            let body = &bytes[p.body.clone()];
            match (&p.filename, std::str::from_utf8(body)) {
                (None, Ok(text)) if body.len() <= 64 << 10 => json!({ "name": p.name, "value": text }),
                _ => json!({
                    "name": p.name, "filename": p.filename, "content_type": p.content_type, "bytes": body.len(),
                    "sha256": hex::encode(crate::crypto::sha256(body)),
                }),
            }
        })
        .collect();
    Bytes::from(json!({ "form": parts }).to_string())
}

/// What stands in a record for a body kept as its size and hash.
fn digest_marker(total: u64, digest: &[u8]) -> String {
    format!("[attachment: {total} bytes, sha256 {}]", hex::encode(digest))
}

/// A request's form type, when it sends a form.
fn form_type(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|ct| ct.trim_start().get(..19).is_some_and(|k| k.eq_ignore_ascii_case("multipart/form-data")))
}

/// A request body on its way to the provider, kept for the record as it passes: up to the size
/// limit, with its whole size and hash.
#[derive(Clone)]
pub struct RequestTap(std::sync::Arc<std::sync::Mutex<Tapped>>);

struct Tapped {
    bytes: BytesMut,
    total: u64,
    limit: usize,
    hash: ring::digest::Context,
}

impl RequestTap {
    pub fn new(limit: usize) -> Self {
        let hash = ring::digest::Context::new(&ring::digest::SHA256);
        Self(std::sync::Arc::new(std::sync::Mutex::new(Tapped { bytes: BytesMut::new(), total: 0, limit, hash })))
    }

    /// Passes `body` on, keeping what it carries.
    pub fn wrap(&self, body: axum::body::Body) -> axum::body::Body {
        use http_body_util::BodyExt;
        let tap = self.clone();
        axum::body::Body::new(body.map_frame(move |frame| {
            if let Some(data) = frame.data_ref() {
                tap.push(data);
            }
            frame
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Tapped> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn push(&self, chunk: &[u8]) {
        let mut t = self.lock();
        t.total += chunk.len() as u64;
        t.hash.update(chunk);
        let room = t.limit.saturating_sub(t.bytes.len());
        t.bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }

    /// What passed: the bytes kept, the whole size and its hash.
    fn taken(&self) -> (Bytes, u64, Vec<u8>) {
        let mut t = self.lock();
        let digest = t.hash.clone().finish().as_ref().to_vec();
        (std::mem::take(&mut t.bytes).freeze(), t.total, digest)
    }
}

impl AuditCapture {
    /// Starts a record with the client's request as received.
    #[allow(clippy::too_many_arguments)]
    pub fn begin(
        plan: AuditPlan,
        start: Instant,
        method: &str,
        path_and_query: &str,
        headers: &HeaderMap,
        key_id: &str,
        body: Option<&Bytes>,
    ) -> Self {
        Self {
            plan,
            id: new_id(),
            start,
            client_request: AuditPart {
                method: method.to_owned(),
                url: path_and_query.to_owned(),
                headers: headers_of(headers, Some(key_id)),
                ..Default::default()
            },
            client_body: body.cloned().unwrap_or_default(),
            client_timed: None,
            attempts: Vec::new(),
            upstream: Buffer::default(),
            client: None,
            client_status: 0,
            client_headers: Vec::new(),
            stream: false,
            framed: false,
            upstream_digest: None,
            resolution: Value::Null,
        }
    }

    /// A request body as the record keeps it: where attachments are kept by reference, a form's
    /// text parts and, of each file, what it is; else the body as it is.
    fn kept(&self, body: &Bytes, headers: &HeaderMap) -> (Bytes, Option<Timed>) {
        let reference = self.plan.attachments == AuditAttachments::Reference;
        match form_type(headers).filter(|_| reference) {
            Some(ct) => {
                let total = body.len() as u64;
                let summary = match pacod_gw_dialects::multipart::Form::parse(ct, body) {
                    Ok(form) => form_summary(&form, body),
                    Err(_) => Bytes::from(digest_marker(total, &crate::crypto::sha256(body))),
                };
                // The summary is not the body cut short: its size is the body's.
                (summary, Some(Timed { total, chunks: Vec::new(), truncated: false }))
            }
            None => (body.clone(), None),
        }
    }

    /// The client's body, read whole after the record began.
    pub fn request_body(&mut self, body: &Bytes, headers: &HeaderMap) {
        let (kept, timed) = self.kept(body, headers);
        self.client_body = kept;
        self.client_timed = timed;
    }

    /// A provider attempt whose body is kept as [`AuditCapture::request_body`] keeps one.
    pub fn attempt_with(&mut self, method: &str, url: &str, headers: &HeaderMap, body: &Bytes) {
        let (kept, timed) = self.kept(body, headers);
        self.attempt(method, url, headers, Some(&kept));
        if let Some(a) = self.attempts.last_mut() {
            a.timed = timed;
        }
    }

    /// The client's body as it streamed through `tap` to the provider, which got the same
    /// bytes: kept once.
    pub fn tapped_request(&mut self, tap: &RequestTap, headers: &HeaderMap) {
        let (bytes, total, digest) = tap.taken();
        let truncated = total > bytes.len() as u64;
        let reference = self.plan.attachments == AuditAttachments::Reference && form_type(headers).is_some();
        let (kept, timed) = match (reference, truncated) {
            // A form too large to be read for its parts is kept as its size and hash.
            (true, true) => (
                Bytes::from(digest_marker(total, &digest)),
                Some(Timed { total, chunks: Vec::new(), truncated: false }),
            ),
            (true, false) => self.kept(&bytes, headers),
            (false, _) => (bytes, Some(Timed { total, chunks: Vec::new(), truncated })),
        };
        self.client_body = kept.clone();
        self.client_timed = timed.clone();
        if let Some(a) = self.attempts.last_mut() {
            a.body = kept;
            a.timed = timed;
        }
    }

    /// The provider's answer is a file's content: where attachments are kept by reference, only
    /// its size and hash are kept.
    pub fn digest_response(&mut self) {
        if self.plan.attachments == AuditAttachments::Reference {
            self.upstream_digest = Some(ring::digest::Context::new(&ring::digest::SHA256));
        }
    }

    fn now_us(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }

    /// A provider attempt begins: what is sent, after the template, the merge and any translation.
    pub fn attempt(&mut self, method: &str, url: &str, headers: &HeaderMap, body: Option<&Bytes>) {
        self.attempts.push(Attempt {
            url: url.to_owned(),
            method: method.to_owned(),
            headers: headers_of(headers, None),
            body: body.cloned().unwrap_or_default(),
            timed: None,
            status: 0,
            response_headers: Vec::new(),
        });
    }

    /// The provider answered the current attempt.
    pub fn attempt_head(&mut self, status: u16, headers: &HeaderMap) {
        if let Some(a) = self.attempts.last_mut() {
            a.status = status;
            a.response_headers = headers_of(headers, None);
        }
    }

    /// The head of the answer to the client.
    pub fn client_head(&mut self, status: u16, headers: &HeaderMap, stream: bool) {
        self.client_status = status;
        self.client_headers = headers_of(headers, None);
        self.stream = stream;
    }

    /// A piece of the provider's answer arrived.
    pub fn upstream_chunk(&mut self, chunk: &[u8]) {
        if let Some(d) = self.upstream_digest.as_mut() {
            d.update(chunk);
            self.upstream.total += chunk.len() as u64;
            return;
        }
        let (at, limit, stream) = (self.now_us(), self.plan.max_body_bytes, self.stream);
        self.upstream.push(chunk, at, limit, stream);
    }

    /// A piece was sent to the client that is not the provider's bytes (a shaped or
    /// translated answer). Without any such call the client is recorded as having received
    /// what the provider sent.
    pub fn client_chunk(&mut self, chunk: &[u8]) {
        let (at, limit, stream) = (self.now_us(), self.plan.max_body_bytes, self.stream);
        self.client.get_or_insert_with(Buffer::default).push(chunk, at, limit, stream);
    }

    /// A body as a part: text where it is text, a reference where it equals an earlier part.
    fn part(&self, mut part: AuditPart, body: &[u8], total: u64, earlier: &[(&str, &[u8])]) -> AuditPart {
        part.body_bytes = total;
        part.truncated = (body.len() as u64) < total;
        if let Some((name, _)) = earlier.iter().find(|(_, b)| !body.is_empty() && *b == body) {
            part.body_encoding = "ref".into();
            part.same_as = (*name).to_owned();
            return part;
        }
        let referenced = (!self.framed && self.plan.attachments == AuditAttachments::Reference)
            .then(|| reference_attachments(body))
            .flatten();
        let body = referenced.as_deref().unwrap_or(body);
        match std::str::from_utf8(body) {
            Ok(text) => {
                part.body = text.to_owned();
                part.body_encoding = "utf8".into();
            }
            Err(_) => {
                part.body = base64::engine::general_purpose::STANDARD.encode(body);
                part.body_encoding = "base64".into();
            }
        }
        part
    }

    /// Finishes the record. `outcome` and `timeline` come from the settled call.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(self, event: &pacod_gw_events::UsageEvent, timeline: Value, outcome: Value) -> AuditRecord {
        let limit = self.plan.max_body_bytes;
        let cut = |b: &Bytes| b.slice(..b.len().min(limit));
        // A session's frames say themselves how they came; a body taken whole is cut here.
        let timed = |part: &mut AuditPart, t: &Option<Timed>| {
            if let Some(t) = t {
                part.truncated = t.truncated;
                if part.body_encoding != "ref" {
                    part.chunks = t.chunks.clone();
                }
            }
        };
        let client_body = cut(&self.client_body);
        let client_total = self.client_timed.as_ref().map_or(self.client_body.len() as u64, |t| t.total);
        let mut client_request = self.part(self.client_request.clone(), &client_body, client_total, &[]);
        timed(&mut client_request, &self.client_timed);
        // A file's content kept as its size and hash stands in for its bytes, both ways.
        let digested =
            self.upstream_digest.as_ref().map(|d| digest_marker(self.upstream.total, d.clone().finish().as_ref()));
        let answer: &[u8] = digested.as_deref().map_or(&self.upstream.bytes[..], str::as_bytes);
        let answer_truncated = digested.is_none() && self.upstream.truncated;

        let mut provider_requests = Vec::with_capacity(self.attempts.len());
        let mut provider_responses = Vec::with_capacity(self.attempts.len());
        let last = self.attempts.len().saturating_sub(1);
        for (i, a) in self.attempts.iter().enumerate() {
            let body = cut(&a.body);
            let head = AuditPart {
                method: a.method.clone(),
                url: a.url.clone(),
                headers: a.headers.clone(),
                ..Default::default()
            };
            // Without a template, proxy mode sends what it received: stored once.
            let total = a.timed.as_ref().map_or(a.body.len() as u64, |t| t.total);
            let mut part = self.part(head, &body, total, &[("client_request", &client_body[..])]);
            timed(&mut part, &a.timed);
            provider_requests.push(part);
            let head = AuditPart { status: a.status, headers: a.response_headers.clone(), ..Default::default() };
            if i == last {
                let mut part = self.part(head, answer, self.upstream.total, &[]);
                part.chunks = self.upstream.chunks.clone();
                part.truncated = answer_truncated;
                provider_responses.push(part);
            } else {
                // An attempt that was retried: its answer was not read.
                provider_responses.push(head);
            }
        }

        let head = AuditPart { status: self.client_status, headers: self.client_headers.clone(), ..Default::default() };
        let client_response = match &self.client {
            Some(sent) => {
                let mut part = self.part(head, &sent.bytes, sent.total, &[]);
                part.chunks = sent.chunks.clone();
                part.truncated = sent.truncated;
                part
            }
            // The relay sent what it received.
            None if !self.attempts.is_empty() => {
                let name = format!("provider_responses[{last}]");
                let mut part = self.part(head, answer, self.upstream.total, &[(&name, answer)]);
                if answer.is_empty() {
                    part.body_encoding = "utf8".into();
                }
                part.truncated = answer_truncated;
                part
            }
            None => head,
        };

        let truncated = client_request.truncated
            || client_response.truncated
            || provider_requests.iter().chain(&provider_responses).any(|p| p.truncated);
        AuditRecord {
            audit_id: self.id.clone(),
            event_id: event.id,
            timestamp: event.timestamp,
            expires_at: event.timestamp + chrono::Duration::days(i64::from(self.plan.retention_days)),
            organisation_id: event.organisation_id.clone(),
            workspace_id: event.workspace_id.clone(),
            key_id: event.key_id.clone(),
            end_user: event.end_user.clone(),
            session_id: event.session_id.clone(),
            trace_id: event.trace_id.clone(),
            agent_id: event.agent_id.clone(),
            user_id: event.user_id.clone(),
            task: event.task.clone(),
            complexity: event.complexity.clone(),
            template: event.template.clone(),
            model: event.model_served.clone(),
            mode: event.mode.clone(),
            client_request,
            provider_requests,
            provider_responses,
            client_response,
            resolution: self.resolution,
            timeline,
            outcome,
            truncated,
        }
    }
}

// --- Batch items ------------------------------------------------------------------------------

/// A batch item's body as its record keeps it: attachments referenced where the workspace says
/// so, cut at the size limit. Returns it, its size before that, and whether it was cut.
pub fn kept_item(plan: &AuditPlan, body: &[u8]) -> (Vec<u8>, i64, bool) {
    let referenced = (plan.attachments == AuditAttachments::Reference).then(|| reference_attachments(body)).flatten();
    let mut kept = referenced.unwrap_or_else(|| body.to_vec());
    let truncated = kept.len() > plan.max_body_bytes;
    kept.truncate(plan.max_body_bytes);
    (kept, body.len() as i64, truncated)
}

/// The record of one batch item: its request as kept when its batch was made (`None` when it is
/// no longer kept), and its result line as the provider wrote it, which the caller reads as it
/// is. `url` is the item's own endpoint, `provider_url` the provider's batch endpoint. The
/// record's id is the item's event's, so that an item settled twice is stored once.
#[allow(clippy::too_many_arguments)]
pub fn item_record(
    plan: AuditPlan,
    event_id: uuid::Uuid,
    url: &str,
    provider_url: &str,
    request: Option<&crate::store::objects::PendingItem>,
    status: u16,
    line: &[u8],
    resolution: Value,
) -> AuditCapture {
    let none = HeaderMap::new();
    let mut c = AuditCapture::begin(plan, Instant::now(), "POST", url, &none, "", None);
    c.id = format!("aud_{}", event_id.simple());
    // The bodies were referenced when they were kept; the result line is referenced here.
    c.framed = true;
    let timed = |bytes: i64, truncated: bool| Timed { total: bytes.max(0) as u64, chunks: Vec::new(), truncated };
    let sent = request.map(|r| Bytes::from(r.sent.clone()));
    if let Some(r) = request {
        c.client_body = Bytes::from(r.received.clone());
        c.client_timed = Some(timed(r.received_bytes, r.received_truncated));
    }
    c.attempt("POST", provider_url, &none, sent.as_ref());
    if let (Some(a), Some(r)) = (c.attempts.last_mut(), request) {
        a.timed = Some(timed(r.sent_bytes, r.sent_truncated));
    }
    c.attempt_head(status, &none);
    c.client_head(status, &none, false);
    let (kept, total, truncated) = kept_item(&c.plan, line);
    c.upstream.bytes.extend_from_slice(&kept);
    c.upstream.total = total as u64;
    c.upstream.truncated = truncated;
    c.resolution = resolution;
    c
}

// --- Realtime sessions ------------------------------------------------------------------------

/// Which way a frame of a session went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// From the caller, passed on to the provider.
    CallerPassed,
    /// From the caller, refused: the provider never saw it.
    CallerRefused,
    /// From the provider, passed on to the caller.
    ProviderPassed,
    /// From the provider after the caller left: not passed on.
    ProviderKept,
    /// The gateway's own, to the caller: an `error` event.
    ToCaller,
    /// The gateway's own, to the provider: `response.cancel`.
    ToProvider,
}

/// A frame of a session, as the relay saw it.
#[derive(Debug, Clone, Copy)]
pub enum Frame<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
}

/// A frame as one line of a record, and the frame's own size. A binary frame is kept as its
/// size and hash: OpenAI's Realtime API sends none.
fn line_of(frame: Frame<'_>, reference: bool) -> (std::borrow::Cow<'_, [u8]>, usize) {
    match frame {
        Frame::Text(t) => match reference.then(|| reference_event(t)).flatten() {
            Some(kept) => (kept.into_bytes().into(), t.len()),
            None => (t.as_bytes().into(), t.len()),
        },
        Frame::Binary(b) => {
            let hash = hex::encode(crate::crypto::sha256(b));
            (format!("[binary frame: {} bytes, sha256 {hash}]", b.len()).into_bytes().into(), b.len())
        }
    }
}

/// The record of a Realtime session as it goes. The session's heads (its handshakes and how it
/// was resolved) are kept once; its frames since the last cut are held as the four views, one
/// line per frame. Each cut is the record of one response, so a session holds at most one
/// response's frames.
///
/// What the gateway sent is held apart only once it differs from what it received (a refused
/// event, the gateway's own error or cancel): until then the two are the same bytes, kept once.
pub struct SessionCapture {
    heads: AuditCapture,
    from_caller: Buffer,
    to_provider: Option<Buffer>,
    from_provider: Buffer,
    to_caller: Option<Buffer>,
    frames: usize,
    cuts: u32,
}

impl SessionCapture {
    /// Takes over the capture of the session's handshakes.
    pub fn new(heads: AuditCapture) -> Self {
        Self {
            heads,
            from_caller: Buffer::default(),
            to_provider: None,
            from_provider: Buffer::default(),
            to_caller: None,
            frames: 0,
            cuts: 0,
        }
    }

    /// How the session was resolved, for the handler to complete once it has opened.
    pub fn resolution(&mut self) -> &mut Value {
        &mut self.heads.resolution
    }

    /// Whether a call is refused when its record cannot be stored.
    pub fn required(&self) -> bool {
        self.heads.plan.guarantee == AuditGuarantee::Required
    }

    /// Frames have passed since the last cut.
    pub fn pending(&self) -> bool {
        self.frames > 0
    }

    pub fn frame(&mut self, way: Way, frame: Frame<'_>) {
        let reference = self.heads.plan.attachments == AuditAttachments::Reference;
        let (line, original) = line_of(frame, reference);
        let (at, limit) = (self.heads.now_us(), self.heads.plan.max_body_bytes);
        let push = |b: &mut Buffer| b.push_line(&line, original, at, limit);
        let diverge = |sent: &mut Option<Buffer>, received: &Buffer| {
            if sent.is_none() {
                *sent = Some(received.clone());
            }
        };
        match way {
            Way::CallerPassed => {
                push(&mut self.from_caller);
                if let Some(sent) = self.to_provider.as_mut() {
                    push(sent);
                }
            }
            Way::CallerRefused => {
                diverge(&mut self.to_provider, &self.from_caller);
                push(&mut self.from_caller);
            }
            Way::ProviderPassed => {
                push(&mut self.from_provider);
                if let Some(sent) = self.to_caller.as_mut() {
                    push(sent);
                }
            }
            Way::ProviderKept => {
                diverge(&mut self.to_caller, &self.from_provider);
                push(&mut self.from_provider);
            }
            Way::ToCaller => {
                diverge(&mut self.to_caller, &self.from_provider);
                if let Some(sent) = self.to_caller.as_mut() {
                    push(sent);
                }
            }
            Way::ToProvider => {
                diverge(&mut self.to_provider, &self.from_caller);
                if let Some(sent) = self.to_provider.as_mut() {
                    push(sent);
                }
            }
        }
        self.frames += 1;
    }

    /// The record of the frames since the last cut, as a call's capture: the caller's frames
    /// are the client request, what the gateway passed on the provider request, and so on.
    pub fn cut(&mut self) -> AuditCapture {
        let h = &self.heads;
        let mut caller = std::mem::take(&mut self.from_caller);
        let sent = self.to_provider.take();
        let client = self.to_caller.take();
        let upstream = std::mem::take(&mut self.from_provider);
        let mut resolution = h.resolution.clone();
        resolution["response"] = json!(self.cuts);
        self.cuts += 1;
        self.frames = 0;

        let client_timed = caller.timing();
        let client_body = caller.bytes.freeze();
        let mut attempts = h.attempts.clone();
        if let Some(a) = attempts.last_mut() {
            match sent {
                Some(mut s) => {
                    a.timed = Some(s.timing());
                    a.body = s.bytes.freeze();
                }
                None => {
                    a.timed = Some(client_timed.clone());
                    a.body = client_body.clone();
                }
            }
        }
        AuditCapture {
            plan: h.plan.clone(),
            id: new_id(),
            start: h.start,
            client_request: h.client_request.clone(),
            client_body,
            client_timed: Some(client_timed),
            attempts,
            upstream,
            client,
            client_status: h.client_status,
            client_headers: h.client_headers.clone(),
            stream: true,
            framed: true,
            upstream_digest: None,
            resolution,
        }
    }
}

/// The outcome of a call, for its audit record.
pub fn outcome_of(e: &pacod_gw_events::UsageEvent) -> Value {
    json!({
        "status": e.status,
        "http_status": e.http_status,
        "error_type": e.error_type,
        "error_message": e.error_message,
        "finish_reason": e.finish_reason,
        "attempts": e.attempts,
        "usage": e.usage,
        "usage_source": e.usage_source,
        "cost": e.cost,
        "currency": e.currency,
        "price_version": e.price_version,
        "price_missing": e.price_missing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::envelope::Kek;
    use crate::snapshot::{Snapshot, build};
    use crate::store::Row;
    use axum::http::HeaderValue;

    fn row(kind: &str, id: &str, parent: Option<&str>, doc: Value) -> Row {
        Row {
            id: id.into(),
            kind: kind.into(),
            parent: parent.map(Into::into),
            name: None,
            doc,
            version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn snapshot(global: Value, org: Value, ws: Value) -> Snapshot {
        let rows = vec![
            row("settings", "settings_global", None, global),
            row("organisation", "org_1", None, json!({ "name": "acme", "settings": org })),
            row("workspace", "ws_1", Some("org_1"), json!({ "name": "prod", "settings": ws })),
        ];
        let s = build(1, rows, &[Kek::new("k", [0u8; 32])]);
        assert!(s.problems.is_empty(), "{:?}", s.problems);
        s
    }

    fn decide(
        global: Value,
        org: Value,
        ws: Value,
        template: Option<AuditSwitch>,
        asked: Option<bool>,
        draw: f64,
    ) -> Option<AuditPlan> {
        let snap = snapshot(global, org, ws);
        let template = template.map(|audit| TemplateRt {
            id: "tpl_1".into(),
            name: "t".into(),
            version: 1,
            spec: crate::model::TemplateSpec { audit, ..Default::default() },
            model_id: "mdl_1".into(),
            pool: Box::default(),
            route: crate::routing::bind(None, &[], "t", &mut Vec::new()),
        });
        plan(&snap.workspaces["ws_1"], template.as_ref(), asked, false, Utc::now(), draw)
    }

    #[test]
    fn a_flagged_call_is_recorded_unless_a_higher_level_forbids_it() {
        let snap = |org: Value, ws: Value| snapshot(json!({}), org, ws);
        let off = snap(json!({}), json!({ "audit": { "mode": "off", "sample": 0.1 } }));
        let p = plan(&off.workspaces["ws_1"], None, None, true, Utc::now(), 0.9);
        assert!(p.is_some(), "flagged: the switch and sampling do not apply");
        let forbid = snap(json!({ "audit": { "rule": "forbid" } }), json!({}));
        assert!(plan(&forbid.workspaces["ws_1"], None, None, true, Utc::now(), 0.0).is_none());
    }

    #[test]
    fn who_decides_whether_a_call_is_recorded() {
        let off = json!({});
        let on = json!({ "audit": { "mode": "on", "retention_days": 30, "guarantee": "required" } });
        let on_request = json!({ "audit": { "mode": "on_request" } });

        // The workspace switch, and its options.
        assert_eq!(
            decide(json!({}), off.clone(), off.clone(), None, Some(true), 0.0),
            None,
            "off by default, whatever a request asks"
        );
        let p = decide(json!({}), off.clone(), on.clone(), None, None, 0.0).unwrap();
        assert_eq!((p.retention_days, p.guarantee, p.max_body_bytes), (30, AuditGuarantee::Required, 8 << 20));
        assert!(decide(json!({}), off.clone(), on_request.clone(), None, Some(true), 0.0).is_some());
        assert!(decide(json!({}), off.clone(), on_request.clone(), None, None, 0.0).is_none());
        assert!(decide(json!({}), off.clone(), on_request, None, Some(false), 0.0).is_none());

        // A template overrides the workspace in both directions.
        assert!(decide(json!({}), off.clone(), off.clone(), Some(AuditSwitch::On), None, 0.0).is_some());
        assert!(decide(json!({}), off.clone(), on.clone(), Some(AuditSwitch::Off), None, 0.0).is_none());
        assert!(decide(json!({}), off.clone(), on.clone(), Some(AuditSwitch::Inherit), None, 0.0).is_some());

        // An end time, and sampling.
        let past = json!({ "audit": { "mode": "on", "until": "2020-01-01T00:00:00Z" } });
        assert!(decide(json!({}), off.clone(), past, None, None, 0.0).is_none(), "the debugging session is over");
        let sampled = json!({ "audit": { "mode": "on", "sample": 0.25 } });
        assert!(decide(json!({}), off.clone(), sampled.clone(), None, None, 0.2).is_some());
        assert!(decide(json!({}), off.clone(), sampled, None, None, 0.3).is_none());

        // A higher level rules it out or makes it mandatory.
        let forbid = json!({ "audit": { "rule": "forbid" } });
        let force = json!({ "audit": { "rule": "force" } });
        assert!(decide(json!({}), forbid, on.clone(), Some(AuditSwitch::On), Some(true), 0.0).is_none());
        assert!(decide(json!({}), force, off.clone(), Some(AuditSwitch::Off), None, 0.0).is_some());
        assert!(
            decide(json!({ "audit_rule": "forbid" }), json!({ "audit": { "rule": "force" } }), on, None, None, 0.0)
                .is_none(),
            "the installation's rule comes first"
        );
        // A workspace without settings of its own follows its organisation's.
        assert!(decide(json!({}), json!({ "audit": { "mode": "on" } }), off, None, None, 0.0).is_some());
    }

    #[test]
    fn which_sessions_that_cannot_be_recorded_are_refused() {
        let refuses = |org: Value, ws: Value, template: Option<AuditSwitch>, asked: Option<bool>| {
            let snap = snapshot(json!({}), org, ws);
            let template = template.map(|audit| TemplateRt {
                id: "tpl_1".into(),
                name: "t".into(),
                version: 1,
                spec: crate::model::TemplateSpec { audit, ..Default::default() },
                model_id: "mdl_1".into(),
                pool: Box::default(),
                route: crate::routing::bind(None, &[], "t", &mut Vec::new()),
            });
            refuses_unrecordable(&snap.workspaces["ws_1"], template.as_ref(), asked, Utc::now(), Unrecordable::Session)
        };
        let ws = |extra: Value| {
            let mut audit = json!({ "mode": "on", "realtime": true, "unrecordable": "refuse" });
            audit.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            json!({ "audit": audit })
        };
        assert!(refuses(json!({}), ws(json!({})), None, None));
        // Allowed unless set otherwise, and only where Realtime sessions are recorded.
        assert!(!refuses(json!({}), json!({ "audit": { "mode": "on", "realtime": true } }), None, None));
        assert!(!refuses(json!({}), ws(json!({ "realtime": false })), None, None));
        assert!(!refuses(json!({}), json!({}), None, None), "nothing is refused by default");
        // Only where audit mode applies, and never by chance.
        assert!(refuses(json!({}), ws(json!({ "sample": 0.1 })), None, None));
        assert!(!refuses(json!({}), ws(json!({ "mode": "on_request" })), None, None));
        assert!(refuses(json!({}), ws(json!({ "mode": "on_request" })), None, Some(true)));
        assert!(!refuses(json!({}), ws(json!({})), Some(AuditSwitch::Off), None));
        assert!(!refuses(json!({}), ws(json!({ "mode": "off" })), None, None));
        // A higher level's rule applies, the workspace's options with it.
        assert!(!refuses(json!({ "audit": { "rule": "forbid" } }), ws(json!({})), None, None));
        assert!(refuses(json!({ "audit": { "rule": "force" } }), ws(json!({ "mode": "off" })), None, None));
    }

    #[test]
    fn each_kind_of_call_that_cannot_be_recorded_is_refused_where_its_own_kind_is_recorded() {
        let refuses = |audit: Value, what| {
            let snap = snapshot(json!({}), json!({}), json!({ "audit": audit }));
            refuses_unrecordable(&snap.workspaces["ws_1"], None, None, Utc::now(), what)
        };
        let realtime = json!({ "mode": "on", "realtime": true, "unrecordable": "refuse" });
        let objects = json!({ "mode": "on", "objects": true, "unrecordable": "refuse" });
        assert!(refuses(realtime.clone(), Unrecordable::Session) && !refuses(realtime, Unrecordable::Batch));
        assert!(refuses(objects.clone(), Unrecordable::Batch) && !refuses(objects, Unrecordable::Session));
        assert!(
            !refuses(json!({ "mode": "on", "objects": true }), Unrecordable::Batch),
            "allowed unless set otherwise"
        );
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        h
    }

    fn a_plan(max: usize, attachments: AuditAttachments) -> AuditPlan {
        AuditPlan { retention_days: 14, attachments, guarantee: AuditGuarantee::BestEffort, max_body_bytes: max }
    }

    fn event() -> pacod_gw_events::UsageEvent {
        let mut e = pacod_gw_events::UsageEvent::new(uuid::Uuid::now_v7(), "i1");
        e.workspace_id = "ws_1".into();
        e.model_served = "anthropic/claude".into();
        e
    }

    #[test]
    fn a_relayed_call_stores_each_body_once_and_no_credential() {
        let body = Bytes::from_static(br#"{"model":"m","messages":[]}"#);
        let mut c = AuditCapture::begin(
            a_plan(1 << 20, AuditAttachments::Full),
            Instant::now(),
            "POST",
            "/proxy/anthropic/v1/messages?beta=true",
            &headers(&[("x-api-key", "pgw_key_secret"), ("anthropic-version", "2023-06-01"), ("cookie", "a=b")]),
            "key_1",
            Some(&body),
        );
        c.attempt(
            "POST",
            "https://api.anthropic.com/v1/messages",
            &headers(&[("x-api-key", "sk-ant-real"), ("content-type", "application/json")]),
            Some(&body),
        );
        c.attempt_head(200, &headers(&[("request-id", "req_1"), ("set-cookie", "s=1")]));
        c.client_head(200, &headers(&[("request-id", "req_1")]), false);
        c.upstream_chunk(br#"{"type":"mess"#);
        c.upstream_chunk(br#"age"}"#);
        let r = c.finish(&event(), json!({}), json!({}));

        let text = serde_json::to_string(&r).unwrap();
        assert!(
            !text.contains("pgw_key_secret")
                && !text.contains("sk-ant-real")
                && !text.contains("a=b")
                && !text.contains("s=1")
        );
        assert_eq!(r.client_request.headers[0], ("x-api-key".into(), "[gateway key key_1]".into()));
        assert_eq!(r.client_request.url, "/proxy/anthropic/v1/messages?beta=true");
        assert_eq!(
            (r.client_request.body.as_str(), r.client_request.body_encoding.as_str()),
            (r#"{"model":"m","messages":[]}"#, "utf8")
        );
        // The provider got the client's bytes: a reference, not a second copy.
        assert_eq!(
            (r.provider_requests[0].body_encoding.as_str(), r.provider_requests[0].same_as.as_str()),
            ("ref", "client_request")
        );
        assert_eq!(r.provider_requests[0].headers[0], ("x-api-key".into(), "[removed]".into()));
        assert_eq!(r.provider_responses[0].body, r#"{"type":"message"}"#);
        assert_eq!((r.client_response.same_as.as_str(), r.client_response.body_bytes), ("provider_responses[0]", 18));
        assert!(r.audit_id.starts_with("aud_") && !r.truncated);
        assert_eq!(r.expires_at, r.timestamp + chrono::Duration::days(14));
    }

    #[test]
    fn retries_streams_translation_and_the_size_limit() {
        let mut c = AuditCapture::begin(
            a_plan(10, AuditAttachments::Full),
            Instant::now(),
            "POST",
            "/v1/chat/completions",
            &HeaderMap::new(),
            "k",
            Some(&Bytes::from_static(b"0123456789abcdef")),
        );
        let sent = Bytes::from_static(b"{\"x\":1}");
        c.attempt("POST", "https://p/v1/messages", &HeaderMap::new(), Some(&sent));
        c.attempt_head(503, &HeaderMap::new());
        c.attempt("POST", "https://p/v1/messages", &HeaderMap::new(), Some(&sent));
        c.attempt_head(200, &HeaderMap::new());
        c.client_head(200, &HeaderMap::new(), true);
        c.upstream_chunk(b"event: a\n\n");
        c.client_chunk(b"data: 1\n\n");
        c.upstream_chunk(b"event: b\n\n");
        c.client_chunk(&[0xff, 0xfe]);
        let r = c.finish(&event(), json!({}), json!({}));

        // A body over the limit is kept up to it and marked, never cut silently.
        assert_eq!(
            (r.client_request.body.as_str(), r.client_request.body_bytes, r.client_request.truncated),
            ("0123456789", 16, true)
        );
        assert!(r.truncated);
        assert_eq!(r.provider_requests.len(), 2);
        assert_eq!(
            (r.provider_responses[0].status, r.provider_responses[0].body.as_str()),
            (503, ""),
            "a retried attempt's answer was not read"
        );
        assert_eq!((r.provider_responses[1].status, r.provider_responses[1].body_bytes), (200, 20));
        assert_eq!(r.provider_responses[1].chunks.len(), 2, "each chunk with its arrival time");
        // What the client was sent differs from what the provider sent, and is not text.
        assert_eq!((r.client_response.body_encoding.as_str(), r.client_response.same_as.as_str()), ("base64", ""));
        assert_eq!(r.client_response.chunks.iter().map(|c| c.1).collect::<Vec<_>>(), [9, 2]);
    }

    #[test]
    fn attachments_can_be_kept_as_size_and_hash() {
        let image = "A".repeat(4000);
        let body = Bytes::from(
            json!({ "messages": [{ "role": "user", "content": [
            { "type": "text", "text": "what is this?" },
            { "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{image}") } }
        ] }] })
            .to_string(),
        );
        let record = |attachments| {
            let c = AuditCapture::begin(
                a_plan(1 << 20, attachments),
                Instant::now(),
                "POST",
                "/v1/chat/completions",
                &HeaderMap::new(),
                "k",
                Some(&body),
            );
            c.finish(&event(), json!({}), json!({})).client_request
        };
        assert!(record(AuditAttachments::Full).body.contains(&image));
        let referenced = record(AuditAttachments::Reference);
        assert!(!referenced.body.contains(&image) && referenced.body.contains("what is this?"));
        assert!(referenced.body.contains("[attachment: 4022 bytes, sha256 "), "{}", referenced.body);
        assert_eq!(referenced.body_bytes, body.len() as u64, "the size is the original's");
    }

    fn form_headers() -> HeaderMap {
        headers(&[("content-type", "multipart/form-data; boundary=b")])
    }

    const FORM: &str = "--b\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\nuser_data\r\n--b\r\n\
        Content-Disposition: form-data; name=\"file\"; filename=\"a.bin\"\r\nContent-Type: image/png\r\n\r\n\
        0123456789\r\n--b--\r\n";

    #[tokio::test]
    async fn a_streamed_upload_is_kept_as_it_passes_up_to_the_limit() {
        use http_body_util::BodyExt;
        let mut c = AuditCapture::begin(
            a_plan(4, AuditAttachments::Full),
            Instant::now(),
            "POST",
            "/v1/files",
            &HeaderMap::new(),
            "k",
            None,
        );
        c.attempt("POST", "https://p/v1/files", &HeaderMap::new(), None);
        let tap = RequestTap::new(4);
        let passed = tap.wrap(axum::body::Body::from("0123456789")).collect().await.unwrap().to_bytes();
        assert_eq!(&passed[..], b"0123456789", "the provider gets every byte");
        c.tapped_request(&tap, &HeaderMap::new());
        let r = c.finish(&event(), json!({}), json!({}));
        assert_eq!(
            (r.client_request.body.as_str(), r.client_request.body_bytes, r.client_request.truncated),
            ("0123", 10, true)
        );
        assert_eq!(r.provider_requests[0].same_as, "client_request", "the same bytes, kept once");
        assert!(r.truncated);
    }

    #[test]
    fn a_form_is_kept_as_what_its_files_are_in_reference_mode() {
        let body = Bytes::from_static(FORM.as_bytes());
        let record = |attachments| {
            let mut c = AuditCapture::begin(
                a_plan(1 << 20, attachments),
                Instant::now(),
                "POST",
                "/v1/files",
                &form_headers(),
                "k",
                None,
            );
            c.request_body(&body, &form_headers());
            c.attempt_with("POST", "https://p/v1/files", &form_headers(), &body);
            c.finish(&event(), json!({}), json!({}))
        };
        let full = record(AuditAttachments::Full);
        assert!(full.client_request.body.contains("0123456789") && !full.truncated);
        let kept = record(AuditAttachments::Reference);
        let summary: Value = serde_json::from_str(&kept.client_request.body).unwrap();
        assert_eq!(summary["form"][0], json!({ "name": "purpose", "value": "user_data" }));
        let sha = hex::encode(crate::crypto::sha256(b"0123456789"));
        assert_eq!(
            summary["form"][1],
            json!({ "name": "file", "filename": "a.bin", "content_type": "image/png", "bytes": 10, "sha256": sha })
        );
        assert_eq!((kept.client_request.body_bytes, kept.client_request.truncated), (FORM.len() as u64, false));
        assert_eq!(kept.provider_requests[0].same_as, "client_request");
    }

    #[test]
    fn a_files_content_is_kept_as_its_size_and_hash_in_reference_mode() {
        let mut c = AuditCapture::begin(
            a_plan(4, AuditAttachments::Reference),
            Instant::now(),
            "GET",
            "/v1/files/f/content",
            &HeaderMap::new(),
            "k",
            None,
        );
        c.attempt("GET", "https://p/v1/files/f/content", &HeaderMap::new(), None);
        c.attempt_head(200, &HeaderMap::new());
        c.digest_response();
        c.upstream_chunk(b"hello ");
        c.upstream_chunk(b"file");
        let r = c.finish(&event(), json!({}), json!({}));
        let sha = hex::encode(crate::crypto::sha256(b"hello file"));
        assert_eq!(r.provider_responses[0].body, format!("[attachment: 10 bytes, sha256 {sha}]"));
        assert_eq!((r.provider_responses[0].body_bytes, r.truncated), (10, false), "longer than the limit, not cut");
        assert_eq!(r.client_response.same_as, "provider_responses[0]");
    }

    #[test]
    fn a_batch_items_request_is_kept_referenced_and_cut_with_its_own_size() {
        let image = "B".repeat(3000);
        let body =
            json!({ "custom_id": "a", "params": { "messages": [{ "content": image }], "note": "x".repeat(50) } })
                .to_string();
        let (kept, size, cut) = kept_item(&a_plan(1 << 20, AuditAttachments::Reference), body.as_bytes());
        let kept = String::from_utf8(kept).unwrap();
        assert!(kept.contains("[attachment: 3000 bytes, sha256 ") && !kept.contains(&image) && !cut, "{kept}");
        assert_eq!(size, body.len() as i64, "the size is the item's own");
        let (kept, _, cut) = kept_item(&a_plan(20, AuditAttachments::Full), body.as_bytes());
        assert_eq!((kept.len(), cut), (20, true));

        // A record without its request says so through its resolution, and keeps the result.
        let r = item_record(
            a_plan(1 << 20, AuditAttachments::Full),
            uuid::Uuid::now_v7(),
            "/v1/messages",
            "https://p/v1/messages/batches",
            None,
            200,
            br#"{"custom_id":"a","result":{"type":"succeeded"}}"#,
            json!({ "request": "no longer kept" }),
        )
        .finish(&event(), Value::Null, Value::Null);
        assert_eq!((r.client_request.body.as_str(), r.provider_requests[0].body.as_str()), ("", ""));
        assert!(
            r.provider_responses[0].body.contains("succeeded") && r.client_response.same_as == "provider_responses[0]"
        );
        assert_eq!(r.resolution["request"], "no longer kept");
    }

    /// A Realtime session's capture after its handshakes.
    fn session(max: usize, attachments: AuditAttachments) -> SessionCapture {
        let mut c = AuditCapture::begin(
            a_plan(max, attachments),
            Instant::now(),
            "GET",
            "/v1/realtime?model=m",
            &headers(&[("authorization", "Bearer pgw_key_secret"), ("sec-websocket-protocol", "realtime")]),
            "key_1",
            None,
        );
        c.attempt("GET", "https://p/v1/realtime?model=m", &headers(&[("authorization", "Bearer sk-real")]), None);
        c.attempt_head(101, &headers(&[("x-request-id", "req_1")]));
        c.client_head(101, &HeaderMap::new(), true);
        c.resolution = json!({ "route": "realtime" });
        SessionCapture::new(c)
    }

    fn record(s: &mut SessionCapture) -> AuditRecord {
        s.cut().finish(&event(), json!({}), json!({}))
    }

    #[test]
    fn a_session_is_recorded_per_response_one_line_per_frame() {
        let mut s = session(1 << 20, AuditAttachments::Full);
        let create = r#"{"type":"response.create"}"#;
        s.frame(Way::CallerPassed, Frame::Text(create));
        s.frame(Way::ProviderPassed, Frame::Text(r#"{"type":"response.created"}"#));
        s.frame(Way::ProviderPassed, Frame::Text(r#"{"type":"response.done"}"#));
        assert!(s.pending());
        let first = record(&mut s);
        assert!(!s.pending(), "a cut starts the next response afresh");
        s.frame(Way::CallerPassed, Frame::Text(r#"{"type":"response.create","n":2}"#));
        let second = record(&mut s);

        assert_ne!(first.audit_id, second.audit_id);
        assert_eq!(first.client_request.body, format!("{create}\n"));
        assert_eq!(first.client_request.chunks.iter().map(|c| c.1).collect::<Vec<_>>(), [create.len() as u32 + 1]);
        assert_eq!(first.provider_responses[0].body, "{\"type\":\"response.created\"}\n{\"type\":\"response.done\"}\n");
        assert_eq!(first.provider_responses[0].chunks.len(), 2, "each frame with its time");
        // What the gateway passed on is what it received: stored once, both ways.
        assert_eq!(
            (first.provider_requests[0].body_encoding.as_str(), first.provider_requests[0].same_as.as_str()),
            ("ref", "client_request")
        );
        assert_eq!(first.client_response.same_as, "provider_responses[0]");
        assert_eq!((&first.resolution["response"], &second.resolution["response"]), (&json!(0), &json!(1)));
        assert_eq!(second.client_request.body, "{\"type\":\"response.create\",\"n\":2}\n");
        assert!(second.provider_responses[0].body.is_empty());

        // Every record has the session's handshakes, without credentials.
        assert_eq!(
            (second.client_request.url.as_str(), second.provider_responses[0].status),
            ("/v1/realtime?model=m", 101)
        );
        let text = serde_json::to_string(&second).unwrap();
        assert!(!text.contains("pgw_key_secret") && !text.contains("sk-real"), "{text}");
    }

    #[test]
    fn what_the_gateway_refused_or_sent_itself_is_told_apart() {
        let mut s = session(1 << 20, AuditAttachments::Full);
        s.frame(Way::CallerPassed, Frame::Text("a"));
        s.frame(Way::CallerRefused, Frame::Text("refused"));
        s.frame(Way::ToCaller, Frame::Text("error"));
        s.frame(Way::ProviderPassed, Frame::Text("p"));
        s.frame(Way::ToProvider, Frame::Text("cancel"));
        s.frame(Way::ProviderKept, Frame::Text("late"));
        s.frame(Way::CallerPassed, Frame::Binary(&[1, 2, 3]));
        let r = record(&mut s);
        let binary = format!("[binary frame: 3 bytes, sha256 {}]", hex::encode(crate::crypto::sha256(&[1, 2, 3])));
        assert_eq!(r.client_request.body, format!("a\nrefused\n{binary}\n"));
        assert_eq!(r.provider_requests[0].body, format!("a\ncancel\n{binary}\n"));
        assert_eq!(r.provider_responses[0].body, "p\nlate\n");
        assert_eq!(r.client_response.body, "error\np\n");
        assert_eq!(r.client_response.chunks.len(), 2);
    }

    #[test]
    fn a_sessions_audio_is_kept_as_size_and_hash_whatever_its_length() {
        let mut s = session(1 << 20, AuditAttachments::Reference);
        let append = json!({ "type": "input_audio_buffer.append", "audio": "AAAA" }).to_string();
        let item = json!({ "type": "conversation.item.create", "item": { "type": "message", "role": "user",
            "content": [{ "type": "input_audio", "audio": "CCCC" }, { "type": "input_text", "text": "hi" }] } })
        .to_string();
        let delta = json!({ "type": "response.output_audio.delta", "delta": "BBBB" }).to_string();
        let text = json!({ "type": "response.output_text.delta", "delta": "hello" }).to_string();
        s.frame(Way::CallerPassed, Frame::Text(&append));
        s.frame(Way::CallerPassed, Frame::Text(&item));
        s.frame(Way::ProviderPassed, Frame::Text(&delta));
        s.frame(Way::ProviderPassed, Frame::Text(&text));
        let r = record(&mut s);
        let caller = &r.client_request.body;
        assert!(!caller.contains("AAAA") && !caller.contains("CCCC") && caller.contains("\"hi\""), "{caller}");
        assert_eq!(caller.matches("[attachment: 4 bytes, sha256 ").count(), 2, "{caller}");
        assert_eq!(r.client_request.body_bytes, (append.len() + item.len() + 2) as u64, "the size is the frames' own");
        let provider: Vec<&str> = r.provider_responses[0].body.lines().collect();
        assert!(provider[0].contains("[attachment: 4 bytes") && !provider[0].contains("BBBB"), "{}", provider[0]);
        assert_eq!(provider[1], text, "text is kept as it came");
        assert!(!r.truncated, "referencing is not truncation");
    }

    #[test]
    fn a_session_record_is_cut_at_the_size_limit_and_says_so() {
        let mut s = session(10, AuditAttachments::Full);
        s.frame(Way::CallerPassed, Frame::Text("0123456789abcdef"));
        let r = record(&mut s);
        assert_eq!(
            (r.client_request.body.as_str(), r.client_request.body_bytes, r.client_request.truncated),
            ("0123456789", 17, true)
        );
        assert!(r.truncated && r.provider_requests[0].truncated);
        // The limit holds per response: the next one starts with room.
        s.frame(Way::CallerPassed, Frame::Text("ok"));
        let r = record(&mut s);
        assert_eq!((r.client_request.body.as_str(), r.truncated), ("ok\n", false));
    }
}
