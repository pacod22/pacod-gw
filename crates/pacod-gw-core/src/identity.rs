//! pacod-id as the core's identity provider (`PGW_AUTH=oidc`, identity concept 13.1): the
//! client, the copy of the bindings the feed keeps current, and the check of who calls. The
//! gateway's manifest (`authz/gw.yaml`) is pushed at start, and again until pacod-id takes it.
//! The data plane never asks any of this.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use pacod_id_authz::Manifest;
use pacod_id_client::{Authenticator, AuthzCache, IdClient, IdConfig};
use tokio::sync::watch;

use crate::bootstrap::IdSettings;

/// The gateway's manifest, the same text the management API's checks use.
pub const MANIFEST: &str = include_str!("../../../authz/gw.yaml");

pub struct Identity {
    pub settings: IdSettings,
    pub client: IdClient,
    pub cache: Arc<AuthzCache>,
    pub auth: Authenticator,
}

impl Identity {
    /// Starts following pacod-id's feed and pushes the manifest, in the background: the core
    /// starts whether or not pacod-id answers, and management calls wait for the copy.
    pub fn start(settings: IdSettings, stop: watch::Receiver<bool>) -> Arc<Self> {
        let client = IdClient::new(IdConfig {
            issuers: settings.issuers.clone(),
            internal_url: settings.internal_url.clone(),
            client_id: settings.client_id.clone(),
            client_secret: settings.client_secret.clone(),
            product: "gw".into(),
            audience: settings.audience.clone(),
        });
        let cache = AuthzCache::spawn(client.clone(), stop.clone());
        let auth = Authenticator::new(client.clone(), cache.clone());
        tokio::spawn(push_manifest(client.clone(), stop));
        Arc::new(Identity { settings, client, cache, auth })
    }

    /// Whether the copy of the bindings is current enough for the management API (concept 7.5:
    /// older than `PGW_ID_MAX_STALENESS`, admin calls answer 503).
    pub fn fresh(&self) -> bool {
        let max = Duration::from_secs(self.settings.max_staleness_seconds);
        self.cache.is_ready() && self.cache.feed_age().is_some_and(|age| age <= max)
    }

    /// Where people sign in, for the answers that send them there.
    pub fn issuer(&self) -> &str {
        self.settings.issuers.first().map_or("", String::as_str)
    }
}

async fn push_manifest(client: IdClient, mut stop: watch::Receiver<bool>) {
    let source = Manifest::from_yaml(MANIFEST).expect("authz/gw.yaml is valid").source().clone();
    let mut backoff = Duration::from_secs(1);
    loop {
        match client.push_catalog(&source, &HashMap::new()).await {
            Ok(pushed) => {
                tracing::info!(
                    version = pushed.version,
                    changed = pushed.changed,
                    "the gateway's manifest is at pacod-id"
                );
                return;
            }
            Err(e) => tracing::warn!(error = %e, "pushing the gateway's manifest to pacod-id failed; trying again"),
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = stop.wait_for(|s| *s) => return,
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}
