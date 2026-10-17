//! Service tokens (`pidt_`, and the gateway's imported `pgwa_`), checked by pacod-id's
//! introspection and cached 60 seconds by the token's SHA-256. A revoked token,
//! or a disabled or deleted service account, leaves the cache as soon as the feed says so.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::{ApiError, Event, IdClient};

const FOR: Duration = Duration::from_secs(60);
/// Past this many entries the cache starts again: refused guesses must not fill memory.
const MAX: usize = 10_000;

/// A service account a token stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePrincipal {
    pub sub: String,
    pub token_id: String,
    pub name: String,
    pub organisation_id: Option<String>,
}

struct Entry {
    until: Instant,
    principal: Option<ServicePrincipal>,
}

pub struct Introspector {
    client: IdClient,
    cache: Mutex<HashMap<[u8; 32], Entry>>,
}

impl Introspector {
    pub fn new(client: IdClient) -> Arc<Self> {
        Arc::new(Introspector { client, cache: Mutex::new(HashMap::new()) })
    }

    /// Forgets cached tokens as the feed revokes them or disables their accounts. Runs until the
    /// feed's sender goes.
    pub fn follow(self: &Arc<Self>, mut events: broadcast::Receiver<Event>) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(e) => me.forget(&e),
                    Err(broadcast::error::RecvError::Lagged(_)) => me.cache.lock().expect("lock").clear(),
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    fn forget(&self, e: &Event) {
        let token = e.data["token_id"].as_str();
        let sub = e.data["sub"].as_str();
        let drop = matches!(e.kind.as_str(), "credential.revoked" | "principal.disabled" | "principal.deleted");
        if drop {
            self.cache.lock().expect("lock").retain(|_, entry| match &entry.principal {
                Some(p) => token.is_none_or(|t| t != p.token_id) && sub.is_none_or(|s| s != p.sub),
                None => true,
            });
        }
    }

    /// The service account of a token, or `None` when pacod-id does not take it.
    pub async fn check(&self, token: &str) -> Result<Option<ServicePrincipal>, ApiError> {
        // Only what looks like a service token (`pidt_…`, or a product's own like `pgwa_…`)
        // costs a call: random strings neither reach pacod-id nor fill the cache.
        if !looks_like_service_token(token) {
            return Ok(None);
        }
        let key = crate::sha256(token.as_bytes());
        if let Some(e) = self.cache.lock().expect("lock").get(&key)
            && e.until > Instant::now()
        {
            return Ok(e.principal.clone());
        }
        let v = self.client.introspect_raw(token).await?;
        let principal = (v["active"] == true && v["pid_kind"] == "service")
            .then(|| {
                Some(ServicePrincipal {
                    sub: v["sub"].as_str()?.to_owned(),
                    token_id: v["token_id"].as_str().unwrap_or_default().to_owned(),
                    name: v["name"].as_str().unwrap_or_default().to_owned(),
                    organisation_id: v["organisation_id"].as_str().map(str::to_owned),
                })
            })
            .flatten();
        // Kept no longer than the token lives.
        let left = v["exp"].as_i64().map(|e| Duration::from_secs(u64::try_from(e - crate::now()).unwrap_or(0)));
        let until = Instant::now() + left.map_or(FOR, |l| l.min(FOR));
        let mut cache = self.cache.lock().expect("lock");
        if cache.len() >= MAX {
            let now = Instant::now();
            cache.retain(|_, e| e.until > now);
        }
        // Full of live entries: a refusal is not kept, so refusals cannot push out the tokens
        // that work; a token that works replaces everything only when nothing else is left.
        if cache.len() >= MAX && principal.is_none() {
            return Ok(principal);
        }
        if cache.len() >= MAX {
            cache.clear();
        }
        cache.insert(key, Entry { until, principal: principal.clone() });
        Ok(principal)
    }
}

/// `<prefix>_<id>_<secret>` or `<prefix>_<secret>`: lowercase letters, then base62 and `_`/`-`.
fn looks_like_service_token(t: &str) -> bool {
    let Some((prefix, rest)) = t.split_once('_') else { return false };
    (2..=8).contains(&prefix.len())
        && prefix.bytes().all(|b| b.is_ascii_lowercase())
        && (16..=256).contains(&rest.len())
        && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_token_shapes_are_introspected() {
        assert!(super::looks_like_service_token(&format!("pidt_01a1_{}", "x".repeat(43))));
        assert!(super::looks_like_service_token(&format!("pgwa_{}", "A".repeat(40))));
        for bad in ["", "Bearer", "eyJhbGciOi.x.y", "pidt_", "PIDT_abcdefghijklmnopqr", "pidt_has spaces in it here"] {
            assert!(!super::looks_like_service_token(bad), "{bad}");
        }
    }
}
