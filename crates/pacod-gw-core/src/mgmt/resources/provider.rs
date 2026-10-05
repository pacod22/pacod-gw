//! Providers and their credentials.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crypto::envelope::{fingerprint, seal};
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

    fn build(mut input: Self::Input, _existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
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
        Ok(input)
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
    let id = new_id(Credential::PREFIX);
    let kek = st.boot.keks.first().ok_or_else(|| ApiError::internal("no key-encryption key"))?;
    let doc = Credential {
        provider_id: provider.id.clone(),
        scope: input.scope.clone(),
        label: input.label.trim().to_owned(),
        // The credential id binds the ciphertext to this record.
        sealed: seal(kek, &id, secret.as_bytes()),
        fingerprint: fingerprint(secret),
        state: if input.make_current { CredentialState::Current } else { CredentialState::Retiring },
        retire_at: None,
    };
    let (org, ws) = scope_ids(&input.scope, &snap);
    let grace = Duration::hours(i64::from(input.grace_hours.unwrap_or(24).min(720)));

    let mut tx = st.store.begin().await?;
    if input.make_current {
        // The credential this one replaces is kept for a rollback.
        for row in tx.list(Credential::KIND, Some(&provider.id)).await? {
            let mut old: Obj<Credential> = row.into_obj()?;
            if old.doc.scope == input.scope && old.doc.state == CredentialState::Current {
                old.doc.state = CredentialState::Retiring;
                old.doc.retire_at = Some(Utc::now() + grace);
                let mut audit = p.audit(org.as_deref(), ws.as_deref());
                audit.action = "retire".into();
                tx.update(&old.id, None, None, &serde_json::to_value(&old.doc).map_err(ApiError::internal)?, audit)
                    .await?;
            }
        }
    }
    let row = tx
        .create(
            Credential::KIND,
            &id,
            Some(&provider.id),
            None,
            &serde_json::to_value(&doc).map_err(ApiError::internal)?,
            p.audit(org.as_deref(), ws.as_deref()),
        )
        .await?;
    st.commit(tx).await?;
    Ok((StatusCode::CREATED, Json(view(&row.into_obj()?))))
}

#[derive(Debug, Default, Deserialize)]
struct CredentialQuery {
    /// Include disabled and retiring credentials. Default true.
    all: Option<bool>,
}

async fn list_credentials(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Query(q): Query<CredentialQuery>,
) -> ApiResult<Json<Page<Item<CredentialView>>>> {
    let snap = st.config.snapshot();
    let provider = provider_of(&st, &params).await?;
    let rows = st.store.list(Credential::KIND, crate::store::ParentFilter::Is(&provider.id), None, 500).await?;
    let mut items = Vec::new();
    for row in rows {
        let obj: Obj<Credential> = row.into_obj()?;
        if !can_manage(&p, &obj.doc.scope, &snap) {
            continue;
        }
        if q.all == Some(false) && obj.doc.state != CredentialState::Current {
            continue;
        }
        items.push(view(&obj));
    }
    Ok(Json(Page { items, next_cursor: None }))
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
        for row in tx.list(Credential::KIND, Some(&obj.doc.provider_id)).await? {
            let mut other: Obj<Credential> = row.into_obj()?;
            if other.id != obj.id && other.doc.scope == obj.doc.scope && other.doc.state == CredentialState::Current {
                other.doc.state = CredentialState::Retiring;
                other.doc.retire_at = Some(Utc::now() + Duration::hours(24));
                let mut audit = p.audit(org.as_deref(), ws.as_deref());
                audit.action = "retire".into();
                tx.update(&other.id, None, None, &serde_json::to_value(&other.doc).map_err(ApiError::internal)?, audit)
                    .await?;
            }
        }
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
        .ok(v),
    );
    api.add(api.op("delete", one, "deleteCredential", "credentials", "Delete a credential").no_content());

    router
        .route("/provider-presets", get(presets))
        .route(coll, get(list_credentials).post(create_credential))
        .route(one, delete(delete_credential))
        .route(&format!("{one}/state"), post(set_credential_state))
}
