//! Templates of workspaces and of organisations (concept section 7). Every workspace inherits
//! its organisation's templates; a template of its own with the same name hides one.

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
    BuildCtx, Item, Ops, Params, Resource, ScopeIds, ScopeKind, collection_path, load, register, resolve_scope,
};
use crate::mgmt::error::{ApiError, ApiResult};
use crate::mgmt::openapi::OpenApi;
use std::collections::BTreeMap;

use crate::model::template::{MAX_POOL, MAX_TEMPLATE_VERSIONS};
use crate::model::{
    Algorithm, ApiKey, BrokerPool, Complexity, Doc, FailoverPolicy, ModelState, Obj, OrgTemplate, PoolEntry,
    RouteSource, Status, Template, TemplateSpec, TemplateVersion, UnsupportedParams, Workspace, is_slug, new_id,
};
use crate::params::{CallerParams, MergeContext, merge};
use crate::params_schema::{SchemaTarget, Spelling, template_schema};
use crate::snapshot::{GrantTerms, OrgRt, Snapshot, WorkspaceRt};
use crate::store::Tx;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TemplateInput {
    /// Slug, unique in the workspace or organisation. Applications address the template as
    /// `@name`.
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

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
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

/// What checking a spec found.
pub struct SpecCheck {
    /// Per model of the pool, in order: the template's own values that model does not accept.
    pub ignored: Vec<Vec<String>>,
    /// Routing that will not work as written, for example tiers the scheme names that the
    /// pool lacks. Not errors: an organisation's scheme may serve templates of every shape.
    pub warnings: Vec<String>,
}

fn invalid(code: &str, message: String) -> ApiError {
    ApiError::invalid(message).with_details(serde_json::json!({ "code": code }))
}

/// The most a pool entry may weigh.
const MAX_WEIGHT: u32 = 10_000;

/// Where a template is checked: an organisation's against the organisation alone, a workspace's
/// (or one a workspace inherits) against the workspace.
#[derive(Clone, Copy)]
pub struct At<'a> {
    pub org: &'a OrgRt,
    pub ws: Option<&'a WorkspaceRt>,
}

impl<'a> At<'a> {
    fn of(snap: &'a Snapshot, scope: &ScopeIds) -> ApiResult<Self> {
        let org = snap.organisations.get(scope.org()).ok_or_else(|| ApiError::not_found("organisation"))?;
        let ws = match &scope.ws {
            Some(w) => Some(&**snap.workspaces.get(w).ok_or_else(|| ApiError::not_found("workspace"))?),
            None => None,
        };
        Ok(Self { org, ws })
    }

    fn workspace(snap: &'a Snapshot, ws: &'a WorkspaceRt) -> ApiResult<Self> {
        let org = snap.organisations.get(&ws.organisation_id).ok_or_else(|| ApiError::not_found("organisation"))?;
        Ok(Self { org, ws: Some(ws) })
    }

    fn terms(&self, snap: &Snapshot, model_id: &str) -> Option<GrantTerms> {
        match self.ws {
            Some(ws) => snap.grant_terms(ws, model_id),
            None => self.org.grant_terms(model_id),
        }
    }

    /// The tag keys a template's tags must be among. At an organisation: those of a workspace
    /// that declares none of its own.
    fn tag_keys(&self, snap: &'a Snapshot) -> &'a [String] {
        match self.ws {
            Some(ws) => &ws.settings.tag_keys,
            None => {
                self.org.doc.settings.tag_keys.as_deref().filter(|k| !k.is_empty()).unwrap_or(&snap.settings.tag_keys)
            }
        }
    }

    fn noun(&self) -> &'static str {
        if self.ws.is_some() { "workspace" } else { "organisation" }
    }
}

/// Checks a spec where the template lives and normalises each model reference to the model's
/// id.
fn check_spec(
    snap: &Snapshot,
    at: At<'_>,
    spec: &mut TemplateSpec,
    previous: Option<&TemplateSpec>,
) -> ApiResult<SpecCheck> {
    match (spec.model.is_empty(), spec.models.is_empty()) {
        (true, true) => return Err(ApiError::invalid("give the template a model, or several in models")),
        (false, false) => return Err(ApiError::invalid("give either model or models, not both")),
        _ => {}
    }
    if spec.models.len() > MAX_POOL {
        return Err(ApiError::invalid(format!("a template holds at most {MAX_POOL} models")));
    }
    if !spec.models.is_empty() && spec.models.iter().all(|e| e.weight == 0) {
        return Err(ApiError::invalid("at least one model of the pool needs a weight above 0"));
    }
    for e in &spec.models {
        if e.weight > MAX_WEIGHT {
            return Err(ApiError::invalid(format!("a weight is at most {MAX_WEIGHT}")));
        }
        if e.tier.as_deref().is_some_and(|t| !is_slug(t)) {
            return Err(ApiError::invalid("a tier must be lower-case letters, digits and hyphens"));
        }
    }
    if let Some(Err(e)) = spec.broker_offer.as_ref().map(|o| o.check()) {
        return Err(ApiError::invalid(format!("broker_offer.{e}")));
    }
    if let Some(f) = &spec.pool_from {
        if !f.query.is_object() {
            return Err(ApiError::invalid("pool_from.query: a broker discovery query (an object)"));
        }
        if !(1..=(MAX_POOL as u8 - 1)).contains(&f.max) {
            return Err(ApiError::invalid(format!("pool_from.max: 1 to {}", MAX_POOL - 1)));
        }
        if f.own_leases && f.mode != crate::model::PoolMode::Follow {
            return Err(ApiError::invalid("pool_from.own_leases needs mode follow: leased endpoints come and go"));
        }
    }
    let org = at.org;
    // A scheme the template already names is kept, as a model is: an organisation that
    // locks the setting later, or a scheme gone missing on import, leaves it editable.
    let mut warnings = Vec::new();
    let kept = previous.is_some_and(|p| p.routing_scheme == spec.routing_scheme);
    if let Some(name) = &spec.routing_scheme {
        let locked = org.doc.settings.locked.iter().any(|l| l == "routing_scheme");
        let unknown = !org.schemes.contains_key(name);
        match (locked, unknown, kept) {
            (true, _, false) => {
                return Err(invalid(
                    "routing_scheme_locked",
                    "the organisation decides the routing scheme for all its templates".into(),
                ));
            }
            (_, true, false) => {
                return Err(invalid(
                    "routing_scheme_unknown",
                    format!("the organisation has no routing scheme \"{name}\""),
                ));
            }
            (true, _, true) => warnings
                .push(format!("the organisation decides the routing scheme: \"{name}\" named here does not apply")),
            (_, true, true) => {
                warnings.push(format!("the routing scheme \"{name}\" does not exist; the next level's applies"))
            }
            _ => {}
        }
    }

    let before: Vec<String> = previous.map(|p| p.pool().iter().map(|e| e.model.clone()).collect()).unwrap_or_default();
    let mut checked: Vec<(PoolEntry, &crate::snapshot::ModelRt, GrantTerms)> = Vec::new();
    let single = spec.models.is_empty();
    let mut entries = if single { vec![PoolEntry::of(&spec.model)] } else { std::mem::take(&mut spec.models) };
    for entry in &mut entries {
        let model_id = snap
            .model_names
            .get(&entry.model)
            .cloned()
            .or_else(|| snap.models.contains_key(&entry.model).then(|| entry.model.clone()))
            .ok_or_else(|| invalid("unknown_model", format!("unknown model \"{}\"", entry.model)))?;
        let model = &snap.models[&model_id];
        entry.model = model.doc.name.clone();
        let terms = at.terms(snap, &model_id).ok_or_else(|| {
            invalid(
                "model_not_granted",
                format!("the model \"{}\" is not granted to this {}", model.doc.name, at.noun()),
            )
        })?;
        // A template keeps a model it already uses; a new choice must be an active model.
        if !before.contains(&entry.model) && model.doc.state != ModelState::Active {
            return Err(invalid(
                "model_not_assignable",
                format!(
                    "the model \"{}\" is {} and cannot be chosen for a template",
                    model.doc.name,
                    model.doc.state.as_str()
                ),
            ));
        }
        if checked.iter().any(|(e, _, _)| e.model == entry.model) {
            return Err(invalid("duplicate_model", format!("the model \"{}\" is in the pool twice", model.doc.name)));
        }
        checked.push((entry.clone(), &**model, terms));
    }
    if single {
        spec.model = entries.remove(0).model;
    } else {
        spec.models = entries;
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
        if !at.tag_keys(snap).iter().any(|k| k == key) {
            return Err(ApiError::invalid(format!("the tag key \"{key}\" is not declared in this {}", at.noun())));
        }
    }

    // Which of the template's own values each model would not accept (its "ignored
    // parameters"): they are stored, never sent to that model, and shown as a warning.
    let mut ignored = Vec::new();
    for (_, model, terms) in &checked {
        let cx = MergeContext {
            spec: Some(spec),
            capabilities: &model.doc.capabilities,
            terms: *terms,
            model_max_output: model.doc.max_output_tokens,
            policy: UnsupportedParams::Drop,
            filter_caller: true,
        };
        ignored.push(merge(&cx, &CallerParams::default()).map_err(|e| ApiError::invalid(e.message))?.dropped);
    }

    // Tiers the effective scheme chooses that this pool does not have.
    let effective = effective_scheme(org, at.ws, spec);
    if let Some((scheme, _)) = effective.and_then(|(n, src)| org.schemes.get(n).map(|s| (s, src))) {
        let have: Vec<&str> = checked.iter().filter_map(|(e, _, _)| e.tier.as_deref()).collect();
        let named = scheme.rules.iter().map(|r| (Some(&*r.name), &r.action)).chain([(None, &scheme.default)]);
        for (rule, action) in named {
            let missing: Vec<&str> = action.tiers.iter().map(String::as_str).filter(|t| !have.contains(t)).collect();
            if missing.is_empty() {
                continue;
            }
            let what = rule.map_or("the default".to_owned(), |r| format!("rule \"{r}\""));
            let effect = if missing.len() == action.tiers.len() {
                if rule.is_some() { "the rule never applies" } else { "the whole pool is used" }
            } else {
                "those tiers are skipped"
            };
            warnings.push(format!(
                "{what} of the routing scheme \"{}\" names the tiers {} this pool does not have; {effect}",
                scheme.name,
                missing.join(", ")
            ));
        }
    }
    Ok(SpecCheck { ignored, warnings })
}

/// The scheme that applies to a template, and where it was assigned.
fn effective_scheme<'a>(
    org: &'a OrgRt,
    ws: Option<&'a WorkspaceRt>,
    spec: &'a TemplateSpec,
) -> Option<(&'a str, RouteSource)> {
    let locked = org.doc.settings.locked.iter().any(|l| l == "routing_scheme");
    [
        (spec.routing_scheme.as_deref().filter(|_| !locked), RouteSource::Template),
        (ws.and_then(|w| w.doc.settings.routing_scheme.as_deref()).filter(|_| !locked), RouteSource::Workspace),
        (org.doc.settings.routing_scheme.as_deref(), RouteSource::Organisation),
    ]
    .into_iter()
    .find_map(|(n, src)| n.filter(|n| org.schemes.get(*n).is_some_and(|s| s.enabled)).map(|n| (n, src)))
}

/// The document a save produces. A changed configuration is a new version; anything else
/// edits the template in place.
fn build_template(
    mut input: TemplateInput,
    existing: Option<&Template>,
    ctx: &BuildCtx<'_>,
    at: At<'_>,
) -> ApiResult<Template> {
    if !is_slug(&input.name) {
        return Err(ApiError::invalid("name must be lower-case letters, digits and hyphens"));
    }
    let previous = existing.and_then(|e| e.current()).map(|v| &v.spec);
    check_spec(ctx.snap, at, &mut input.spec, previous)?;

    let mut doc = match existing {
        Some(e) => e.clone(),
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

fn view_of(d: &Template) -> TemplateView {
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

/// The names of the API keys of a workspace whose allow-list names the template `name`.
async fn keys_naming(tx: &mut Tx, ws: &str, name: &str) -> ApiResult<Vec<String>> {
    let mut keys = Vec::new();
    for row in tx.list(ApiKey::KIND, Some(ws)).await? {
        let key: Obj<ApiKey> = row.into_obj()?;
        if key.doc.allowed_templates.iter().any(|t| t == name) {
            keys.push(key.doc.name);
        }
    }
    Ok(keys)
}

/// The record of what a template's pool from the broker resolved to goes with the template:
/// nothing else removes it, and a workspace or organisation is not deleted while it holds one.
async fn forget_broker_pool(tx: &mut Tx, parent: &str, template_id: &str) -> ApiResult<()> {
    for row in tx.list(BrokerPool::KIND, Some(parent)).await? {
        let pool: Obj<BrokerPool> = row.into_obj()?;
        if pool.doc.template_id == template_id {
            let mut audit = crate::store::AuditEntry::system("system");
            audit.detail = serde_json::json!({ "reason": "template deleted" });
            tx.delete(&pool.id, audit).await?;
        }
    }
    Ok(())
}

fn template_in_use(keys: Vec<String>) -> ApiError {
    ApiError::conflict("API keys are restricted to this template; change them first")
        .with_details(serde_json::json!({ "code": "template_in_use", "keys": keys }))
}

/// A template of either kind: the endpoints beside the generic ones serve both.
pub trait TemplateKind: Resource<Input = TemplateInput, View = TemplateView> {
    fn template(&self) -> &Template;
    fn template_mut(&mut self) -> &mut Template;
}

impl TemplateKind for Template {
    fn template(&self) -> &Template {
        self
    }

    fn template_mut(&mut self) -> &mut Template {
        self
    }
}

impl TemplateKind for OrgTemplate {
    fn template(&self) -> &Template {
        &self.0
    }

    fn template_mut(&mut self) -> &mut Template {
        &mut self.0
    }
}

impl Resource for Template {
    const PLURAL: &'static str = "templates";
    const LABEL: &'static str = "template";
    const SCOPE: ScopeKind = ScopeKind::Workspace;
    const STRICT: bool = true;
    type Input = TemplateInput;
    type View = TemplateView;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.name.clone())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        build_template(input, existing.map(|e| &e.doc), ctx, At::of(ctx.snap, ctx.scope)?)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        view_of(&obj.doc)
    }

    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let ws = obj.parent.as_deref().unwrap_or_default();
        let keys = keys_naming(tx, ws, &obj.doc.name).await?;
        if !keys.is_empty() {
            return Err(template_in_use(keys));
        }
        forget_broker_pool(tx, ws, &obj.id).await
    }
}

impl Resource for OrgTemplate {
    const PLURAL: &'static str = "templates";
    const LABEL: &'static str = "template";
    const SCOPE: ScopeKind = ScopeKind::Organisation;
    const STRICT: bool = true;
    type Input = TemplateInput;
    type View = TemplateView;

    fn unique_name(doc: &Self) -> Option<String> {
        Some(doc.0.name.clone())
    }

    fn build(input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self> {
        let org = ctx.snap.organisations.get(ctx.scope.org()).ok_or_else(|| ApiError::not_found("organisation"))?;
        build_template(input, existing.map(|e| &e.doc.0), ctx, At { org, ws: None }).map(OrgTemplate)
    }

    fn view(obj: &Obj<Self>) -> Self::View {
        view_of(&obj.doc.0)
    }

    fn effects(obj: &Obj<Self>, snap: &Snapshot) -> Vec<String> {
        let mut using: Vec<&str> = snap
            .workspaces
            .values()
            .filter(|w| w.templates.get(&obj.doc.0.name).is_some_and(|t| t.id == obj.id))
            .map(|w| w.doc.name.as_str())
            .collect();
        using.sort_unstable();
        if using.is_empty() {
            return Vec::new();
        }
        vec![format!("calls that name @{} are rejected in the workspaces {}", obj.doc.0.name, using.join(", "))]
    }

    /// Workspace admins use the templates they inherit, so they may read them.
    fn can_read(p: &Principal, scope: &ScopeIds, _snap: &Snapshot) -> bool {
        p.sees_org(scope.org())
    }

    /// A workspace whose own template of the same name hides this one keeps its keys.
    async fn before_delete(tx: &mut Tx, obj: &Obj<Self>) -> ApiResult<()> {
        let org = obj.parent.as_deref().unwrap_or_default();
        let name = &obj.doc.0.name;
        let mut keys = Vec::new();
        for row in tx.list(Workspace::KIND, Some(org)).await? {
            let ws: Obj<Workspace> = row.into_obj()?;
            let own = tx.list(Template::KIND, Some(&ws.id)).await?;
            if own.iter().any(|t| t.name.as_deref() == Some(name)) {
                continue;
            }
            keys.extend(keys_naming(tx, &ws.id, name).await?.into_iter().map(|k| format!("{}/{k}", ws.doc.name)));
        }
        if !keys.is_empty() {
            return Err(template_in_use(keys));
        }
        forget_broker_pool(tx, org, &obj.id).await
    }
}

/// One model of a template, checked.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ModelCheck {
    pub model: String,
    pub tier: Option<String>,
    pub state: ModelState,
    /// Set when the model is deprecated or retired and has a successor.
    pub successor: Option<String>,
    /// The template's own values that are stored but never sent to this model.
    pub ignored_params: Vec<String>,
}

/// The routing that applies to a template.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RoutingCheck {
    /// The scheme, unset for the built-in routing (the pool by priority).
    pub scheme: Option<String>,
    pub source: RouteSource,
    pub warnings: Vec<String>,
}

/// Parameters of the current version that its models do not accept, and its routing.
#[derive(Debug, Serialize, JsonSchema)]
pub struct TemplateCheck {
    /// The template's own values that are stored but never sent: for the first model.
    pub ignored_params: Vec<String>,
    /// The state of the template's (first) model.
    pub model_state: ModelState,
    /// Set when that model is deprecated or retired and has a successor.
    pub successor: Option<String>,
    /// Every model of the template.
    pub models: Vec<ModelCheck>,
    pub routing: RoutingCheck,
    /// What a request to the template may carry, as a JSON Schema in the native interface's
    /// names: what every model of its pool takes, under the template's rules. Applications read
    /// the same from the model lists.
    pub parameters_schema: serde_json::Value,
}

async fn check<R: TemplateKind>(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<TemplateCheck>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(R::SCOPE, &params, &snap)?;
    if !R::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("template"))?;
    let obj = load::<R>(&st, id, &scope).await?;
    let at = At::of(&snap, &scope)?;
    let t = obj.doc.template();
    let mut spec = t.current().map(|v| v.spec.clone()).unwrap_or_default();
    let previous = spec.clone();
    let found = check_spec(&snap, at, &mut spec, Some(&previous))?;
    let mut models = Vec::new();
    for (entry, ignored) in spec.pool().iter().zip(found.ignored) {
        let model = snap.model_by_name(&entry.model).ok_or_else(|| ApiError::not_found("model"))?;
        models.push(ModelCheck {
            model: entry.model.clone(),
            tier: entry.tier.clone(),
            state: model.doc.state,
            successor: model.doc.successor_id.as_ref().and_then(|s| snap.models.get(s)).map(|m| m.doc.name.clone()),
            ignored_params: ignored,
        });
    }
    let pool: Vec<SchemaTarget<'_>> = spec
        .pool()
        .iter()
        .filter_map(|e| snap.model_by_name(&e.model))
        .map(|m| SchemaTarget::with_terms(&snap, m, at.terms(&snap, &m.id).unwrap_or_else(GrantTerms::open)))
        .collect();
    let parameters_schema = template_schema(&t.name, &spec, &pool, Spelling::Native);
    let primary = models.iter().position(|m| m.model == spec.primary_model()).unwrap_or_default();
    let (scheme, source) = match effective_scheme(at.org, at.ws, &spec) {
        Some((n, src)) => (Some(n.to_owned()), src),
        None => (None, RouteSource::BuiltIn),
    };
    Ok(Json(TemplateCheck {
        ignored_params: models[primary].ignored_params.clone(),
        model_state: models[primary].state,
        successor: models[primary].successor.clone(),
        models,
        routing: RoutingCheck { scheme, source, warnings: found.warnings },
        parameters_schema,
    }))
}

/// An organisation's template as one of its workspaces inherits it.
#[derive(Debug, Serialize, JsonSchema)]
pub struct InheritedTemplate {
    #[serde(flatten)]
    pub template: Item<TemplateView>,
    /// The workspace has a template of this name of its own, which applies instead.
    pub hidden: bool,
    /// Why the workspace could not save the template as its own, for example a model it is not
    /// granted. Calls to such a model fail in this workspace; other models of a pool serve.
    pub problem: Option<String>,
    /// Routing in this workspace that will not work as written.
    pub warnings: Vec<String>,
}

/// `GET /orgs/{org}/workspaces/{ws}/inherited-templates`: the organisation's templates, checked
/// against the workspace.
async fn inherited(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<Vec<InheritedTemplate>>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !Template::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let at = At::workspace(&snap, &snap.workspaces[scope.ws()])?;
    let mut tx = st.store.begin().await?;
    let own: Vec<String> =
        tx.list(Template::KIND, Some(scope.ws())).await?.into_iter().filter_map(|r| r.name).collect();
    let mut out = Vec::new();
    for row in tx.list(OrgTemplate::KIND, Some(scope.org())).await? {
        let obj: Obj<OrgTemplate> = row.into_obj()?;
        let t = &obj.doc.0;
        let (problem, warnings) = match t.current() {
            Some(v) => match check_spec(&snap, at, &mut v.spec.clone(), Some(&v.spec)) {
                Ok(found) => (None, found.warnings),
                Err(e) => (Some(e.message), Vec::new()),
            },
            None => (Some("the template has no current version".into()), Vec::new()),
        };
        out.push(InheritedTemplate {
            hidden: own.contains(&t.name),
            template: Item { meta: obj.meta(), data: view_of(t) },
            problem,
            warnings,
        });
    }
    out.sort_by(|a, b| a.template.data.name.cmp(&b.template.data.name));
    Ok(Json(out))
}

/// A call to try the routing with. Every field is optional, as on a real call.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RouteQuestion {
    pub complexity: Option<Complexity>,
    pub agent_id: Option<String>,
    pub task: Option<String>,
    pub user_id: Option<String>,
    pub end_user: Option<String>,
    pub session_id: Option<String>,
    pub tags: BTreeMap<String, String>,
    /// Apply this API key's model allow-list.
    pub key_id: Option<String>,
    /// Proxy mode: only models at this provider (its name).
    pub provider: Option<String>,
    /// `openai`, `anthropic`, `gemini`, `native` or `proxy`. Default `openai`.
    pub interface: Option<String>,
    pub stream: bool,
    /// The intent an enforcing intent plugin would find, as routing acts on it.
    pub intent: Option<String>,
    /// Its confidence, from 0 to 1. Default 1 with an intent.
    pub intent_confidence: Option<f32>,
    /// The plugins' flags: the probability of yes, by question id.
    pub flags: BTreeMap<String, f32>,
}

/// One model routing would try, in order.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RouteCandidate {
    pub model: String,
    pub provider: String,
    pub tier: Option<String>,
    pub weight: u32,
    pub priority: u8,
    /// `primary` for the model tried first, `backup` for the rest.
    pub role: String,
    pub state: ModelState,
    pub breaker: crate::routing::BreakerState,
    pub in_flight: u32,
    pub first_byte_ms: Option<f64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RouteExcluded {
    pub model: String,
    pub reason: String,
}

/// How routing would treat a call.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RouteExplain {
    pub scheme: Option<String>,
    pub scheme_version: Option<i32>,
    pub source: RouteSource,
    /// The rule that matched; unset for the default.
    pub rule: Option<String>,
    pub algorithm: Algorithm,
    pub failover: FailoverPolicy,
    pub candidates: Vec<RouteCandidate>,
    pub excluded: Vec<RouteExcluded>,
    /// Set when the first model is drawn at random or in turn: another call may start elsewhere.
    pub note: Option<String>,
}

async fn explain(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(q): Json<RouteQuestion>,
) -> ApiResult<Json<RouteExplain>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(ScopeKind::Workspace, &params, &snap)?;
    if !Template::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("template"))?;
    let ws = &snap.workspaces[scope.ws()];
    // An inherited template is found here by its organisation's id.
    let t = ws
        .templates
        .values()
        .find(|t| &t.id == id)
        .ok_or_else(|| ApiError::invalid("the template is disabled, or none of its models exists"))?;
    let key = match &q.key_id {
        Some(k) => Some(
            snap.keys
                .values()
                .find(|x| &x.id == k && x.workspace_id == ws.id)
                .ok_or_else(|| ApiError::invalid("key_id does not name a key of this workspace"))?,
        ),
        None => None,
    };
    let provider = match &q.provider {
        Some(n) => {
            Some(snap.provider_by_name(n).ok_or_else(|| ApiError::invalid("provider does not name a provider"))?)
        }
        None => None,
    };
    let key_allows = |m: &crate::routing::PoolMember| {
        key.is_none_or(|k| {
            k.doc.allowed_models.is_empty()
                || k.doc.allowed_models.iter().any(|a| *a == m.model.doc.name || *a == m.model.id)
        })
    };
    let at_provider = |m: &crate::routing::PoolMember| provider.is_none_or(|p| p.id == m.model.doc.provider_id);
    let input = crate::routing::RouteInput {
        complexity: q.complexity,
        agent_id: q.agent_id.as_deref(),
        task: q.task.as_deref(),
        user_id: q.user_id.as_deref(),
        end_user: q.end_user.as_deref(),
        session_id: q.session_id.as_deref(),
        template: &t.name,
        tags: &q.tags,
        interface: q.interface.as_deref().unwrap_or("openai"),
        stream: q.stream,
        input_tokens: 0,
        intent: q.intent.as_deref(),
        intent_confidence: q.intent_confidence.unwrap_or(if q.intent.is_some() { 1.0 } else { 0.0 }),
        flags: &q.flags,
    };
    let action = t.route.action_for(&input);
    let now = crate::routing::live::now_ms();
    let chosen = action.select(&t.pool, &input, |m| key_allows(m) && at_provider(m), now, &mut fastrand::Rng::new());
    let candidates = chosen
        .iter()
        .enumerate()
        .map(|(n, i)| {
            let m = &t.pool[i];
            RouteCandidate {
                model: m.model.doc.name.clone(),
                provider: snap.providers.get(&m.model.doc.provider_id).map(|p| p.doc.name.clone()).unwrap_or_default(),
                tier: m.entry.tier.clone(),
                weight: m.entry.weight,
                priority: m.entry.priority,
                role: if n == 0 { "primary" } else { "backup" }.into(),
                state: m.model.doc.state,
                breaker: m.state.breaker(now),
                in_flight: m.state.in_flight(),
                first_byte_ms: match m.state.ewma_ttfb_us() {
                    0 => None,
                    us => Some(us as f64 / 1000.0),
                },
            }
        })
        .collect();
    let excluded = t
        .pool
        .iter()
        .enumerate()
        .filter(|(i, _)| !chosen.contains(*i))
        .map(|(_, m)| RouteExcluded {
            model: m.model.doc.name.clone(),
            reason: if !key_allows(m) {
                "the key may not use it"
            } else if !at_provider(m) {
                "at another provider"
            } else {
                "not among the tiers the rule chooses, or beyond failover.max_targets"
            }
            .into(),
        })
        .collect();
    let random = matches!(
        action.algorithm,
        Algorithm::Weighted
            | Algorithm::RoundRobin
            | Algorithm::WeightedRoundRobin
            | Algorithm::LeastInFlight
            | Algorithm::LowestLatency
    ) && !(action.affinity == crate::model::Affinity::Session && q.session_id.is_some());
    Ok(Json(RouteExplain {
        scheme: t.route.scheme.as_ref().map(|s| s.name.to_string()),
        scheme_version: t.route.scheme.as_ref().map(|s| s.version),
        source: t.route.source,
        rule: action.rule.as_deref().map(str::to_owned),
        algorithm: action.algorithm,
        failover: action.failover,
        candidates,
        excluded,
        note: random.then(|| format!("{} picks the first model per call; this is one draw", action.algorithm.name())),
    }))
}

async fn version<R: TemplateKind>(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<TemplateVersion>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(R::SCOPE, &params, &snap)?;
    if !R::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("template"))?;
    let n: u32 = params.get("version").and_then(|v| v.parse().ok()).ok_or_else(|| ApiError::not_found("version"))?;
    let obj = load::<R>(&st, id, &scope).await?;
    if let Some(v) = obj.doc.template().versions.iter().find(|v| v.version == n) {
        return Ok(Json(v.clone()));
    }
    // Older versions are kept outside the document.
    archived(&st, id, n).await?.map(Json).ok_or_else(|| ApiError::not_found("version"))
}

/// A version kept in `template_versions`.
async fn archived(st: &AdminState, id: &str, n: u32) -> ApiResult<Option<TemplateVersion>> {
    let mut tx = st.store.begin().await?;
    let found = tx.template_version(id, n).await?;
    found.map(serde_json::from_value).transpose().map_err(ApiError::internal)
}

/// `GET …/templates/{id}/versions`: every version ever saved, newest first.
async fn versions<R: TemplateKind>(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<Vec<VersionInfo>>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(R::SCOPE, &params, &snap)?;
    if !R::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("template"))?;
    load::<R>(&st, id, &scope).await?;
    let mut tx = st.store.begin().await?;
    let rows = tx.template_versions(id).await?;
    rows.into_iter().map(serde_json::from_value).collect::<Result<Vec<_>, _>>().map(Json).map_err(ApiError::internal)
}

/// A template as a file: what `POST …/templates` takes, with its models by their ids, so that
/// it can be created in another workspace, organisation or installation (concept section 7.3).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TemplateFile {
    /// `pacod-gw-template/1`.
    pub format: String,
    pub name: String,
    pub description: String,
    pub spec: TemplateSpec,
}

/// `GET …/templates/{id}/export`.
async fn export<R: TemplateKind>(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<TemplateFile>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(R::SCOPE, &params, &snap)?;
    if !R::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("template"))?;
    let obj = load::<R>(&st, id, &scope).await?;
    let t = obj.doc.template();
    let mut spec = t.current().map(|v| v.spec.clone()).ok_or_else(|| ApiError::not_found("version"))?;
    // Object ids mean nothing elsewhere; model ids do.
    let named = |r: &str| snap.models.get(r).map_or_else(|| r.to_owned(), |m| m.doc.name.clone());
    spec.model = named(&spec.model);
    for entry in &mut spec.models {
        entry.model = named(&entry.model);
    }
    Ok(Json(TemplateFile {
        format: "pacod-gw-template/1".into(),
        name: t.name.clone(),
        description: t.description.clone(),
        spec,
    }))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RollbackInput {
    /// The version to make current again.
    pub version: u32,
}

async fn rollback<R: TemplateKind>(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(input): Json<RollbackInput>,
) -> ApiResult<Json<Item<TemplateView>>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(R::SCOPE, &params, &snap)?;
    if !R::can_write(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get("id").ok_or_else(|| ApiError::not_found("template"))?;
    let mut obj = load::<R>(&st, id, &scope).await?;
    let t = obj.doc.template_mut();
    if !t.versions.iter().any(|v| v.version == input.version) {
        // An older version comes back into the document from where all of them are kept.
        let old = archived(&st, id, input.version).await?.ok_or_else(|| ApiError::not_found("version"))?;
        t.versions.push(old);
        t.versions.sort_by_key(|v| v.version);
    }
    t.current_version = input.version;
    let name = t.name.clone();
    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(scope.org.as_deref(), scope.ws.as_deref());
    audit.action = format!("rollback:{}", input.version);
    let row = tx
        .update(
            &obj.id,
            Some(obj.version),
            Some(&name),
            &serde_json::to_value(&obj.doc).map_err(ApiError::internal)?,
            audit,
        )
        .await?;
    st.commit(tx).await?;
    let obj: Obj<R> = row.into_obj()?;
    Ok(Json(Item { meta: obj.meta(), data: R::view(&obj) }))
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
    check_spec(&snap, At::workspace(&snap, target)?, &mut spec, None)?;
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
    router = register::<OrgTemplate>(router, api, Ops::ALL);
    let base = collection_path(ScopeKind::Workspace, Template::PLURAL);
    let one = format!("{base}/{{id}}");
    let view = api.schema::<Item<TemplateView>>();
    let clone_in = api.schema::<CloneInput>();
    let question = api.schema::<RouteQuestion>();
    let explain_schema = api.schema::<RouteExplain>();
    api.add(
        api.op("post", &format!("{one}/route"), "explainTemplateRoute", "templates", "How routing would treat a call")
            .body(question)
            .ok(explain_schema),
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
    let inherited_schema = api.schema::<InheritedTemplate>();
    let inherited_path = collection_path(ScopeKind::Workspace, "inherited-templates");
    api.add(
        api.op(
            "get",
            &inherited_path,
            "listInheritedTemplates",
            "templates",
            "The organisation's templates, as this workspace inherits them",
        )
        .ok(serde_json::json!({ "type": "array", "items": inherited_schema })),
    );
    router = router
        .route(&format!("{one}/route"), post(explain))
        .route(&format!("{one}/clone"), post(clone_template))
        .route(&inherited_path, get(inherited));
    router = template_routes::<Template>(router, api, "Template");
    template_routes::<OrgTemplate>(router, api, "OrgTemplate")
}

/// The endpoints both kinds of template have besides the generic ones.
fn template_routes<R: TemplateKind>(router: Router<AdminState>, api: &mut OpenApi, name: &str) -> Router<AdminState> {
    let one = format!("{}/{{id}}", collection_path(R::SCOPE, R::PLURAL));
    let view = api.schema::<Item<TemplateView>>();
    let version_schema = api.schema::<TemplateVersion>();
    let check_schema = api.schema::<TemplateCheck>();
    let rollback_in = api.schema::<RollbackInput>();
    let info = api.schema::<VersionInfo>();
    let file = api.schema::<TemplateFile>();
    api.add(
        api.op(
            "get",
            &format!("{one}/check"),
            &format!("check{name}"),
            "templates",
            "Which of the template's values its models do not accept, and its routing",
        )
        .ok(check_schema),
    );
    api.add(
        api.op(
            "get",
            &format!("{one}/versions/{{version}}"),
            &format!("get{name}Version"),
            "templates",
            "One saved version",
        )
        .ok(version_schema),
    );
    api.add(
        api.op(
            "get",
            &format!("{one}/versions"),
            &format!("list{name}Versions"),
            "templates",
            "Every version, newest first",
        )
        .ok(serde_json::json!({ "type": "array", "items": info })),
    );
    api.add(
        api.op(
            "get",
            &format!("{one}/export"),
            &format!("export{name}"),
            "templates",
            "The template as a file that POST …/templates takes, elsewhere",
        )
        .ok(file),
    );
    api.add(
        api.op(
            "post",
            &format!("{one}/rollback"),
            &format!("rollback{name}"),
            "templates",
            "Make an earlier version current",
        )
        .body(rollback_in)
        .ok(view),
    );
    router
        .route(&format!("{one}/check"), get(check::<R>))
        .route(&format!("{one}/versions/{{version}}"), get(version::<R>))
        .route(&format!("{one}/versions"), get(versions::<R>))
        .route(&format!("{one}/export"), get(export::<R>))
        .route(&format!("{one}/rollback"), post(rollback::<R>))
}
