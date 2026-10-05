//! Authenticates gateway API keys on the data plane (concept section 5.5).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use chrono::Utc;
use parking_lot::Mutex;

use super::Gw;
use super::errors::GwError;
use crate::crypto::keys::API_KEY_PREFIX;
use crate::model::Mode;
use crate::snapshot::{KeyRt, Snapshot, WorkspaceRt};

/// The caller of a data-plane request.
pub struct Caller {
    pub snapshot: Arc<Snapshot>,
    pub key: Arc<KeyRt>,
    pub workspace: Arc<WorkspaceRt>,
}

/// Failed authentications allowed per source address per minute.
const MAX_FAILURES_PER_MINUTE: u32 = 60;

/// Slows down key guessing: a source that keeps presenting bad keys is refused for a while.
#[derive(Default)]
pub struct FailureLimiter {
    by_ip: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl FailureLimiter {
    fn blocked(&self, ip: IpAddr) -> bool {
        let map = self.by_ip.lock();
        matches!(map.get(&ip), Some((n, since)) if *n >= MAX_FAILURES_PER_MINUTE && since.elapsed() < Duration::from_secs(60))
    }

    fn failed(&self, ip: IpAddr) {
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
    Ok(Caller { snapshot, key, workspace })
}

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
