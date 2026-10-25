//! Providers and their credentials.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crypto::envelope::{Kek, fingerprint, seal};
use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{BuildCtx, Item, Ops, Page, Params, Resource, ScopeKind, register};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::{OpenApi, page_schema};
use crate::model::{
    Credential, CredentialScope, CredentialState, Doc, Obj, Provider, ProviderPreset, is_slug, new_id, provider_presets,
};
use crate::snapshot::Snapshot;
use crate::store::Tx;

impl Resource for Provider {
    const PLURAL: &'static str = "providers";
    const LABEL: &'static str = "provider";
    const SCOPE: ScopeKind = ScopeKind::Global;
    type Input = Provider;
    type View = Provider;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(mut input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if !is_slug(&input.name) {
            return Err(ApiError::invalid("name must be lower-case letters, digits and hyphens"));
        }
        if input.display_name.trim().is_empty() {
            input.display_name = input.name.clone();
        }
        input.base_url = input.base_url.trim().trim_end_matches('/').to_owned();
        ctx.st.egress.check_url(&input.base_url).map_err(|e| ApiError::invalid(format!("base_url: {e}")))?;
        if let Some(p) = &mut input.proxy_base_url {
            *p = p.trim().trim_end_matches('/').to_owned();
            ctx.st.egress.check_url(p).map_err(|e| ApiError::invalid(format!("proxy_base_url: {e}")))?;
        }
        for path in &input.proxy_paths {
            if !path.starts_with('/') {
                return Err(ApiError::invalid("proxy_paths entries must start with a slash"));
            }
        }
        let service_account = input.auth == Some(crate::model::AuthScheme::GoogleServiceAccount);
        if input.platform == crate::model::Platform::Vertex {
            if input.family != crate::model::Family::Gemini || input.managed_api != crate::model::ManagedApi::Native {
                return Err(ApiError::invalid("platform vertex is for the Gemini family with managed_api native"));
            }
            if !input.proxy_paths.is_empty() {
                return Err(ApiError::invalid(
                    "proxy_paths: Vertex AI's are fixed, the model methods of the provider's own project",
                ));
            }
            let site = crate::model::VertexSite::of(&input.base_url).ok_or_else(|| {
                ApiError::invalid(
                    "base_url: Vertex AI's is https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google (aiplatform.googleapis.com for the global location), or https://aiplatform.googleapis.com/v1/publishers/google in express mode",
                )
            })?;
            if service_account && site.project.is_none() {
                return Err(ApiError::invalid(
                    "base_url: a service account works on a project; the express path takes an API key",
                ));
            }
        } else if service_account {
            return Err(ApiError::invalid("auth google_service_account is for providers on platform vertex"));
        }
        // The stored credentials must fit the scheme: a key file is never sent as an API key,
        // nor an API key used as a key file.
        if let Some(rt) = existing.and_then(|e| ctx.snap.providers.get(&e.id)) {
            let c = &rt.credentials;
            for s in c.global.iter().chain(c.organisations.values()).chain(c.workspaces.values()) {
                if crate::upstream::google::looks_like_service_account(&s.value) != service_account {
                    return Err(ApiError::invalid(if service_account {
                        "auth: the provider holds API keys; remove them, change auth, then add the service account's key file"
                    } else {
                        "auth: the provider holds service-account key files, which are not sent as API keys; replace its credentials first"
                    }));
                }
            }
        }
        if input.managed_api == crate::model::ManagedApi::Native && input.family != crate::model::Family::Gemini {
            return Err(ApiError::invalid("managed_api native exists for the Gemini family only"));
        }
        if let Some(name) = &input.holidays
            && !ctx.st.config.snapshot().settings.holiday_calendars.contains_key(name)
        {
            return Err(ApiError::invalid(format!(
                "holidays: the settings have no holiday calendar \"{name}\" (holiday_calendars)"
            )));
        }
        if !(1..=600).contains(&input.default_timeout_seconds) {
            return Err(ApiError::invalid("default_timeout_seconds must be between 1 and 600"));
        }
        let q = &input.quirks;
        if q.reasoning_field.is_empty() || q.drop_fields.len() > 32 || q.drop_fields.iter().any(|f| f.is_empty()) {
            return Err(ApiError::invalid(
                "quirks: reasoning_field must not be empty and drop_fields takes up to 32 field names",
            ));
        }
        const GOVERNED: &[&str] = &["model", "messages", "stream", "stream_options", "gateway"];
        if let Some(f) = q.drop_fields.iter().find(|f| GOVERNED.contains(&f.as_str())) {
            return Err(ApiError::invalid(format!(
                "quirks.drop_fields cannot name \"{f}\": the gateway sets that field itself"
            )));
        }
        // Fields added to every request: the conversation and what the gateway governs stay
        // the caller's and the template's.
        const CONVERSATION: &[&str] =
            &["tools", "tool_choice", "response_format", "max_tokens", "max_completion_tokens"];
        if let Some(f) =
            q.request_fields.keys().find(|f| GOVERNED.contains(&f.as_str()) || CONVERSATION.contains(&f.as_str()))
        {
            return Err(ApiError::invalid(format!(
                "quirks.request_fields cannot name \"{f}\": that field is the caller's or the gateway's"
            )));
        }
        if q.request_fields.len() > 16 || q.request_fields.values().map(|v| v.to_string().len()).sum::<usize>() > 4096 {
            return Err(ApiError::invalid("quirks.request_fields takes up to 16 members and 4 KiB"));
        }
        if q.error_rules.code_at.len() > 8 || q.error_rules.quota.len() > 64 {
            return Err(ApiError::invalid("quirks.error_rules takes up to 8 paths and 64 codes"));
        }
        Ok(input)
    }

    fn effects(obj: &Obj<Self>, snap: &Snapshot) -> Vec<String> {
        let models: Vec<&str> =
            snap.models.values().filter(|m| m.doc.provider_id == obj.id).map(|m| m.doc.name.as_str()).collect();
        let mut out = Vec::new();
        if !models.is_empty() {
            out.push(format!("calls to its models are refused: {}", models.join(", ")));
        }
        out.push(format!("proxy mode no longer relays /proxy/{}/…", obj.doc.name));
        out
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        if tx.count_children(&obj.id).await? > 0 {
            return Err(ApiError::conflict("the provider still has credentials; remove them first"));
        }
        for row in tx.list(crate::model::Model::KIND, None).await? {
            if row.doc["provider_id"] == obj.id.as_str() {
                return Err(ApiError::conflict("models still use this provider; remove or retire them first"));
            }
        }
        Ok(())
    }
}

// --- credentials -----------------------------------------------------------------------------

/// A new provider key.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CredentialInput {
    #[serde(default)]
    pub label: String,
    /// The provider key. It is encrypted before it is stored and never returned.
    #[schemars(extend("writeOnly" = true))]
    pub secret: String,
    /// Who the credential is for. Defaults to the shared credential of the installation.
    #[serde(default = "global_scope")]
    pub scope: CredentialScope,
    /// Use this credential for new requests from now on. Default true.
    #[serde(default = "yes")]
    pub make_current: bool,
    /// How long the credential it replaces is kept for a rollback, in hours. Default 24.
    #[serde(default)]
    pub grace_hours: Option<u32>,
}

fn global_scope() -> CredentialScope {
    CredentialScope::Global
}

fn yes() -> bool {
    true
}

/// A credential as the API returns it. The secret is never part of it.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CredentialView {
    pub provider_id: String,
    pub scope: CredentialScope,
    pub label: String,
    /// Identifies the secret without revealing it.
    pub fingerprint: String,
    pub state: CredentialState,
    pub retire_at: Option<DateTime<Utc>>,
    /// The key-encryption key this credential is encrypted with.
    pub kek_id: String,
}

fn view(obj: &Obj<Credential>) -> Item<CredentialView> {
    let d = &obj.doc;
    Item {
        meta: obj.meta(),
        data: CredentialView {
            provider_id: d.provider_id.clone(),
            scope: d.scope.clone(),
            label: d.label.clone(),
            fingerprint: d.fingerprint.clone(),
            state: d.state,
            retire_at: d.retire_at,
            kek_id: d.sealed.kek_id.clone(),
        },
    }
}

/// Whether a principal may manage credentials of a scope.
fn can_manage(p: &Principal, scope: &CredentialScope, snap: &Snapshot) -> bool {
    match scope {
        CredentialScope::Global => p.is_platform_admin(),
        CredentialScope::Organisation(org) => snap.organisations.contains_key(org) && p.can_admin_org(org),
        CredentialScope::Workspace(ws) => {
            snap.workspaces.get(ws).is_some_and(|w| p.can_admin_workspace(ws, &w.organisation_id))
        }
    }
}

fn scope_ids(scope: &CredentialScope, snap: &Snapshot) -> (Option<String>, Option<String>) {
    match scope {
        CredentialScope::Global => (None, None),
        CredentialScope::Organisation(o) => (Some(o.clone()), None),
        CredentialScope::Workspace(w) => (snap.workspaces.get(w).map(|x| x.organisation_id.clone()), Some(w.clone())),
    }
}

/// Disables the retiring credentials whose `retire_at` has passed, so that a credential
/// replaced in a rotation cannot be made current again by mistake after its grace period.
/// Safe to run twice: a credential already disabled is left alone, and a concurrent change
/// makes the update fail on its version.
pub async fn disable_retired_credentials(st: &AdminState) -> anyhow::Result<usize> {
    let now = Utc::now();
    let snap = st.config.snapshot();
    let mut tx = st.store.begin().await?;
    let mut disabled = 0;
    for row in tx.list(Credential::KIND, None).await? {
        let obj: Obj<Credential> = row.into_obj()?;
        if obj.doc.state != CredentialState::Retiring || obj.doc.retire_at.is_none_or(|t| t > now) {
            continue;
        }
        let mut doc = obj.doc.clone();
        doc.state = CredentialState::Disabled;
        let (org, ws) = scope_ids(&doc.scope, &snap);
        let audit = crate::store::AuditEntry {
            action: "set_state:disabled".into(),
            organisation_id: org,
            workspace_id: ws,
            detail: serde_json::json!({ "retire_at": doc.retire_at }),
            ..crate::store::AuditEntry::system("retirement")
        };
        tx.update(&obj.id, Some(obj.version), None, &serde_json::to_value(&doc)?, audit).await?;
        disabled += 1;
    }
    if disabled > 0 {
        st.commit(tx).await.map_err(|e| anyhow::anyhow!(e.message))?;
        tracing::info!(disabled, "retiring credentials disabled after their grace period");
    }
    Ok(disabled)
}

/// What re-wrapping the credentials did.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct RewrapReport {
    /// The key-encryption key every credential is now wrapped under.
    pub kek_id: String,
    /// Credentials whose data key was wrapped under the current key.
    pub rewrapped: usize,
    /// Credentials already under the current key.
    pub unchanged: usize,
    /// Credentials that could not be read, with the reason. They keep their old wrapping.
    pub failed: Vec<RewrapFailure>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RewrapFailure {
    pub credential_id: String,
    pub kek_id: String,
    pub problem: String,
}

/// The running instances that do not use `current` as their key-encryption key, or have not
/// published which key they use. Re-wrapping while one of them runs would take the provider
/// credentials away from it: it cannot open a data key wrapped under a key it does not have.
pub async fn instances_without(store: &crate::store::Store, current: &Kek) -> anyhow::Result<Vec<String>> {
    let now = Utc::now();
    let check = current.check_value();
    let mut out = Vec::new();
    for r in store.instance_records(crate::cluster::ALIVE_SECONDS).await? {
        let fresh = r.live_at.is_some_and(|t| (now - t).num_seconds() < crate::cluster::ALIVE_SECONDS);
        let report = if fresh { r.live.as_ref().and_then(crate::cluster::report::LiveReport::parse) } else { None };
        if !report.is_some_and(|rep| rep.secrets.kek_id == current.id && rep.secrets.kek == check) {
            out.push(r.row.id);
        }
    }
    Ok(out)
}

/// Wraps the data key of every credential under the current key-encryption key, after a
/// rotation (runbook, "The key-encryption key"). The secrets themselves are not encrypted
/// again. Used by `pacod-gw-core rewrap-secrets` and by `POST /system/rewrap-secrets`; the
/// caller commits.
pub async fn rewrap_credentials(
    tx: &mut Tx,
    keks: &[Kek],
    actor: &crate::store::AuditEntry,
) -> anyhow::Result<RewrapReport> {
    let current = keks.first().ok_or_else(|| anyhow::anyhow!("no key-encryption key"))?;
    let mut report = RewrapReport { kek_id: current.id.clone(), ..Default::default() };
    for row in tx.list(Credential::KIND, None).await? {
        let obj: Obj<Credential> = row.into_obj()?;
        let sealed = match crate::crypto::envelope::rewrap(keks, current, &obj.id, &obj.doc.sealed) {
            Ok(Some(sealed)) => sealed,
            Ok(None) => {
                report.unchanged += 1;
                continue;
            }
            Err(e) => {
                report.failed.push(RewrapFailure {
                    credential_id: obj.id.clone(),
                    kek_id: obj.doc.sealed.kek_id.clone(),
                    problem: e.to_string(),
                });
                continue;
            }
        };
        let from = obj.doc.sealed.kek_id.clone();
        let mut doc = obj.doc.clone();
        doc.sealed = sealed;
        let audit = crate::store::AuditEntry {
            action: "rewrap".into(),
            organisation_id: match &doc.scope {
                CredentialScope::Organisation(o) => Some(o.clone()),
                _ => None,
            },
            workspace_id: match &doc.scope {
                CredentialScope::Workspace(w) => Some(w.clone()),
                _ => None,
            },
            detail: serde_json::json!({ "from_kek_id": from, "to_kek_id": current.id }),
            ..actor.clone()
        };
        tx.update(&obj.id, Some(obj.version), None, &serde_json::to_value(&doc)?, audit).await?;
        report.rewrapped += 1;
    }
    Ok(report)
}

async fn provider_of(st: &AdminState, params: &std::collections::HashMap<String, String>) -> ApiResult<Obj<Provider>> {
    let id = params.get("id").ok_or_else(|| ApiError::not_found("provider"))?;
    st.store.get_obj::<Provider>(id).await.map_err(|_| ApiError::not_found("provider"))
}

async fn create_credential(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<CredentialInput>,
) -> ApiResult<(StatusCode, Json<Item<CredentialView>>)> {
    let snap = st.config.snapshot();
    let provider = provider_of(&st, &params).await?;
    if !can_manage(&p, &input.scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let secret = input.secret.trim();
    if secret.is_empty() {
        return Err(ApiError::invalid("secret must not be empty"));
    }
    // A service account's key is read strictly, since administrators of organisations and
    // workspaces add credentials too; an API key is never mistaken for one, or the reverse.
    use crate::upstream::google::{ServiceAccount, looks_like_service_account};
    let shown = if provider.doc.auth == Some(crate::model::AuthScheme::GoogleServiceAccount) {
        ServiceAccount::parse(secret).map_err(|e| ApiError::invalid(format!("secret: {e}")))?.fingerprint()
    } else if looks_like_service_account(secret) {
        return Err(ApiError::invalid(
            "secret: this is a service account's key, but the provider takes an API key; set the provider's auth to google_service_account",
        ));
    } else {
        fingerprint(secret)
    };
    let id = new_id(Credential::PREFIX);
    let kek = st.boot.keks.first().ok_or_else(|| ApiError::internal("no key-encryption key"))?;
    let doc = Credential {
        provider_id: provider.id.clone(),
        scope: input.scope.clone(),
        label: input.label.trim().to_owned(),
        // The credential id binds the ciphertext to this record.
        sealed: seal(kek, &id, secret.as_bytes()),
        fingerprint: shown,
        state: if input.make_current { CredentialState::Current } else { CredentialState::Retiring },
        retire_at: None,
    };
    let (org, ws) = scope_ids(&input.scope, &snap);
    let audit = p.audit(org.as_deref(), ws.as_deref());
    let created = store_credential(&st, &p, &snap, &id, &doc, input.grace_hours, audit).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Stores a new credential. Made current, it retires the current credential of its scope,
/// which is kept for a rollback for the grace period.
async fn store_credential(
    st: &AdminState,
    p: &Principal,
    snap: &Snapshot,
    id: &str,
    doc: &Credential,
    grace_hours: Option<u32>,
    audit: crate::store::AuditEntry,
) -> ApiResult<Item<CredentialView>> {
    let grace = Duration::hours(i64::from(grace_hours.unwrap_or(24).min(720)));
    let mut tx = st.store.begin().await?;
    if doc.state == CredentialState::Current {
        retire_current(&mut tx, p, snap, &doc.provider_id, &doc.scope, id, grace).await?;
    }
    let row = tx
        .create(
            Credential::KIND,
            id,
            Some(&doc.provider_id),
            None,
            &serde_json::to_value(doc).map_err(ApiError::internal)?,
            audit,
        )
        .await?;
    st.commit(tx).await?;
    Ok(view(&row.into_obj()?))
}

/// Makes the current credential of a scope at a provider retiring for `grace`, except
/// `replacing_with`, the one that takes its place.
async fn retire_current(
    tx: &mut Tx,
    p: &Principal,
    snap: &Snapshot,
    provider_id: &str,
    scope: &CredentialScope,
    replacing_with: &str,
    grace: Duration,
) -> ApiResult<()> {
    let (org, ws) = scope_ids(scope, snap);
    for row in tx.list(Credential::KIND, Some(provider_id)).await? {
        let mut old: Obj<Credential> = row.into_obj()?;
        if old.id != replacing_with && &old.doc.scope == scope && old.doc.state == CredentialState::Current {
            old.doc.state = CredentialState::Retiring;
            old.doc.retire_at = Some(Utc::now() + grace);
            let mut audit = p.audit(org.as_deref(), ws.as_deref());
            audit.action = "retire".into();
            tx.update(&old.id, None, None, &serde_json::to_value(&old.doc).map_err(ApiError::internal)?, audit).await?;
        }
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
struct CredentialQuery {
    /// Include disabled and retiring credentials. Default true.
    all: Option<bool>,
    cursor: Option<String>,
    limit: Option<i64>,
}

async fn list_credentials(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Query(q): Query<CredentialQuery>,
) -> ApiResult<Json<Page<Item<CredentialView>>>> {
    let snap = st.config.snapshot();
    let provider = provider_of(&st, &params).await?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows = st
        .store
        .list(Credential::KIND, crate::store::ParentFilter::Is(&provider.id), q.cursor.as_deref(), limit + 1)
        .await?;
    let more = rows.len() as i64 > limit;
    let mut items = Vec::new();
    let mut last = None;
    for row in rows.into_iter().take(limit as usize) {
        last = Some(row.id.clone());
        let obj: Obj<Credential> = row.into_obj()?;
        if !can_manage(&p, &obj.doc.scope, &snap) {
            continue;
        }
        if q.all == Some(false) && obj.doc.state != CredentialState::Current {
            continue;
        }
        items.push(view(&obj));
    }
    Ok(Json(Page { items, next_cursor: if more { last } else { None } }))
}

async fn load_credential(
    st: &AdminState,
    p: &Principal,
    params: &std::collections::HashMap<String, String>,
    snap: &Snapshot,
) -> ApiResult<Obj<Credential>> {
    let provider = provider_of(st, params).await?;
    let id = params.get("cid").ok_or_else(|| ApiError::not_found("credential"))?;
    let obj = st.store.get_obj::<Credential>(id).await.map_err(|_| ApiError::not_found("credential"))?;
    if obj.doc.provider_id != provider.id {
        return Err(ApiError::not_found("credential"));
    }
    if !can_manage(p, &obj.doc.scope, snap) {
        return Err(ApiError::forbidden());
    }
    Ok(obj)
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CredentialStateInput {
    /// `current` makes it the credential for new requests (the one it replaces becomes
    /// `retiring`); `disabled` takes it out of use.
    pub state: CredentialState,
}

async fn set_credential_state(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<CredentialStateInput>,
) -> ApiResult<Json<Item<CredentialView>>> {
    let snap = st.config.snapshot();
    let mut obj = load_credential(&st, &p, &params, &snap).await?;
    let (org, ws) = scope_ids(&obj.doc.scope, &snap);
    let mut tx = st.store.begin().await?;
    if input.state == CredentialState::Current {
        retire_current(&mut tx, &p, &snap, &obj.doc.provider_id, &obj.doc.scope, &obj.id, Duration::hours(24)).await?;
        obj.doc.retire_at = None;
    }
    obj.doc.state = input.state;
    let mut audit = p.audit(org.as_deref(), ws.as_deref());
    audit.action =
        format!("set_state:{}", serde_json::to_value(input.state).map_err(ApiError::internal)?.as_str().unwrap_or(""));
    let row =
        tx.update(&obj.id, None, None, &serde_json::to_value(&obj.doc).map_err(ApiError::internal)?, audit).await?;
    st.commit(tx).await?;
    Ok(Json(view(&row.into_obj()?)))
}

async fn delete_credential(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<StatusCode> {
    let snap = st.config.snapshot();
    let obj = load_credential(&st, &p, &params, &snap).await?;
    let (org, ws) = scope_ids(&obj.doc.scope, &snap);
    let mut tx = st.store.begin().await?;
    tx.delete(&obj.id, p.audit(org.as_deref(), ws.as_deref())).await?;
    st.commit(tx).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A stored key given to an organisation or a workspace, so that its secret is not entered
/// again.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CredentialCopyInput {
    /// The organisation or workspace that gets the key. A key of the installation goes to any
    /// organisation or workspace, a key of an organisation to its own workspaces.
    pub scope: CredentialScope,
    /// Defaults to the label of the key copied.
    #[serde(default)]
    pub label: Option<String>,
    /// Use the copy for new requests from now on. Default true.
    #[serde(default = "yes")]
    pub make_current: bool,
    /// How long the credential it replaces is kept for a rollback, in hours. Default 24.
    #[serde(default)]
    pub grace_hours: Option<u32>,
}

/// Whether a key of scope `from` may be given to `to`. Keys go down, from the installation to
/// an organisation or a workspace and from an organisation to its own workspaces, never
/// across: one organisation's account is not handed to another.
fn flows_down(from: &CredentialScope, to: &CredentialScope, snap: &Snapshot) -> bool {
    match (from, to) {
        (CredentialScope::Global, CredentialScope::Organisation(_) | CredentialScope::Workspace(_)) => true,
        (CredentialScope::Organisation(org), CredentialScope::Workspace(ws)) => {
            snap.workspaces.get(ws).is_some_and(|w| &w.organisation_id == org)
        }
        _ => false,
    }
}

/// Copies a stored key into another scope. The secret is opened and sealed again under the
/// new credential's id, since a sealed secret is bound to the id it was stored under; it never
/// leaves the gateway. The copy is a credential of its own: rotating the key it came from does
/// not change it.
async fn copy_credential(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<CredentialCopyInput>,
) -> ApiResult<(StatusCode, Json<Item<CredentialView>>)> {
    let snap = st.config.snapshot();
    let source = load_credential(&st, &p, &params, &snap).await?;
    if !can_manage(&p, &input.scope, &snap) {
        return Err(ApiError::forbidden());
    }
    if !flows_down(&source.doc.scope, &input.scope, &snap) {
        return Err(ApiError::invalid(
            "scope: a key of the installation goes to an organisation or a workspace, a key of an organisation to its own workspaces",
        ));
    }
    if source.doc.state == CredentialState::Disabled {
        return Err(ApiError::conflict("the credential is disabled; make it current or retiring before copying it"));
    }
    let id = new_id(Credential::PREFIX);
    let kek = st.boot.keks.first().ok_or_else(|| ApiError::internal("no key-encryption key"))?;
    let secret = crate::crypto::envelope::open(&st.boot.keks, &source.id, &source.doc.sealed)
        .map_err(|e| ApiError::conflict(format!("the credential cannot be read: {e}")))?;
    let doc = Credential {
        provider_id: source.doc.provider_id.clone(),
        scope: input.scope.clone(),
        label: input.label.map_or_else(|| source.doc.label.clone(), |l| l.trim().to_owned()),
        sealed: seal(kek, &id, &secret),
        fingerprint: source.doc.fingerprint.clone(),
        state: if input.make_current { CredentialState::Current } else { CredentialState::Retiring },
        retire_at: None,
    };
    let (org, ws) = scope_ids(&input.scope, &snap);
    let mut audit = p.audit(org.as_deref(), ws.as_deref());
    audit.action = "copy".into();
    audit.detail = serde_json::json!({ "from": source.id });
    let created = store_credential(&st, &p, &snap, &id, &doc, input.grace_hours, audit).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// A model the provider lists.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Discovered {
    /// The provider's own id of the model.
    pub upstream_id: String,
    /// The repository model with this id at this provider, when there is one.
    pub model: Option<String>,
    pub display_name: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// Whether the provider says the model takes images, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Whether the provider says the model reasons, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasons: Option<bool>,
    /// Whether the provider says the model makes embeddings rather than text, where it says
    /// (Gemini's methods, Ollama's capabilities).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<bool>,
    /// The kind a draft of this model starts as: from its id and what the provider reported.
    pub suggested_kind: crate::model::ModelKind,
    /// The capability preset a draft of this model starts from.
    pub suggested_preset: String,
}

/// What a provider offers, compared with the repository (concept section 6.4).
#[derive(Debug, Serialize, JsonSchema)]
pub struct Discovery {
    /// Every model the provider lists now. Those without `model` are not in the repository:
    /// candidates for a draft. Nothing is created or activated by this call.
    pub listed: Vec<Discovered>,
    /// Repository models of this provider that the provider no longer lists.
    pub missing: Vec<String>,
}

/// One page of a provider's model list, in the provider's own shape.
async fn fetch_models(
    st: &AdminState,
    provider: &crate::snapshot::ProviderRt,
    url: &str,
) -> ApiResult<serde_json::Value> {
    fetch_json(st, provider, url, None).await
}

/// A provider's answer about its models: a GET, or a POST of `body` (Ollama's `/api/show`).
async fn fetch_json(
    st: &AdminState,
    provider: &crate::snapshot::ProviderRt,
    url: &str,
    body: Option<serde_json::Value>,
) -> ApiResult<serde_json::Value> {
    use http_body_util::BodyExt;
    let failed = |what: String| ApiError::new(StatusCode::BAD_GATEWAY, "provider_unreachable", what);
    let mut req = http::Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(url)
        .header("content-type", "application/json")
        .body(body.map_or_else(axum::body::Body::empty, |b| axum::body::Body::from(b.to_string())))
        .map_err(|e| failed(format!("the URL of the model list is not valid: {e}")))?;
    let scheme = provider.doc.managed_auth_scheme();
    if let Some(secret) = &provider.credentials.global {
        let header = crate::dataplane::common::auth_header(&st.gw.google, &st.upstream, scheme, Some(secret))
            .await
            .map_err(failed)?;
        if let Some((name, value)) = header {
            req.headers_mut().insert(name, value);
        }
    } else if scheme != crate::model::AuthScheme::None {
        return Err(ApiError::conflict("the provider has no shared credential to ask with"));
    }
    if provider.doc.family == crate::model::Family::Anthropic {
        req.headers_mut().insert("anthropic-version", http::HeaderValue::from_static("2023-06-01"));
    }
    let sent = st.upstream.send(req, std::time::Duration::from_secs(20)).await.map_err(|e| failed(e.to_string()))?;
    let status = sent.response.status();
    let body = http_body_util::Limited::new(sent.response.into_body(), 8 << 20)
        .collect()
        .await
        .map_err(|e| failed(format!("the provider's model list could not be read: {e}")))?
        .to_bytes();
    if !status.is_success() {
        return Err(failed(format!("the provider answered {status} to the request for its model list")));
    }
    serde_json::from_slice(&body).map_err(|e| failed(format!("the provider's model list is not JSON: {e}")))
}

/// Asks the provider which models it offers and compares the answer with the repository.
async fn discovery(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<Discovery>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let id = params.get("id").ok_or_else(|| ApiError::not_found("provider"))?;
    let provider = snap.providers.get(id).ok_or_else(|| ApiError::not_found("provider"))?;
    discover(&st, &snap, provider).await.map(Json)
}

/// What a provider lists, compared with the repository. Used on request, by the scheduled
/// discovery, and by the provider probe.
pub async fn discover(
    st: &AdminState,
    snap: &crate::snapshot::Snapshot,
    provider: &crate::snapshot::ProviderRt,
) -> ApiResult<Discovery> {
    use crate::model::Family;
    let base = provider.doc.base_url.trim_end_matches('/');
    let text = |v: &serde_json::Value, key: &str| v.get(key).and_then(|x| x.as_str()).map(str::to_owned);

    let mut listed: Vec<Discovered> = Vec::new();
    if provider.vertex.is_some() {
        // Vertex AI's model list is not read yet: its shape is unrecorded.
        return Err(ApiError::conflict(
            "discovery does not read Vertex AI's model list yet; add its models by their id, for example gemini-3.8-flash",
        ));
    }
    match provider.doc.family {
        Family::Anthropic => {
            // Anthropic pages its list and tells limits and names with each model.
            let mut after: Option<String> = None;
            for _ in 0..20 {
                let cursor = after.as_deref().map(|a| format!("&after_id={a}")).unwrap_or_default();
                let page = fetch_models(st, provider, &format!("{base}/v1/models?limit=1000{cursor}")).await?;
                for m in page["data"].as_array().into_iter().flatten() {
                    if let Some(upstream_id) = text(m, "id") {
                        listed.push(Discovered {
                            upstream_id,
                            model: None,
                            display_name: text(m, "display_name"),
                            context_window: m["max_input_tokens"].as_u64(),
                            max_output_tokens: m["max_tokens"].as_u64(),
                            vision: None,
                            reasons: None,
                            embedding: None,
                            suggested_kind: Default::default(),
                            suggested_preset: String::new(),
                        });
                    }
                }
                after = text(&page, "last_id").filter(|_| page["has_more"].as_bool() == Some(true));
                if after.is_none() {
                    break;
                }
            }
        }
        Family::Ollama => {
            let page = fetch_models(st, provider, &format!("{}/api/tags", provider.proxy_origin)).await?;
            for (n, m) in page["models"].as_array().into_iter().flatten().enumerate() {
                let Some(upstream_id) = text(m, "name") else { continue };
                // What a model can do and how long its context is, model by model: for the
                // first fifty, which is more than a server holds in practice.
                let shown = if n < 50 {
                    fetch_json(
                        st,
                        provider,
                        &format!("{}/api/show", provider.proxy_origin),
                        Some(serde_json::json!({ "model": upstream_id })),
                    )
                    .await
                    .ok()
                } else {
                    None
                };
                let capabilities: Vec<&str> = shown
                    .as_ref()
                    .and_then(|v| v["capabilities"].as_array())
                    .map(|c| c.iter().filter_map(serde_json::Value::as_str).collect())
                    .unwrap_or_default();
                let context = shown.as_ref().and_then(|v| v["model_info"].as_object()).and_then(|info| {
                    info.iter().find(|(k, _)| k.ends_with(".context_length")).and_then(|(_, v)| v.as_u64())
                });
                listed.push(Discovered {
                    upstream_id,
                    model: None,
                    display_name: None,
                    context_window: context,
                    max_output_tokens: None,
                    vision: shown.as_ref().map(|_| capabilities.contains(&"vision")),
                    reasons: shown.as_ref().map(|_| capabilities.contains(&"thinking")),
                    embedding: shown.as_ref().map(|_| capabilities.contains(&"embedding")),
                    suggested_kind: Default::default(),
                    suggested_preset: String::new(),
                });
            }
        }
        // Gemini's own API: `models[]` with names `models/<id>` and their limits, in pages.
        Family::Gemini if provider.doc.managed_api == crate::model::ManagedApi::Native => {
            let mut token: Option<String> = None;
            for _ in 0..20 {
                let url = match &token {
                    Some(t) => format!("{base}/models?pageSize=1000&pageToken={t}"),
                    None => format!("{base}/models?pageSize=1000"),
                };
                let page = fetch_models(st, provider, &url).await?;
                for m in page["models"].as_array().into_iter().flatten() {
                    // Chat models answer generateContent, embedding models embedContent; others
                    // (images, video, speech) are not for managed mode.
                    let methods = m["supportedGenerationMethods"].as_array();
                    let has = |name: &str| methods.is_none_or(|list| list.iter().any(|x| x == name));
                    let (generates, embeds) = (has("generateContent"), methods.is_some() && has("embedContent"));
                    if let (Some(name), true) = (text(m, "name"), generates || embeds) {
                        listed.push(Discovered {
                            upstream_id: name.trim_start_matches("models/").to_owned(),
                            model: None,
                            display_name: text(m, "displayName"),
                            context_window: m["inputTokenLimit"].as_u64(),
                            max_output_tokens: m["outputTokenLimit"].as_u64(),
                            vision: None,
                            reasons: m["thinking"].as_bool(),
                            embedding: Some(embeds && !generates),
                            suggested_kind: Default::default(),
                            suggested_preset: String::new(),
                        });
                    }
                }
                token = text(&page, "nextPageToken").filter(|t| !t.is_empty());
                if token.is_none() {
                    break;
                }
            }
        }
        // Cohere lists its models with the endpoints each serves, in pages; only rerank
        // models can be called through the gateway.
        Family::Cohere => {
            let mut token: Option<String> = None;
            for _ in 0..20 {
                let cursor = token.as_deref().map(|t| format!("&page_token={t}")).unwrap_or_default();
                let page =
                    fetch_models(st, provider, &format!("{base}/v1/models?endpoint=rerank&page_size=1000{cursor}"))
                        .await?;
                for m in page["models"].as_array().into_iter().flatten() {
                    if let Some(upstream_id) = text(m, "name") {
                        listed.push(Discovered {
                            upstream_id,
                            model: None,
                            display_name: None,
                            context_window: m["context_length"].as_u64(),
                            max_output_tokens: None,
                            vision: None,
                            reasons: None,
                            embedding: None,
                            suggested_kind: Default::default(),
                            suggested_preset: String::new(),
                        });
                    }
                }
                token = text(&page, "next_page_token").filter(|t| !t.is_empty());
                if token.is_none() {
                    break;
                }
            }
        }
        // TypeSafe lists the aliases of its decision models; versioned ids are accepted
        // whether listed or not.
        Family::Typesafe => {
            let page = fetch_models(st, provider, &format!("{base}/models")).await?;
            for m in page["data"].as_array().into_iter().flatten() {
                if let Some(upstream_id) = text(m, "id") {
                    listed.push(Discovered {
                        upstream_id,
                        model: None,
                        display_name: None,
                        context_window: None,
                        max_output_tokens: None,
                        vision: None,
                        reasons: None,
                        embedding: None,
                        suggested_kind: Default::default(),
                        suggested_preset: String::new(),
                    });
                }
            }
        }
        Family::Openai | Family::Gemini => {
            let page = fetch_models(st, provider, &format!("{base}/models")).await?;
            for m in page["data"].as_array().into_iter().flatten() {
                // Gemini's compatible endpoint names its models `models/<id>`; calls take the id.
                let Some(upstream_id) = text(m, "id").map(|id| id.trim_start_matches("models/").to_owned()) else {
                    continue;
                };
                // What some providers add to the list (recorded: DeepSeek; documented: Kimi).
                let number = |keys: &[&str]| keys.iter().find_map(|k| m[*k].as_u64());
                let vision = m["input_modalities"]
                    .as_array()
                    .map(|list| list.iter().any(|x| x == "image"))
                    .or_else(|| m["supports_image_in"].as_bool());
                let reasons = m["supports_reasoning"]
                    .as_bool()
                    .or_else(|| m.get("effort").filter(|e| e.is_object()).map(|_| true));
                listed.push(Discovered {
                    upstream_id,
                    model: None,
                    display_name: text(m, "display_name").or_else(|| text(m, "name")),
                    context_window: number(&["context_window", "context_length", "max_input_tokens"]),
                    max_output_tokens: number(&["max_output_tokens"]),
                    vision,
                    reasons,
                    embedding: None,
                    suggested_kind: Default::default(),
                    suggested_preset: String::new(),
                });
            }
        }
    }

    for d in &mut listed {
        let (kind, preset) =
            crate::model::repo::suggestion(provider.doc.family, &d.upstream_id, d.vision, d.reasons, d.embedding);
        (d.suggested_kind, d.suggested_preset) = (kind, preset.to_owned());
    }
    let mut missing = Vec::new();
    for m in snap.models.values().filter(|m| m.doc.provider_id == provider.id) {
        match listed.iter_mut().find(|d| d.upstream_id == m.doc.upstream_id) {
            Some(d) => d.model = Some(m.doc.name.clone()),
            // A retired model is expected to be gone.
            None if m.doc.state != crate::model::ModelState::Retired => missing.push(m.doc.name.clone()),
            None => {}
        }
    }
    listed.sort_by(|a, b| a.upstream_id.cmp(&b.upstream_id));
    missing.sort();
    Ok(Discovery { listed, missing })
}

/// Whether a provider answers with its credential: the outcome of asking for its model list.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ProviderProbe {
    pub ok: bool,
    pub detail: String,
    pub latency_ms: u64,
    /// How many models the provider lists, when it answered.
    pub models: Option<usize>,
}

/// `POST /providers/{id}/probe`: asks the provider for its model list with the shared
/// credential, which tests the address, the network path and the credential at once.
async fn probe(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<ProviderProbe>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let id = params.get("id").ok_or_else(|| ApiError::not_found("provider"))?;
    let provider = snap.providers.get(id).ok_or_else(|| ApiError::not_found("provider"))?;
    let started = std::time::Instant::now();
    let result = discover(&st, &snap, provider).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    Ok(Json(match result {
        Ok(d) => ProviderProbe {
            ok: true,
            detail: format!("the provider lists {} models", d.listed.len()),
            latency_ms,
            models: Some(d.listed.len()),
        },
        Err(e) => ProviderProbe { ok: false, detail: e.message, latency_ms, models: None },
    }))
}

async fn presets(_p: Principal) -> Json<Vec<ProviderPreset>> {
    Json(provider_presets())
}

pub fn routes(mut router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    router = register::<Provider>(router, api, Ops::ALL);
    let preset = api.schema::<ProviderPreset>();
    api.add(
        api.op(
            "get",
            "/provider-presets",
            "providerPresets",
            "providers",
            "Starting points for the providers the gateway knows",
        )
        .ok(serde_json::json!({ "type": "array", "items": preset })),
    );

    let coll = "/providers/{id}/credentials";
    let one = "/providers/{id}/credentials/{cid}";
    let v = api.schema::<Item<CredentialView>>();
    let input = api.schema::<CredentialInput>();
    let state_input = api.schema::<CredentialStateInput>();
    api.add(
        api.op("get", coll, "listCredentials", "credentials", "List a provider's credentials (no secrets)")
            .query("all", "Set to false to list only current credentials.")
            .query("cursor", "Continue a listing: the `next_cursor` of the previous page.")
            .query_int("limit", "Page size, 1 to 500. Default 100.")
            .ok(page_schema(v.clone())),
    );
    api.add(
        api.op("post", coll, "createCredential", "credentials", "Add or rotate a provider key")
            .body(input)
            .created(v.clone()),
    );
    api.add(
        api.op(
            "post",
            &format!("{one}/state"),
            "setCredentialState",
            "credentials",
            "Make a credential current or disable it",
        )
        .body(state_input)
        .ok(v.clone()),
    );
    api.add(api.op("delete", one, "deleteCredential", "credentials", "Delete a credential").no_content());
    let copy_input = api.schema::<CredentialCopyInput>();
    api.add(
        api.op(
            "post",
            &format!("{one}/copy"),
            "copyCredential",
            "credentials",
            "Give an organisation or a workspace a stored key, without entering it again",
        )
        .body(copy_input)
        .created(v),
    );

    let found = api.schema::<Discovery>();
    api.add(
        api.op(
            "get",
            "/providers/{id}/discovery",
            "discoverModels",
            "providers",
            "Ask the provider which models it offers",
        )
        .ok(found),
    );

    let probed = api.schema::<ProviderProbe>();
    api.add(
        api.op(
            "post",
            "/providers/{id}/probe",
            "probeProvider",
            "providers",
            "Test the address and the shared credential by asking for the model list",
        )
        .ok(probed),
    );

    router
        .route("/provider-presets", get(presets))
        .route("/providers/{id}/discovery", get(discovery))
        .route("/providers/{id}/probe", post(probe))
        .route(coll, get(list_credentials).post(create_credential))
        .route(one, delete(delete_credential))
        .route(&format!("{one}/state"), post(set_credential_state))
        .route(&format!("{one}/copy"), post(copy_credential))
}
