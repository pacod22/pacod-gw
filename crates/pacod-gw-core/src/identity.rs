//! pacod-id as the core's identity provider (`PGW_AUTH=oidc`): the
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

/// How often a row whose scope pacod-id does not know is tried before it is parked: about
/// eight and a half minutes with the backoff.
const UNKNOWN_SCOPE_TRIES: i32 = 10;

impl Identity {
    /// Starts following pacod-id's feed and pushes the manifest, in the background: the core
    /// starts whether or not pacod-id answers, and management calls wait for the copy.
    pub fn start(settings: IdSettings, stop: watch::Receiver<bool>) -> Arc<Self> {
        let client = client(&settings);
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
        let facade = self.exchanger.for_caller(bearer).await.map_err(|e| match (e.status(), e.code()) {
            // The caller's session ended, or their account was disabled, since the check.
            (_, Some("invalid_grant")) => ApiError::unauthorized("invalid or expired credentials"),
            // The core's client is refused (its secret, its audience): the installation's fault.
            (Some(400 | 401 | 403), _) => {
                tracing::error!(error = %e, "pacod-id refused the core's token exchange; check PGW_ID_CLIENT_SECRET and the client's audience");
                ApiError::internal("pacod-id refused the core's client")
            }
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

    /// Delivers the identity outbox in order (concept 13.1): organisations and workspaces become
    /// scopes at pacod-id. One deliverer at a time (an advisory lock); a row pacod-id cannot
    /// take yet (`scope_unknown`, `catalog_missing`) or a pacod-id that does not answer stops
    /// the round, to be tried again in order; a row it refuses for good is parked and logged,
    /// so that it does not hold up the rest. A workspace whose organisation stays unknown (its
    /// registration was refused) is parked too after [`UNKNOWN_SCOPE_TRIES`] tries; the sweep
    /// queues it again once the organisation is there. Returns how many rows went through.
    pub async fn deliver_outbox(&self, store: &crate::store::Store) -> usize {
        let mut tx = match store.idp_turn().await {
            Ok(Some(tx)) => tx,
            Ok(None) => return 0,
            Err(e) => {
                tracing::warn!(error = %e, "the identity outbox cannot be read");
                return 0;
            }
        };
        let rows = match crate::store::Store::idp_pending(&mut tx, 100).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "the identity outbox cannot be read");
                return 0;
            }
        };
        let mut done = 0;
        for row in rows {
            if row.next_attempt_at > chrono::Utc::now() {
                break;
            }
            match self.deliver(&row).await {
                Ok(()) => {
                    if crate::store::Store::idp_done(&mut tx, row.id).await.is_err() {
                        break;
                    }
                    done += 1;
                }
                Err(e) => {
                    let code = e.code().unwrap_or_default().to_owned();
                    // 401 and 403 say the core's own client is wrong (its secret, its rights at
                    // pacod-id): an operator fixes that, and the row must still go through then.
                    let refused_client = e.status().is_some_and(|s| s == 401 || s == 403);
                    let later = code == "catalog_missing"
                        || (code == "scope_unknown" && row.attempts + 1 < UNKNOWN_SCOPE_TRIES)
                        || refused_client
                        || e.status().is_none_or(|s| s >= 500 || s == 429);
                    if refused_client {
                        tracing::warn!(target = %row.target, error = %e,
                            "pacod-id refused the core's client; the identity outbox waits (check PGW_ID_CLIENT_SECRET)");
                    } else if later {
                        tracing::info!(target = %row.target, error = %e, "the identity outbox waits for pacod-id");
                    } else {
                        tracing::error!(target = %row.target, op = %row.op, error = %e,
                            "pacod-id refused an identity outbox row for good; it is parked");
                    }
                    let _ = crate::store::Store::idp_failed(&mut tx, row.id, &e.to_string(), !later).await;
                    if later {
                        break;
                    }
                }
            }
        }
        if let Err(e) = tx.commit().await {
            tracing::warn!(error = %e, "the identity outbox's progress was not saved");
            return 0;
        }
        done
    }

    async fn deliver(&self, row: &crate::store::idp::IdpRow) -> Result<(), pacod_id_client::ApiError> {
        let p = &row.payload;
        let s = |k: &str| p[k].as_str().unwrap_or_default().to_owned();
        let system = p["system"].as_bool().unwrap_or(false);
        match row.op.as_str() {
            "put_organisation" => {
                self.client.put_organisation(&s("id"), &s("slug"), &s("display_name"), system).await.map(|_| ())
            }
            "delete_organisation" => self.client.delete_organisation(&s("id")).await.map(|_| ()),
            "put_scope" => {
                self.client.put_scope(&s("kind"), &s("id"), &s("parent"), &s("display_name"), system).await.map(|_| ())
            }
            "delete_scope" => self.client.delete_scope(&s("kind"), &s("id")).await.map(|_| ()),
            other => Err(pacod_id_client::ApiError::Malformed(format!("unknown outbox operation {other}"))),
        }
    }

    /// Queues what pacod-id should know but its feed does not show: organisations and
    /// workspaces made before OIDC mode, the built-in `_system` ones, an import's. Changes made
    /// through the API reach the outbox in their own transaction; this catches the rest.
    pub async fn sweep(&self, store: &crate::store::Store, snap: &crate::snapshot::Snapshot) {
        if !self.cache.is_ready() {
            return;
        }
        let known = self.cache.snapshot();
        let pending: std::collections::HashSet<String> = match store.idp_targets().await {
            Ok(t) => t,
            Err(_) => return,
        };
        let mut queued = 0;
        for (id, org) in &snap.organisations {
            let target = format!("organisation:{id}");
            if known.scope_names.get(&target) == Some(&org.doc.display_name) || pending.contains(&target) {
                continue;
            }
            let payload = serde_json::json!({
                "id": id, "slug": org.doc.name, "display_name": org.doc.display_name,
                "system": id == crate::model::SYSTEM_ORG_ID,
            });
            if store.idp_enqueue_existing(id, "put_organisation", &target, &payload).await.unwrap_or(false) {
                queued += 1;
            }
        }
        for (id, ws) in &snap.workspaces {
            let target = format!("gw.workspace:{id}");
            if known.scope_names.get(&target) == Some(&ws.doc.display_name) || pending.contains(&target) {
                continue;
            }
            let payload = serde_json::json!({
                "kind": "gw.workspace", "id": id, "parent": format!("organisation:{}", ws.organisation_id),
                "display_name": ws.doc.display_name, "system": id == crate::model::SYSTEM_WS_ID,
            });
            if store.idp_enqueue_existing(id, "put_scope", &target, &payload).await.unwrap_or(false) {
                queued += 1;
            }
        }
        if queued > 0 {
            tracing::info!(queued, "organisations and workspaces queued for pacod-id");
        }
    }

    /// Delivers the outbox every half second, sweeps at start and every five minutes, and reports
    /// the outbox's rows every fifteen seconds.
    pub fn run_outbox(
        self: &Arc<Self>,
        store: crate::store::Store,
        config: Arc<crate::snapshot::ConfigState>,
        mut stop: watch::Receiver<bool>,
    ) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut swept_at: Option<tokio::time::Instant> = None;
            let mut counted_at: Option<tokio::time::Instant> = None;
            loop {
                if me.cache.is_ready() && swept_at.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) {
                    me.sweep(&store, &config.snapshot()).await;
                    swept_at = Some(tokio::time::Instant::now());
                }
                me.deliver_outbox(&store).await;
                if counted_at.is_none_or(|t| t.elapsed() > Duration::from_secs(15)) {
                    if let Ok((pending, parked)) = store.idp_counts().await {
                        crate::telemetry::metrics::set_identity_outbox(pending, parked);
                    }
                    counted_at = Some(tokio::time::Instant::now());
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                    _ = stop.wait_for(|s| *s) => return,
                }
            }
        });
    }

    /// Forgets the accounts pacod-id deletes (concept 6.9) and confirms each: on its
    /// `principal.deleted`, and every minute for those the snapshot still lists as pending (an
    /// event missed while the core was down or lagging). Forgetting is idempotent; a failed
    /// confirmation is tried again.
    pub fn run_deletions(self: &Arc<Self>, store: crate::store::Store, mut stop: watch::Receiver<bool>) {
        let me = self.clone();
        let mut events = self.cache.subscribe();
        tokio::spawn(async move {
            // Handled: confirmed to pacod-id, or a service account's (below).
            let mut confirmed: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut sweep = tokio::time::interval(Duration::from_secs(60));
            loop {
                let subs: Vec<String> = tokio::select! {
                    e = events.recv() => match e {
                        // A person's account. A deleted service account (an admin token) keeps the
                        // record of what it did, under its id: whoever looks into it needs that.
                        // The cache's `deleted` has it too (to refuse its tokens), so the sweep
                        // must skip it; pacod-id waits for no confirmation of one.
                        Ok(e) if e.kind == "principal.deleted" => match e.data["sub"].as_str() {
                            Some(sub) if e.data["principal_kind"] == "service" => {
                                confirmed.insert(sub.to_owned());
                                continue;
                            }
                            sub => sub.map(str::to_owned).into_iter().collect(),
                        },
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                    _ = sweep.tick() => {
                        if !me.cache.is_ready() {
                            continue;
                        }
                        me.cache.snapshot().deleted.iter().filter(|s| !confirmed.contains(*s)).cloned().collect()
                    }
                    _ = stop.wait_for(|s| *s) => return,
                };
                for sub in subs {
                    match forget(&store, &sub).await {
                        Err(e) => tracing::warn!(%sub, error = %e, "forgetting a deleted account failed; tried again"),
                        Ok(()) => match me.client.deletion_done(&sub).await {
                            Ok(()) => {
                                tracing::info!(%sub, "a deleted account is forgotten here");
                                confirmed.insert(sub);
                            }
                            Err(e) => {
                                tracing::warn!(%sub, error = %e, "confirming a deletion to pacod-id failed; tried again")
                            }
                        },
                    }
                }
            }
        });
    }

    /// Where people sign in, for the answers that send them there.
    pub fn issuer(&self) -> &str {
        self.settings.issuers.first().map_or("", String::as_str)
    }
}

/// Forgets an account pacod-id deleted, in one transaction: the gateway's own copy of it (until
/// `identity finalize`, its user or admin token and role bindings), its sessions and links, and
/// its name in the records of what it did.
pub async fn forget(store: &crate::store::Store, sub: &str) -> Result<(), crate::store::StoreError> {
    // The configuration's lock only while there is a copy to delete (until identity finalize).
    if store.identity_copy_exists(sub).await? {
        let audit = || crate::store::AuditEntry::system("identity-deletion");
        let mut tx = store.begin().await?;
        for row in tx.list("role_binding", None).await? {
            if row.doc["principal"] == sub {
                tx.delete(&row.id, audit()).await?;
            }
        }
        if tx.get(sub).await?.is_some_and(|r| r.kind == "user" || r.kind == "admin_token") {
            tx.delete(sub, audit()).await?;
        }
        tx.commit().await?;
    }
    store.forget_principal(sub).await
}

/// The core's client of pacod-id, as `PGW_ID_*` configure it.
pub fn client(settings: &IdSettings) -> IdClient {
    IdClient::new(IdConfig {
        issuers: settings.issuers.clone(),
        internal_url: settings.internal_url.clone(),
        client_id: settings.client_id.clone(),
        client_secret: settings.client_secret.clone(),
        product: "gw".into(),
        audience: settings.audience.clone(),
    })
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
