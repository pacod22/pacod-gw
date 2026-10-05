//! Workspace templates (concept section 7).

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mgmt::AdminState;
use crate::mgmt::auth::Principal;
use crate::mgmt::crud::{
    BuildCtx, Item, Ops, Params, Resource, ScopeKind, collection_path, load, register, resolve_scope,
};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use crate::model::template::MAX_TEMPLATE_VERSIONS;
use crate::model::{
    ApiKey, Doc, ModelState, Obj, Status, Template, TemplateSpec, TemplateVersion, UnsupportedParams, is_slug, new_id,
};
use crate::params::{CallerParams, MergeContext, merge};
use crate::snapshot::{GrantTerms, Snapshot, WorkspaceRt};
use crate::store::Tx;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TemplateInput {
    /// Slug, unique in the workspace. Applications address the template as `@name`.
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub spec: TemplateSpec,
    /// A note on what this version changes.
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub status: Status,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct VersionInfo {
    pub version: u32,
    pub comment: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TemplateView {
    pub name: String,
    pub description: String,
    pub status: Status,
    /// The version calls use.
    pub current_version: u32,
    /// The configuration of the current version.
    pub spec: TemplateSpec,
    /// Newest first.
    pub versions: Vec<VersionInfo>,
}

/// Checks a spec against the workspace and normalises the model reference to the model's id.
fn check_spec(
    snap: &Snapshot,
    ws: &WorkspaceRt,
    spec: &mut TemplateSpec,
    previous: Option<&TemplateSpec>,
) -> ApiResult<Vec<String>> {
    let model_id = snap
        .model_names
        .get(&spec.model)
        .cloned()
        .or_else(|| snap.models.contains_key(&spec.model).then(|| spec.model.clone()))
        .ok_or_else(|| {
            ApiError::invalid(format!("unknown model \"{}\"", spec.model))
                .with_details(serde_json::json!({ "code": "unknown_model" }))
        })?;
    let model = &snap.models[&model_id];
    spec.model = model.doc.name.clone();
    let terms = snap.grant_terms(ws, &model_id).ok_or_else(|| {
        ApiError::invalid(format!("the model \"{}\" is not granted to this workspace", model.doc.name))
            .with_details(serde_json::json!({ "code": "model_not_granted" }))
    })?;
    // A template keeps a model it already uses; a new choice must be an active model.
    let unchanged = previous.is_some_and(|p| p.model == spec.model);
    if !unchanged && model.doc.state != ModelState::Active {
        return Err(ApiError::invalid(format!(
            "the model \"{}\" is {} and cannot be chosen for a template",
            model.doc.name,
            model.doc.state.as_str()
        ))
        .with_details(serde_json::json!({ "code": "model_not_assignable" })));
    }

    let range = |name: &str, v: Option<f64>, lo: f64, hi: f64| -> ApiResult<()> {
        match v {
            Some(x) if !(lo..=hi).contains(&x) => {
                Err(ApiError::invalid(format!("{name} must be between {lo} and {hi}")))
            }
            _ => Ok(()),
        }
    };
    range("temperature", spec.temperature.as_ref().and_then(|p| p.value), 0.0, 2.0)?;
    range("top_p", spec.top_p.as_ref().and_then(|p| p.value), 0.0, 1.0)?;
    range("frequency_penalty", spec.frequency_penalty.as_ref().and_then(|p| p.value), -2.0, 2.0)?;
    range("presence_penalty", spec.presence_penalty.as_ref().and_then(|p| p.value), -2.0, 2.0)?;
    if spec.max_output_tokens == Some(0) {
        return Err(ApiError::invalid("max_output_tokens must be at least 1"));
    }
    if spec.timeout_seconds.is_some_and(|t| !(1..=600).contains(&t)) {
        return Err(ApiError::invalid("timeout_seconds must be between 1 and 600"));
    }
    if spec.retry_max_attempts.is_some_and(|r| !(1..=5).contains(&r)) {
        return Err(ApiError::invalid("retry_max_attempts must be between 1 and 5"));
    }
    for key in spec.tags.keys() {
        if !ws.doc.tag_keys.iter().any(|k| k == key) {
            return Err(ApiError::invalid(format!("the tag key \"{key}\" is not declared in this workspace")));
        }
    }

    // Which of the template's own values the model would not accept (PACOD's "ignored
    // parameters"): they are stored, never sent, and shown as a warning.
    let cx = MergeContext {
        spec: Some(spec),
        capabilities: &model.doc.capabilities,
        terms: GrantTerms { follow_successor: terms.follow_successor, max_output_tokens: terms.max_output_tokens },
        model_max_output: model.doc.max_output_tokens,
        policy: UnsupportedParams::Drop,
        filter_caller: true,
    };
    let decisions = merge(&cx, &CallerParams::default()).map_err(|e| ApiError::invalid(e.message))?;
    Ok(decisions.dropped)
}

impl Resource for Template {
    const PLURAL: &'static str = "templates";
    const LABEL: &'static str = "template";
    const SCOPE: ScopeKind = ScopeKind::Workspace;
    type Input = TemplateInput;
    type View = TemplateView;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(mut input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        if !is_slug(&input.name) {
            return Err(ApiError::invalid("name must be lower-case letters, digits and hyphens"));
        }
        let ws = ctx.snap.workspaces.get(ctx.scope.ws()).ok_or_else(|| ApiError::not_found("workspace"))?;
        let previous = existing.and_then(|e| e.doc.current()).map(|v| &v.spec);
        check_spec(ctx.snap, ws, &mut input.spec, previous)?;

        let mut doc = match existing {
            Some(e) => e.doc.clone(),
            None => Template {
                name: String::new(),
                description: String::new(),
                current_version: 0,
                versions: Vec::new(),
                status: Status::Active,
            },
        };
        doc.name = input.name;
        doc.description = input.description;
        doc.status = input.status;
        // A changed configuration is a new version; anything else edits the template in place.
        if previous != Some(&input.spec) {
            let version = doc.next_version();
            doc.versions.push(TemplateVersion {
                version,
                spec: input.spec,
                comment: input.comment,
                created_by: ctx.actor.to_owned(),
                created_at: Utc::now(),
            });
            doc.current_version = version;
            // Old versions are dropped from the front; the current one is the newest.
            if doc.versions.len() > MAX_TEMPLATE_VERSIONS {
                let excess = doc.versions.len() - MAX_TEMPLATE_VERSIONS;
                doc.versions.drain(..excess);
            }
        }
        Ok(doc)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        let d = &obj.doc;
        TemplateView {
            name: d.name.clone(),
            description: d.description.clone(),
            status: d.status,
            current_version: d.current_version,
            spec: d.current().map(|v| v.spec.clone()).unwrap_or_default(),
            versions: d
                .versions
                .iter()
                .rev()
                .map(|v| VersionInfo {
                    version: v.version,
                    comment: v.comment.clone(),
                    created_by: v.created_by.clone(),
                    created_at: v.created_at,
                })
                .collect(),
        }
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let ws = obj.parent.as_deref().unwrap_or_default();
        let mut keys = Vec::new();
        for row in tx.list(ApiKey::KIND, Some(ws)).await? {
            let key: Obj<ApiKey> = row.into_obj()?;
            if key.doc.allowed_templates.contains(&obj.doc.name) {
                keys.push(key.doc.name);
            }
        }
        if !keys.is_empty() {
            return Err(ApiError::conflict("API keys are restricted to this template; change them first")
                .with_details(serde_json::json!({ "code": "template_in_use", "keys": keys })));
        }
        Ok(())
    }
}

/// Parameters of the current version that the model does not accept.
#[derive(Debug, Serialize, JsonSchema)]
pub struct TemplateCheck {
    /// The template's own values that are stored but never sent.
    pub ignored_params: Vec<String>,
    /// The state of the template's model.
    pub model_state: ModelState,
    /// Set when the model is deprecated or retired and has a successor.
    pub successor: Option<String>,
}

async fn check(State(st): State<AdminState>, p: Principal, Params(params): Params) -> ApiResult<Json<TemplateCheck>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !Template::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("template"))?;
    let obj = load::<Template>(&st, id, &scope).await?;
    let ws = &snap.workspaces[scope.ws()];
    let mut spec = obj.doc.current().map(|v| v.spec.clone()).unwrap_or_default();
    let previous = spec.clone();
    let ignored_params = check_spec(&snap, ws, &mut spec, Some(&previous))?;
    let model = snap.model_by_name(&spec.model).ok_or_else(|| ApiError::not_found("model"))?;
    Ok(Json(TemplateCheck {
        ignored_params,
        model_state: model.doc.state,
        successor: model.doc.successor_id.as_ref().and_then(|s| snap.models.get(s)).map(|m| m.doc.name.clone()),
    }))
}

async fn version(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<TemplateVersion>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !Template::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("template"))?;
    let n: u32 = params.get("version").and_then(|v| v.parse().ok()).ok_or_else(|| ApiError::not_found("version"))?;
    let obj = load::<Template>(&st, id, &scope).await?;
    obj.doc.versions.into_iter().find(|v| v.version == n).map(Json).ok_or_else(|| ApiError::not_found("version"))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RollbackInput {
    /// The version to make current again.
    pub version: u32,
}

async fn rollback(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<RollbackInput>,
) -> ApiResult<Json<Item<TemplateView>>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !Template::can_write(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("template"))?;
    let mut obj = load::<Template>(&st, id, &scope).await?;
    if !obj.doc.versions.iter().any(|v| v.version == input.version) {
        return Err(ApiError::not_found("version"));
    }
    obj.doc.current_version = input.version;
    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(scope.org.as_deref(), scope.ws.as_deref());
    audit.action = format!("rollback:{}", input.version);
    let row = tx
        .update(
            &obj.id,
            Some(obj.version),
            Some(&obj.doc.name),
            &serde_json::to_value(&obj.doc).map_err(ApiError::internal)?,
            audit,
        )
        .await?;
    st.commit(tx).await?;
    let obj: Obj<Template> = row.into_obj()?;
    Ok(Json(Item { meta: obj.meta(), data: Template::view(&obj) }))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CloneInput {
    /// The workspace to copy the template into.
    pub workspace_id: String,
    /// The name of the copy. Defaults to the same name.
    #[serde(default)]
    pub name: Option<String>,
}

async fn clone_template(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<CloneInput>,
) -> ApiResult<(StatusCode, Json<Item<TemplateView>>)> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !Template::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let target = snap
        .workspaces
        .get(&input.workspace_id)
        .ok_or_else(|| ApiError::invalid("workspace_id does not name a workspace"))?;
    if !p.can_admin_workspace(&target.id, &target.organisation_id) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("template"))?;
    let source = load::<Template>(&st, id, &scope).await?;
    let mut spec = source.doc.current().map(|v| v.spec.clone()).ok_or_else(|| ApiError::not_found("version"))?;
    // The copy must be valid where it lands: model granted there, tags declared there.
    check_spec(&snap, target, &mut spec, None)?;
    let name = input.name.unwrap_or_else(|| source.doc.name.clone());
    if !is_slug(&name) {
        return Err(ApiError::invalid("name must be lower-case letters, digits and hyphens"));
    }
    let doc = Template {
        name,
        description: source.doc.description.clone(),
        current_version: 1,
        versions: vec![TemplateVersion {
            version: 1,
            spec,
            comment: format!("copied from {}", source.doc.name),
            created_by: p.id.clone(),
            created_at: Utc::now(),
        }],
        status: Status::Active,
    };
    let new = new_id(Template::PREFIX);
    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(Some(&target.organisation_id), Some(&target.id));
    audit.action = "clone".into();
    let row = tx
        .create(
            Template::KIND,
            &new,
            Some(&target.id),
            Some(&doc.name),
            &serde_json::to_value(&doc).map_err(ApiError::internal)?,
            audit,
        )
        .await?;
    st.commit(tx).await?;
    let obj: Obj<Template> = row.into_obj()?;
    Ok((StatusCode::CREATED, Json(Item { meta: obj.meta(), data: Template::view(&obj) })))
}

pub fn routes(mut router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    router = register::<Template>(router, api, Ops::ALL);
    let base = collection_path(ScopeKind::Workspace, Template::PLURAL);
    let one = format!("{base}/{{id}}");
    let view = api.schema::<Item<TemplateView>>();
    let version_schema = api.schema::<TemplateVersion>();
    let check_schema = api.schema::<TemplateCheck>();
    let rollback_in = api.schema::<RollbackInput>();
    let clone_in = api.schema::<CloneInput>();
    api.add(
        api.op(
            "get",
            &format!("{one}/check"),
            "checkTemplate",
            "templates",
            "Which of the template's values its model does not accept",
        )
        .ok(check_schema),
    );
    api.add(
        api.op("get", &format!("{one}/versions/{{version}}"), "getTemplateVersion", "templates", "One saved version")
            .ok(version_schema),
    );
    api.add(
        api.op("post", &format!("{one}/rollback"), "rollbackTemplate", "templates", "Make an earlier version current")
            .body(rollback_in)
            .ok(view.clone()),
    );
    api.add(
        api.op(
            "post",
            &format!("{one}/clone"),
            "cloneTemplate",
            "templates",
            "Copy the template into another workspace",
        )
        .body(clone_in)
        .created(view),
    );
    router
        .route(&format!("{one}/check"), get(check))
        .route(&format!("{one}/versions/{{version}}"), get(version))
        .route(&format!("{one}/rollback"), post(rollback))
        .route(&format!("{one}/clone"), post(clone_template))
}
