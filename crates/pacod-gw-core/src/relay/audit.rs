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

use crate::model::{AuditAttachments, AuditGuarantee, AuditMode, AuditRule, AuditSwitch};
use crate::snapshot::{TemplateRt, WorkspaceRt};

/// Headers whose value is a credential. They are recorded by name only.
const SECRET_HEADERS: &[&str] =
    &["authorization", "proxy-authorization", "x-api-key", "x-goog-api-key", "cookie", "set-cookie"];

/// What was decided for a call that is recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditPlan {
    pub retention_days: u32,
    pub attachments: AuditAttachments,
    pub guarantee: AuditGuarantee,
    /// The most one body may hold.
    pub max_body_bytes: usize,
}

/// Decides whether a call is recorded. `asked` is what the request said (`gateway.audit` or
/// `x-gw-audit`); `draw` is a number in `[0, 1)` for sampling.
pub fn plan(
    ws: &WorkspaceRt,
    template: Option<&TemplateRt>,
    asked: Option<bool>,
    now: DateTime<Utc>,
    draw: f64,
) -> Option<AuditPlan> {
    let s = &ws.settings;
    let a = &s.audit;
    let on = match s.audit_rule {
        // A higher level has decided; nothing below it changes that.
        Some(AuditRule::Forbid) => return None,
        Some(AuditRule::Force) => true,
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

/// A body as it accumulates, up to the size limit.
#[derive(Debug, Default)]
struct Buffer {
    bytes: BytesMut,
    /// The size of the whole body, including what was not kept.
    total: u64,
    /// For streams: when each chunk passed, in microseconds since the call began, and its size.
    chunks: Vec<(u64, u32)>,
}

impl Buffer {
    fn push(&mut self, chunk: &[u8], at_us: u64, limit: usize, stream: bool) {
        self.total += chunk.len() as u64;
        let room = limit.saturating_sub(self.bytes.len());
        self.bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
        // The timing of the first few thousand chunks is enough to see a stream's rhythm.
        if stream && self.chunks.len() < 10_000 {
            self.chunks.push((at_us, chunk.len() as u32));
        }
    }
}

/// One provider attempt.
#[derive(Debug)]
struct Attempt {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Bytes,
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
    attempts: Vec<Attempt>,
    upstream: Buffer,
    /// What the client was sent, when that is not the provider's bytes.
    client: Option<Buffer>,
    client_status: u16,
    client_headers: Vec<(String, String)>,
    stream: bool,
    /// How the call was resolved; filled by the handler.
    pub resolution: Value,
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

/// Replaces long base64 payloads in a JSON body by their size and hash.
fn reference_attachments(body: &[u8]) -> Option<Vec<u8>> {
    fn walk(v: &mut Value, changed: &mut bool) {
        match v {
            Value::String(s) if s.len() >= 2048 => {
                let payload = s.split_once(";base64,").map_or(s.as_str(), |(_, d)| d);
                let looks_encoded =
                    payload.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'));
                if looks_encoded {
                    let hash = hex::encode(crate::crypto::sha256(s.as_bytes()));
                    *s = format!("[attachment: {} bytes, sha256 {hash}]", s.len());
                    *changed = true;
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|i| walk(i, changed)),
            Value::Object(map) => map.values_mut().for_each(|i| walk(i, changed)),
            _ => {}
        }
    }
    let mut v: Value = serde_json::from_slice(body).ok()?;
    let mut changed = false;
    walk(&mut v, &mut changed);
    changed.then(|| serde_json::to_vec(&v).unwrap_or_default())
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
            id: format!("aud_{}", uuid::Uuid::now_v7().simple()),
            start,
            client_request: AuditPart {
                method: method.to_owned(),
                url: path_and_query.to_owned(),
                headers: headers_of(headers, Some(key_id)),
                ..Default::default()
            },
            client_body: body.cloned().unwrap_or_default(),
            attempts: Vec::new(),
            upstream: Buffer::default(),
            client: None,
            client_status: 0,
            client_headers: Vec::new(),
            stream: false,
            resolution: Value::Null,
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
        let referenced =
            (self.plan.attachments == AuditAttachments::Reference).then(|| reference_attachments(body)).flatten();
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
        let client_body = cut(&self.client_body);
        let client_request = self.part(self.client_request.clone(), &client_body, self.client_body.len() as u64, &[]);

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
            provider_requests.push(self.part(
                head,
                &body,
                a.body.len() as u64,
                &[("client_request", &client_body[..])],
            ));
            let head = AuditPart { status: a.status, headers: a.response_headers.clone(), ..Default::default() };
            if i == last {
                let mut part = self.part(head, &self.upstream.bytes, self.upstream.total, &[]);
                part.chunks = self.upstream.chunks.clone();
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
                part
            }
            // The relay sent what it received.
            None if !self.attempts.is_empty() => {
                let name = format!("provider_responses[{last}]");
                let mut part =
                    self.part(head, &self.upstream.bytes, self.upstream.total, &[(&name, &self.upstream.bytes[..])]);
                if self.upstream.bytes.is_empty() {
                    part.body_encoding = "utf8".into();
                }
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
        });
        plan(&snap.workspaces["ws_1"], template.as_ref(), asked, Utc::now(), draw)
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
}
