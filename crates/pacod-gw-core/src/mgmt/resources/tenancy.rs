//! Organisations, workspaces and API keys.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crypto::keys::{API_KEY_PREFIX, generate};
use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{
    BuildCtx, Item, Ops, Params, Resource, ScopeIds, ScopeKind, collection_path, load, register, resolve_scope,
};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::mgmt::resources::access::delete_bindings;
use crate::model::tenancy::all_modes;
use crate::model::{
    ApiKey, Doc, InspectMode, KeyStatus, Mode, Obj, OrgConfigTemplate, Organisation, RoleScope, UsagePolicy, Workspace,
    is_slug, new_id, org_config_template,
};
use crate::snapshot::Snapshot;
use crate::store::Tx;

/// Checks the settings of an organisation or a workspace. `org` is the organisation they
/// belong to, unset for one being created.
fn check_settings(
    s: &crate::model::ScopeSettings,
    organisation: bool,
    org: Option<&crate::snapshot::OrgRt>,
) -> ApiResult<()> {
    if let Some(name) = &s.routing_scheme
        && !org.is_some_and(|o| o.schemes.contains_key(name))
    {
        return Err(ApiError::invalid(format!(
            "settings.routing_scheme: the organisation has no routing scheme \"{name}\""
        ))
        .with_details(serde_json::json!({ "code": "routing_scheme_unknown" })));
    }
    if !organisation && !s.locked.is_empty() {
        return Err(ApiError::invalid(
            "settings.locked belongs to an organisation: it names what its workspaces may not override",
        ));
    }
    if let Some(tools) = &s.provider_tools
        && tools.iter().any(|t| t.trim().is_empty() || t.len() > 64)
    {
        return Err(ApiError::invalid("settings.provider_tools holds tool names such as web_search"));
    }
    if let Some(a) = &s.audit {
        if a.sample.is_some_and(|x| !(x > 0.0 && x <= 1.0)) {
            return Err(ApiError::invalid("settings.audit.sample must be above 0 and at most 1"));
        }
        if a.retention_days.is_some_and(|d| !(1..=3650).contains(&d)) {
            return Err(ApiError::invalid("settings.audit.retention_days must be between 1 and 3650"));
        }
        if !organisation && a.rule.is_some() {
            return Err(ApiError::invalid(
                "settings.audit.rule belongs to an organisation: it decides for its workspaces",
            ));
        }
    }
    Ok(())
}

// --- organisations ---------------------------------------------------------------------------

impl Resource for Organisation {
    const PLURAL: &'static str = "orgs";
    const LABEL: &'static str = "organisation";
    const SCOPE: ScopeKind = ScopeKind::Global;
    const ID_PARAM: &'static str = "org";
    type Input = Organisation;
    type View = Organisation;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(mut input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        // The organisation's admins keep everything but its name and whether it is enabled:
        // those are the platform's to decide.
        if let Some(e) = existing
            && !ctx.platform_admin
        {
            input.name = e.doc.name.clone();
            input.status = e.doc.status;
        }
        if !is_slug(&input.name) {
            return Err(ApiError::invalid("name must be lower-case letters, digits and hyphens"));
        }
        if input.display_name.trim().is_empty() {
            input.display_name = input.name.clone();
        }
        check_settings(&input.settings, true, ctx.snap.organisations.get(ctx.id).map(|o| &**o))?;
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }

    fn visible(p: &Principal, obj: &Obj<Self>, snap: &Snapshot) -> bool {
        p.sees_org(&obj.id, snap)
    }

    fn can_replace(p: &Principal, _scope: &ScopeIds, id: &str, _snap: &Snapshot) -> bool {
        p.can_admin_org(id)
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        if tx.count_children(&obj.id).await? > 0 {
            return Err(ApiError::conflict(
                "the organisation still has workspaces or other objects; remove them first",
            ));
        }
        tx.invitations_delete_for_scope(&obj.id, None).await?;
        delete_bindings(tx, "organisation deleted", |b| b.scope == RoleScope::Organisation(obj.id.clone())).await
    }
}

// --- workspaces ------------------------------------------------------------------------------

impl Resource for Workspace {
    const PLURAL: &'static str = "workspaces";
    const LABEL: &'static str = "workspace";
    const SCOPE: ScopeKind = ScopeKind::Organisation;
    const ID_PARAM: &'static str = "ws";
    type Input = Workspace;
    type View = Workspace;

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
        if input.tag_keys.len() > 8 {
            return Err(ApiError::invalid("a workspace can declare at most eight tag keys"));
        }
        for k in &input.tag_keys {
            let ok = !k.is_empty()
                && k.len() <= 32
                && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !ok {
                return Err(ApiError::invalid(format!(
                    "tag key \"{k}\" must be lower-case letters, digits and underscores, at most 32 long"
                )));
            }
        }
        input.tag_keys.sort();
        input.tag_keys.dedup();
        if input.time_zone.parse::<chrono_tz::Tz>().is_err() {
            return Err(ApiError::invalid(format!("\"{}\" is not an IANA time zone", input.time_zone)));
        }
        check_settings(&input.settings, false, ctx.snap.organisations.get(ctx.scope.org()).map(|o| &**o))?;
        Ok(input)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        obj.doc.clone()
    }

    fn can_read(p: &Principal, scope: &ScopeIds, snap: &Snapshot) -> bool {
        p.sees_org(scope.org(), snap)
    }

    fn visible(p: &Principal, obj: &Obj<Self>, _snap: &Snapshot) -> bool {
        p.can_read_workspace(&obj.id, obj.parent.as_deref().unwrap_or_default())
    }

    /// Creating and deleting workspaces is the organisation's business; a workspace admin
    /// keeps the settings of the workspace they administer.
    fn can_replace(p: &Principal, scope: &ScopeIds, id: &str, _snap: &Snapshot) -> bool {
        p.can_admin_workspace(id, scope.org())
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        if tx.count_children(&obj.id).await? > 0 {
            return Err(ApiError::conflict("the workspace still has keys or other objects; remove them first"));
        }
        tx.invitations_delete_for_scope(obj.parent.as_deref().unwrap_or_default(), Some(&obj.id)).await?;
        delete_bindings(tx, "workspace deleted", |b| b.scope == RoleScope::Workspace(obj.id.clone())).await
    }
}

// --- API keys --------------------------------------------------------------------------------

/// What can be set on an API key. The secret is generated by the gateway.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct KeyInput {
    pub name: String,
    #[serde(default = "all_modes")]
    pub allowed_modes: Vec<Mode>,
    /// Model ids this key may use. Empty means every model the workspace may use.
    #[serde(default)]
    pub allowed_models: Vec<String>,
    /// Template names this key may use. Empty means every template of the workspace.
    #[serde(default)]
    pub allowed_templates: Vec<String>,
    /// Proxy mode: accept only requests that name a template.
    #[serde(default)]
    pub require_template: bool,
    #[serde(default)]
    pub usage_policy: UsagePolicy,
    #[serde(default)]
    pub inspect: InspectMode,
    /// The most calls this key may have in progress at once, across all gateway instances.
    /// Each of N running gateways allows `ceil(max / N)` of them, so the cap is approximate,
    /// and one below N allows N. Above it the gateway answers `429 rate_limited`.
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// An API key as the API returns it. The secret is not part of it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct KeyView {
    pub name: String,
    /// The last four characters of the secret.
    pub hint: String,
    pub allowed_modes: Vec<Mode>,
    pub allowed_models: Vec<String>,
    pub allowed_templates: Vec<String>,
    pub require_template: bool,
    pub usage_policy: UsagePolicy,
    pub inspect: InspectMode,
    pub max_concurrency: Option<u32>,
    pub expires_at: Option<DateTime<Utc>>,
    pub status: KeyStatus,
    /// After a rotation: until when the previous secret still works.
    pub previous_secret_valid_until: Option<DateTime<Utc>>,
}

fn validate_key(input: &KeyInput) -> ApiResult<()> {
    if input.name.trim().is_empty() || input.name.len() > 100 {
        return Err(ApiError::invalid("name must be 1 to 100 characters"));
    }
    if input.allowed_modes.is_empty() {
        return Err(ApiError::invalid("allowed_modes must name at least one mode"));
    }
    if input.max_concurrency == Some(0) {
        return Err(ApiError::invalid("max_concurrency must be at least 1; revoke the key to stop it"));
    }
    if input.inspect == InspectMode::None
        && (!input.allowed_models.is_empty() || input.usage_policy != UsagePolicy::Ignore || input.require_template)
    {
        return Err(ApiError::invalid(
            "blind relay (inspect: none) needs usage_policy: ignore and cannot be combined with a model restriction or a required template",
        ));
    }
    Ok(())
}

impl Resource for ApiKey {
    const PLURAL: &'static str = "keys";
    const LABEL: &'static str = "API key";
    const SCOPE: ScopeKind = ScopeKind::Workspace;
    type Input = KeyInput;
    type View = KeyView;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, _ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        validate_key(&input)?;
        // Replace keeps the secret and status; only the settings change.
        let existing = existing.ok_or_else(|| ApiError::internal("API keys are created through their own endpoint"))?;
        Ok(ApiKey {
            name: input.name.trim().to_owned(),
            allowed_modes: dedup(input.allowed_modes),
            allowed_models: input.allowed_models,
            allowed_templates: input.allowed_templates,
            require_template: input.require_template,
            usage_policy: input.usage_policy,
            inspect: input.inspect,
            max_concurrency: input.max_concurrency,
            expires_at: input.expires_at,
            ..existing.doc.clone()
        })
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        let d = &obj.doc;
        KeyView {
            name: d.name.clone(),
            hint: d.hint.clone(),
            allowed_modes: d.allowed_modes.clone(),
            allowed_models: d.allowed_models.clone(),
            allowed_templates: d.allowed_templates.clone(),
            require_template: d.require_template,
            usage_policy: d.usage_policy,
            inspect: d.inspect,
            max_concurrency: d.max_concurrency,
            expires_at: d.expires_at,
            status: d.status,
            previous_secret_valid_until: d.prev_valid_until.filter(|t| *t > Utc::now()),
        }
    }
}

fn dedup(mut modes: Vec<Mode>) -> Vec<Mode> {
    let mut seen = Vec::new();
    modes.retain(|m| {
        let new = !seen.contains(m);
        seen.push(*m);
        new
    });
    modes
}

/// A key together with its secret. Returned once, when the key is created or rotated.
#[derive(Debug, Serialize, JsonSchema)]
pub struct KeyWithSecret {
    #[serde(flatten)]
    pub key: Item<KeyView>,
    /// The full key. It cannot be shown again.
    pub secret: String,
}

fn hint_of(secret: &str) -> String {
    secret.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect()
}

async fn create_key(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<KeyInput>,
) -> ApiResult<(StatusCode, Json<KeyWithSecret>)> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !ApiKey::can_write(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    validate_key(&input)?;
    let id = new_id(ApiKey::PREFIX);
    let secret = generate(API_KEY_PREFIX, id.trim_start_matches("key_")).full;
    let doc = ApiKey {
        name: input.name.trim().to_owned(),
        secret_hmac: hex::encode(st.boot.pepper.hash(&secret)),
        prev_hmac: None,
        prev_valid_until: None,
        hint: hint_of(&secret),
        allowed_modes: dedup(input.allowed_modes),
        allowed_models: input.allowed_models,
        allowed_templates: input.allowed_templates,
        require_template: input.require_template,
        usage_policy: input.usage_policy,
        inspect: input.inspect,
        max_concurrency: input.max_concurrency,
        expires_at: input.expires_at,
        status: KeyStatus::Active,
    };
    let mut tx = st.store.begin().await?;
    let row = tx
        .create(
            ApiKey::KIND,
            &id,
            scope.ws.as_deref(),
            Some(&doc.name),
            &serde_json::to_value(&doc).map_err(ApiError::internal)?,
            p.audit(scope.org.as_deref(), scope.ws.as_deref()),
        )
        .await?;
    st.commit(tx).await?;
    let obj: Obj<ApiKey> = row.into_obj()?;
    Ok((StatusCode::CREATED, Json(KeyWithSecret { key: Item { meta: obj.meta(), data: ApiKey::view(&obj) }, secret })))
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RotateInput {
    /// How long the previous secret keeps working, in hours. Default 24, at most 720.
    #[serde(default)]
    pub grace_hours: Option<u32>,
}

async fn rotate_key(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    body: Option<Json<RotateInput>>,
) -> ApiResult<Json<KeyWithSecret>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !ApiKey::can_write(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("API key"))?;
    let existing = load::<ApiKey>(&st, id, &scope).await?;
    if existing.doc.status != KeyStatus::Active {
        return Err(ApiError::conflict("a revoked key cannot be rotated; create a new one"));
    }
    let grace = body.map(|b| b.0).unwrap_or_default().grace_hours.unwrap_or(24).min(720);
    let secret = generate(API_KEY_PREFIX, id.trim_start_matches("key_")).full;
    let mut doc = existing.doc.clone();
    doc.prev_hmac = (grace > 0).then(|| doc.secret_hmac.clone());
    doc.prev_valid_until = (grace > 0).then(|| Utc::now() + Duration::hours(i64::from(grace)));
    doc.secret_hmac = hex::encode(st.boot.pepper.hash(&secret));
    doc.hint = hint_of(&secret);

    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(scope.org.as_deref(), scope.ws.as_deref());
    audit.action = "rotate".into();
    let row =
        tx.update(id, None, Some(&doc.name), &serde_json::to_value(&doc).map_err(ApiError::internal)?, audit).await?;
    st.commit(tx).await?;
    let obj: Obj<ApiKey> = row.into_obj()?;
    Ok(Json(KeyWithSecret { key: Item { meta: obj.meta(), data: ApiKey::view(&obj) }, secret }))
}

async fn revoke_key(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<Item<KeyView>>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !ApiKey::can_write(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("API key"))?;
    let existing = load::<ApiKey>(&st, id, &scope).await?;
    let mut doc = existing.doc.clone();
    doc.status = KeyStatus::Revoked;
    doc.prev_hmac = None;
    doc.prev_valid_until = None;
    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(scope.org.as_deref(), scope.ws.as_deref());
    audit.action = "revoke".into();
    let row =
        tx.update(id, None, Some(&doc.name), &serde_json::to_value(&doc).map_err(ApiError::internal)?, audit).await?;
    st.commit(tx).await?;
    let obj: Obj<ApiKey> = row.into_obj()?;
    Ok(Json(Item { meta: obj.meta(), data: ApiKey::view(&obj) }))
}

async fn config_template(_p: Principal) -> Json<OrgConfigTemplate> {
    Json(org_config_template())
}

pub fn routes(mut router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    router = register::<Organisation>(router, api, Ops::ALL);
    router = register::<Workspace>(router, api, Ops::ALL);
    router = register::<ApiKey>(router, api, Ops::NO_CREATE);

    let template = api.schema::<OrgConfigTemplate>();
    api.add(
        api.op(
            "get",
            "/org-config-template",
            "orgConfigTemplate",
            "orgs",
            "A starting configuration for an organisation: settings and routing schemes",
        )
        .ok(template),
    );

    let keys = collection_path(ScopeKind::Workspace, ApiKey::PLURAL);
    let with_secret = api.schema::<KeyWithSecret>();
    let key_input = api.schema::<KeyInput>();
    let key_view = api.schema::<Item<KeyView>>();
    let rotate_input = api.schema::<RotateInput>();
    api.add(
        api.op("post", &keys, "createApiKey", "keys", "Create an API key. The secret is returned once.")
            .body(key_input)
            .created(with_secret.clone()),
    );
    api.add(
        api.op("post", &format!("{keys}/{{id}}/rotate"), "rotateApiKey", "keys", "Give the key a new secret")
            .body(rotate_input)
            .ok(with_secret),
    );
    api.add(api.op("post", &format!("{keys}/{{id}}/revoke"), "revokeApiKey", "keys", "Revoke the key").ok(key_view));
    router
        .route("/org-config-template", get(config_template))
        .route(&keys, post(create_key))
        .route(&format!("{keys}/{{id}}/rotate"), post(rotate_key))
        .route(&format!("{keys}/{{id}}/revoke"), post(revoke_key))
}
