//! Organisations, workspaces and API keys.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
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
    ApiKey, Doc, InspectMode, KeyStatus, Mode, Obj, OrgConfigTemplate, Organisation, RoleScope, SYSTEM_NAME,
    SYSTEM_ORG_ID, SYSTEM_WS_ID, Status, UsagePolicy, Workspace, is_slug, new_id, org_config_template,
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
    if let Some(parties) = &s.plugin_third_parties
        && parties.iter().any(|t| !crate::model::is_slug(t))
    {
        return Err(ApiError::invalid(
            "settings.plugin_third_parties holds the names of third parties, such as typesafe",
        ));
    }
    if s.usage_retention_days.is_some_and(|d| d > 36_500) {
        return Err(ApiError::invalid("settings.usage_retention_days must be at most 36500; 0 keeps every event"));
    }
    if let Some(keys) = &s.tag_keys {
        if !organisation {
            return Err(ApiError::invalid(
                "settings.tag_keys belongs to an organisation; a workspace declares tag_keys",
            ));
        }
        let ok = |k: &String| {
            !k.is_empty()
                && k.len() <= 32
                && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        };
        if keys.len() > 8 || !keys.iter().all(ok) {
            return Err(ApiError::invalid(
                "settings.tag_keys holds up to eight tag keys of lower-case letters, digits and underscores",
            ));
        }
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
        // The built-in organisation keeps its name and stays enabled.
        if ctx.id == SYSTEM_ORG_ID {
            input.name = SYSTEM_NAME.into();
            input.status = Status::Active;
            check_settings(&input.settings, true, ctx.snap.organisations.get(ctx.id).map(|o| &**o))?;
            return Ok(input);
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

    fn visible(p: &Principal, obj: &Obj<Self>, _snap: &Snapshot) -> bool {
        p.sees_org(&obj.id)
    }

    fn can_replace(p: &Principal, _scope: &ScopeIds, id: &str, _snap: &Snapshot) -> bool {
        p.can_admin_org(id)
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        if obj.id == SYSTEM_ORG_ID {
            return Err(ApiError::conflict("the built-in _system organisation cannot be deleted"));
        }
        if tx.count_children(&obj.id).await? > 0 {
            return Err(ApiError::conflict(
                "the organisation still has workspaces or other objects; remove them first",
            ));
        }
        tx.invitations_delete_for_scope(&obj.id, None).await?;
        delete_bindings(tx, "organisation deleted", |b| b.scope == RoleScope::Organisation(obj.id.clone())).await
    }

    /// pacod-id learns of the organisation in step (OIDC mode): its roles and invitations hang
    /// on the scope `organisation:<id>`.
    async fn in_tx(st: &AdminState, tx: &mut Tx, id: &str, _parent: Option<&str>, doc: Option<&Self>) -> ApiResult<()> {
        if st.identity.is_none() {
            return Ok(());
        }
        let target = format!("organisation:{id}");
        match doc {
            Some(o) => {
                let payload = serde_json::json!({
                    "id": id, "slug": o.name, "display_name": o.display_name, "system": id == SYSTEM_ORG_ID,
                });
                tx.idp_enqueue("put_organisation", &target, &payload).await?;
            }
            None => tx.idp_enqueue("delete_organisation", &target, &serde_json::json!({ "id": id })).await?,
        }
        Ok(())
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
        if ctx.id == SYSTEM_WS_ID {
            input.name = SYSTEM_NAME.into();
            input.status = Status::Active;
        } else if !is_slug(&input.name) {
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

    fn can_read(p: &Principal, scope: &ScopeIds, _snap: &Snapshot) -> bool {
        p.sees_org(scope.org())
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
        if obj.id == SYSTEM_WS_ID {
            return Err(ApiError::conflict("the built-in _system workspace cannot be deleted"));
        }
        if tx.count_children(&obj.id).await? > 0 {
            return Err(ApiError::conflict("the workspace still has keys or other objects; remove them first"));
        }
        tx.invitations_delete_for_scope(obj.parent.as_deref().unwrap_or_default(), Some(&obj.id)).await?;
        delete_bindings(tx, "workspace deleted", |b| b.scope == RoleScope::Workspace(obj.id.clone())).await
    }

    /// pacod-id learns of the workspace in step (OIDC mode): `gw.workspace:<id>` under its
    /// organisation.
    async fn in_tx(st: &AdminState, tx: &mut Tx, id: &str, parent: Option<&str>, doc: Option<&Self>) -> ApiResult<()> {
        if st.identity.is_none() {
            return Ok(());
        }
        let target = format!("gw.workspace:{id}");
        match doc {
            Some(w) => {
                let payload = serde_json::json!({
                    "kind": "gw.workspace", "id": id, "parent": format!("organisation:{}", parent.unwrap_or_default()),
                    "display_name": w.display_name, "system": id == SYSTEM_WS_ID,
                });
                tx.idp_enqueue("put_scope", &target, &payload).await?;
            }
            None => {
                tx.idp_enqueue("delete_scope", &target, &serde_json::json!({ "kind": "gw.workspace", "id": id }))
                    .await?
            }
        }
        Ok(())
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
    /// Unset: automatic, `require` while an enforced token or spend limit applies to the
    /// workspace, `estimate` otherwise.
    #[serde(default)]
    pub usage_policy: Option<UsagePolicy>,
    /// The rate class whose throttles apply to this key, by name. Unset: none.
    #[serde(default)]
    pub rate_class: Option<String>,
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
    /// Unset: automatic.
    pub usage_policy: Option<UsagePolicy>,
    pub rate_class: Option<String>,
    pub inspect: InspectMode,
    pub max_concurrency: Option<u32>,
    pub expires_at: Option<DateTime<Utc>>,
    pub status: KeyStatus,
    /// After a rotation: until when the previous secret still works.
    pub previous_secret_valid_until: Option<DateTime<Utc>>,
}

fn validate_key(input: &KeyInput, snap: &Snapshot) -> ApiResult<()> {
    if let Some(class) = input.rate_class.as_deref().filter(|c| !c.is_empty())
        && !snap.rate_classes.contains_key(class)
    {
        return Err(ApiError::invalid(format!("there is no rate class \"{class}\"")));
    }
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
        && (!input.allowed_models.is_empty()
            || input.usage_policy != Some(UsagePolicy::Ignore)
            || input.require_template)
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

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        validate_key(&input, ctx.snap)?;
        // Replace keeps the secret and status; only the settings change.
        let existing = existing.ok_or_else(|| ApiError::internal("API keys are created through their own endpoint"))?;
        Ok(ApiKey {
            name: input.name.trim().to_owned(),
            allowed_modes: dedup(input.allowed_modes),
            allowed_models: input.allowed_models,
            allowed_templates: input.allowed_templates,
            require_template: input.require_template,
            usage_policy: input.usage_policy,
            rate_class: input.rate_class.filter(|c| !c.is_empty()),
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
            rate_class: d.rate_class.clone(),
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
    validate_key(&input, &snap)?;
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
        rate_class: input.rate_class.filter(|c| !c.is_empty()),
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
    Query(dry): Query<crate::mgmt::crud::DryRunQuery>,
) -> ApiResult<axum::response::Response> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !ApiKey::can_write(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("API key"))?;
    let existing = load::<ApiKey>(&st, id, &scope).await?;
    if dry.dry_run == Some(true) {
        let mut would = existing.clone();
        would.doc.status = KeyStatus::Revoked;
        return Ok(Json(crate::mgmt::crud::DryRun {
            applied: false,
            object: Item { meta: would.meta(), data: ApiKey::view(&would) },
            effects: vec![format!(
                "calls with the key \"{}\" are refused within two seconds on every instance; a revoked key cannot be restored",
                existing.doc.name
            )],
        })
        .into_response());
    }
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
    Ok(Json(Item { meta: obj.meta(), data: ApiKey::view(&obj) }).into_response())
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
    api.add(
        api.op("post", &format!("{keys}/{{id}}/revoke"), "revokeApiKey", "keys", "Revoke the key")
            .query("dry_run", "true: answer what revoking would do, without revoking.")
            .ok(key_view),
    );
    router
        .route("/org-config-template", get(config_template))
        .route(&keys, post(create_key))
        .route(&format!("{keys}/{{id}}/rotate"), post(rotate_key))
        .route(&format!("{keys}/{{id}}/revoke"), post(revoke_key))
}
