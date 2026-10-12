//! pacod-id as the core's identity provider (`PGW_AUTH=oidc`, identity concept 13.1): the
//! client, the copy of the bindings the feed keeps current, and the check of who calls. The
//! gateway's manifest (`authz/gw.yaml`) is pushed at start, and again until pacod-id takes it.
//! The data plane never asks any of this.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use pacod_id_authz::Manifest;
use pacod_id_client::{Authenticator, AuthzCache, IdClient, IdConfig, TokenExchanger};
use serde_json::Value;

use crate::mgmt::error::ApiError;
use tokio::sync::watch;

use crate::bootstrap::IdSettings;

/// The gateway's manifest, the same text the management API's checks use.
pub const MANIFEST: &str = include_str!("../../../authz/gw.yaml");

pub struct Identity {
    pub settings: IdSettings,
    pub client: IdClient,
    pub cache: Arc<AuthzCache>,
    pub auth: Authenticator,
    /// Facade tokens per caller, cached for their short life.
    pub exchanger: TokenExchanger,
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
        let exchanger = TokenExchanger::new(client.clone());
        Arc::new(Identity { settings, client, cache, auth, exchanger })
    }

    /// Whether the copy of the bindings is current enough for the management API (concept 7.5:
    /// older than `PGW_ID_MAX_STALENESS`, admin calls answer 503).
    pub fn fresh(&self) -> bool {
        let max = Duration::from_secs(self.settings.max_staleness_seconds);
        self.cache.is_ready() && self.cache.feed_age().is_some_and(|age| age <= max)
    }

    /// Keeps this instance's row in `instances` telling how far its copy has come, for the
    /// facades' "applied on n of m" (concept 7.4).
    pub fn report_applied(
        self: &Arc<Self>,
        store: crate::store::Store,
        instance: String,
        mut stop: watch::Receiver<bool>,
    ) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut reported = -1;
            loop {
                let seq = me.cache.seq();
                if me.cache.is_ready() && seq != reported {
                    match store.instance_authz_seq(&instance, seq).await {
                        Ok(()) => reported = seq,
                        Err(e) => tracing::debug!(error = %e, "recording the copy's position failed"),
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                    _ = stop.wait_for(|s| *s) => return,
                }
            }
        });
    }

    /// Calls pacod-id's admin API acting for the caller of `bearer` (concept 13.2): pacod-id
    /// decides with the caller's own rights. A refusal comes back with pacod-id's status,
    /// message and details, so that the console can show it; the type follows its
    /// `details.code`.
    pub async fn admin(
        &self,
        bearer: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ApiError> {
        let unreachable = || {
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "identity_unavailable",
                "pacod-id cannot be reached; try again shortly",
            )
        };
        let facade = self.exchanger.for_caller(bearer).await.map_err(|e| match e.status() {
            Some(400 | 401) => ApiError::unauthorized("invalid or expired credentials"),
            _ => {
                tracing::warn!(error = %e, "exchanging a caller's token at pacod-id failed");
                unreachable()
            }
        })?;
        let mut request = self.client.admin(method, path, &facade);
        if let Some(b) = body {
            request = request.json(b);
        }
        let response = request.send().await.map_err(|e| {
            tracing::warn!(error = %e, "pacod-id's admin API cannot be reached");
            unreachable()
        })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let value: Value =
            if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
        if status.is_success() {
            return Ok(value);
        }
        let details = value["error"]["details"].clone();
        let code = details["code"].as_str().unwrap_or_default().to_owned();
        let message = value["error"]["message"].as_str().unwrap_or("pacod-id refused the change").to_owned();
        let status = axum::http::StatusCode::from_u16(status.as_u16()).unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
        if status.is_server_error() {
            tracing::warn!(%code, %message, "pacod-id's admin API failed");
        }
        Err(ApiError::new(status, kind_of(&code), message).with_details(details))
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

/// The gateway's error type for a pacod-id code: the codes the console handles keep their name.
fn kind_of(code: &str) -> &'static str {
    match code {
        "not_allowed" | "forbidden" => "forbidden",
        "holds_more_rights" => "holds_more_rights",
        "last_holder" => "last_holder",
        "invitation_exists" => "invitation_exists",
        "email_taken" => "email_taken",
        "principal_unknown" | "not_found" => "not_found",
        "scope_unknown" => "scope_unknown",
        "bad_email" | "bad_locale" | "bad_status" | "bad_name" | "bad_scope" | "bad_request" => "validation_failed",
        c if c.starts_with("password_") => "validation_failed",
        "conflict" => "conflict",
        "unauthorized" | "invalid_token" => "unauthorized",
        _ => "identity_refused",
    }
}
