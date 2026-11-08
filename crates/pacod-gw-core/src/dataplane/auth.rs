//! Authenticates gateway API keys on the data plane (concept section 5.5).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, Uri};
use chrono::Utc;
use parking_lot::Mutex;

use super::Gw;
use super::errors::GwError;
use crate::crypto::keys::API_KEY_PREFIX;
use crate::model::Mode;
use crate::snapshot::{AgentRt, KeyRt, Snapshot, WorkspaceRt};

/// The caller of a data-plane request.
#[derive(Clone)]
pub struct Caller {
    pub snapshot: Arc<Snapshot>,
    pub key: Arc<KeyRt>,
    pub workspace: Arc<WorkspaceRt>,
    /// Set on the gateway's own calls, never from a request: no header or body field reaches
    /// it. Plugins never look at these calls (`docs/plugin-concept.md`, section 3.8).
    pub internal: Option<Internal>,
    /// The registered agent the key speaks for in this call. `key` is then narrowed to what
    /// the agent may use, so that every check of the key holds the agent too.
    pub agent: Option<Arc<AgentRt>>,
}

/// Why the gateway makes a call of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Internal {
    /// A plugin's model call about the call `parent` (its request id).
    Plugin { plugin: String, parent: String },
}

/// Failed authentications allowed per source address per minute.
const MAX_FAILURES_PER_MINUTE: u32 = 60;

/// Slows down key guessing: a source that keeps presenting bad keys is refused for a while.
/// The data plane uses it for gateway keys, the management API for admin tokens.
#[derive(Default)]
pub struct FailureLimiter {
    by_ip: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl FailureLimiter {
    pub(crate) fn blocked(&self, ip: IpAddr) -> bool {
        let map = self.by_ip.lock();
        matches!(map.get(&ip), Some((n, since)) if *n >= MAX_FAILURES_PER_MINUTE && since.elapsed() < Duration::from_secs(60))
    }

    pub(crate) fn failed(&self, ip: IpAddr) {
        let mut map = self.by_ip.lock();
        if map.len() > 50_000 {
            map.retain(|_, (_, since)| since.elapsed() < Duration::from_secs(60));
        }
        let e = map.entry(ip).or_insert((0, Instant::now()));
        if e.1.elapsed() >= Duration::from_secs(60) {
            *e = (0, Instant::now());
        }
        e.0 += 1;
    }
}

/// Finds the gateway key in a request. It is accepted as a Bearer token and in the headers
/// provider SDKs use, so that an SDK works with only its base URL and key changed.
pub fn presented_key(headers: &HeaderMap) -> Option<&str> {
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim);
    let from = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim);
    bearer.or_else(|| from("x-api-key")).or_else(|| from("x-goog-api-key")).filter(|k| !k.is_empty())
}

/// Gemini's SDKs and REST examples can put the key in the query (`?key=`). A gateway key found
/// there is taken out of the URI before anything reads or records it, and presented as
/// `x-goog-api-key` unless the request carries a key already. A provider's own key stays.
pub fn take_query_key(parts: &mut axum::http::request::Parts) {
    if let Some((key, uri)) = query_key(&parts.uri) {
        parts.uri = uri;
        if presented_key(&parts.headers).is_none()
            && let Ok(mut v) = axum::http::HeaderValue::from_str(&key)
        {
            v.set_sensitive(true);
            parts.headers.insert("x-goog-api-key", v);
        }
    }
}

fn query_key(uri: &Uri) -> Option<(String, Uri)> {
    let query = uri.query()?;
    let mut key = None;
    let rest: Vec<&str> = query
        .split('&')
        .filter(|pair| match pair.strip_prefix("key=") {
            Some(v) if key.is_none() && v.starts_with(API_KEY_PREFIX) => {
                key = Some(v.to_owned());
                false
            }
            _ => true,
        })
        .collect();
    let key = key?;
    let rebuilt = if rest.is_empty() { uri.path().to_owned() } else { format!("{}?{}", uri.path(), rest.join("&")) };
    Some((key, rebuilt.parse().ok()?))
}

/// Authenticates the caller and checks that the key may use `mode`.
pub fn authenticate(gw: &Gw, headers: &HeaderMap, peer: Option<IpAddr>, mode: Mode) -> Result<Caller, GwError> {
    let caller = identify(gw, headers, peer)?;
    if !caller.key.doc.allowed_modes.contains(&mode) {
        return Err(GwError::not_allowed(format!("this key may not use {} mode", mode.as_str())));
    }
    Ok(caller)
}

/// Authenticates the caller without asking for a mode. For the endpoints that only describe
/// what a key can do (the model list, the quota state).
pub fn identify(gw: &Gw, headers: &HeaderMap, peer: Option<IpAddr>) -> Result<Caller, GwError> {
    if let Some(ip) = peer
        && gw.auth_failures.blocked(ip)
    {
        return Err(GwError::rate_limited("too many requests with invalid keys from this address", 60));
    }
    let fail = || {
        if let Some(ip) = peer {
            gw.auth_failures.failed(ip);
        }
        GwError::invalid_key()
    };

    let presented = presented_key(headers).ok_or_else(fail)?;
    // Anything that is not shaped like a gateway key is refused without hashing it.
    if !presented.starts_with(API_KEY_PREFIX) || presented.len() > 256 {
        return Err(fail());
    }
    let snapshot = gw.config.snapshot();
    let hmac = gw.boot.pepper.hash(presented);
    let key = snapshot.key(&hmac, Utc::now()).cloned().ok_or_else(fail)?;
    let workspace = snapshot
        .workspace_of(&key)
        .cloned()
        .ok_or_else(|| GwError::not_allowed("the workspace of this key, or its organisation, is disabled"))?;
    let named = headers.get("x-gw-agent-id").and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty());
    let agent = agent_of(&key, &workspace, named)?;
    let key = match &agent {
        Some(a) => Arc::new(narrowed(&key, &a.doc)),
        None => key,
    };
    Ok(Caller { snapshot, key, workspace, internal: None, agent })
}

/// The registered agent a call is made for (catalogue MCP-15): the one its key speaks for,
/// chosen by `x-gw-agent-id` when the key speaks for several. A key that speaks for none may
/// name an agent that is not registered, unless the workspace accepts registered agents only;
/// it may never name one that is. A disabled agent's calls are refused.
pub fn agent_of(key: &KeyRt, ws: &WorkspaceRt, named: Option<&str>) -> Result<Option<Arc<AgentRt>>, GwError> {
    let agent = match (key.doc.agents.as_slice(), named) {
        ([], None) => return Ok(None),
        ([], Some(name)) => {
            check_label(ws, name)?;
            return Ok(None);
        }
        ([one], None) => one.as_str(),
        ([one], Some(name)) if name == one => one.as_str(),
        ([one], Some(name)) => {
            return Err(GwError::not_allowed(format!("this key speaks for the agent {one}, not {name}")));
        }
        (_, None) => {
            return Err(GwError::not_allowed(
                "this key speaks for several agents: name the agent of the call in x-gw-agent-id",
            ));
        }
        (many, Some(name)) => many
            .iter()
            .find(|a| *a == name)
            .map(String::as_str)
            .ok_or_else(|| GwError::not_allowed(format!("this key does not speak for the agent {name}")))?,
    };
    let rt = ws.agents.get(agent).cloned().ok_or_else(|| {
        GwError::not_allowed(format!("the agent {agent} of this key is no longer registered in the workspace"))
    })?;
    if rt.doc.status != crate::model::Status::Active {
        return Err(GwError::agent_disabled(agent));
    }
    Ok(Some(rt))
}

/// Whether a key that speaks for no registered agent may name `name` as its agent.
pub fn check_label(ws: &WorkspaceRt, name: &str) -> Result<(), GwError> {
    if ws.agents.contains_key(name) {
        return Err(GwError::not_allowed(format!(
            "{name} is a registered agent of this workspace: only its own keys speak for it"
        )));
    }
    if ws.settings.agent_labels == crate::model::AgentLabels::RegisteredOnly {
        return Err(GwError::not_allowed(
            "this workspace accepts registered agents only, and this key speaks for none",
        ));
    }
    Ok(())
}

/// A key as it stands for one of its agents: its models, templates and tool bundles narrowed
/// to the agent's, where the agent names any.
fn narrowed(key: &KeyRt, agent: &crate::model::Agent) -> KeyRt {
    let mut k = key.clone();
    k.doc.allowed_models = within(&key.doc.allowed_models, &agent.allowed_models, true);
    k.doc.allowed_templates = within(&key.doc.allowed_templates, &agent.allowed_templates, true);
    k.doc.tool_bundles = within(&key.doc.tool_bundles, &agent.tool_bundles, false);
    k
}

/// What remains of a key's list under an agent's. `empty_is_all`: an empty list allows
/// everything (models, templates), so an intersection that left nothing is written as a list
/// whose only entry matches nothing, never as an empty one. Bundles are empty for none.
fn within(key: &[String], agent: &[String], empty_is_all: bool) -> Vec<String> {
    if agent.is_empty() {
        return key.to_vec();
    }
    if key.is_empty() && empty_is_all {
        return agent.to_vec();
    }
    let both: Vec<String> = key.iter().filter(|k| agent.contains(k)).cloned().collect();
    if both.is_empty() && empty_is_all { vec![NOTHING.to_owned()] } else { both }
}

/// The entry of a narrowed list that matches no model or template: names and ids never hold a
/// NUL.
const NOTHING: &str = "\u{0}none";

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn a_gateway_key_in_the_query_is_taken_out() {
        let uri: Uri = "/v1beta/models/m:streamGenerateContent?alt=sse&key=pgw_abc_def".parse().unwrap();
        let (key, rest) = query_key(&uri).unwrap();
        assert_eq!(
            (key.as_str(), rest.to_string().as_str()),
            ("pgw_abc_def", "/v1beta/models/m:streamGenerateContent?alt=sse")
        );
        let only: Uri = "/v1beta/models/m:generateContent?key=pgw_x".parse().unwrap();
        assert_eq!(query_key(&only).unwrap().1.to_string(), "/v1beta/models/m:generateContent");
        // A key of the provider's own, or none at all, stays where it is.
        assert!(query_key(&"/v1/x?key=AIzaSy123".parse().unwrap()).is_none());
        assert!(query_key(&"/v1/x?alt=sse".parse().unwrap()).is_none());
        assert!(query_key(&"/v1/x".parse().unwrap()).is_none());
    }

    #[test]
    fn key_is_found_in_the_headers_sdks_use() {
        assert_eq!(presented_key(&headers(&[("authorization", "Bearer pgw_a_b")])), Some("pgw_a_b"));
        assert_eq!(presented_key(&headers(&[("x-api-key", "pgw_a_b")])), Some("pgw_a_b"));
        assert_eq!(presented_key(&headers(&[("x-goog-api-key", "pgw_a_b")])), Some("pgw_a_b"));
        assert_eq!(presented_key(&headers(&[("authorization", "Basic abc")])), None);
        assert_eq!(presented_key(&headers(&[("x-api-key", "")])), None);
        assert_eq!(presented_key(&headers(&[])), None);
        // The Bearer token wins when both are present.
        assert_eq!(
            presented_key(&headers(&[("authorization", "Bearer pgw_1_x"), ("x-api-key", "pgw_2_y")])),
            Some("pgw_1_x")
        );
    }

    #[test]
    fn repeated_failures_block_the_source() {
        let l = FailureLimiter::default();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        for _ in 0..MAX_FAILURES_PER_MINUTE - 1 {
            l.failed(ip);
        }
        assert!(!l.blocked(ip));
        l.failed(ip);
        assert!(l.blocked(ip));
        assert!(!l.blocked("203.0.113.10".parse().unwrap()));
    }
}
