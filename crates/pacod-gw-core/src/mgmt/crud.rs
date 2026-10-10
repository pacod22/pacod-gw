//! Generic handlers for the configuration resources.
//!
//! Most resources of the management API behave the same way: list, create, read, replace,
//! delete, scoped to the installation, an organisation or a workspace, with role checks,
//! optimistic concurrency, an audit entry and a configuration version increment. A resource
//! implements [`Resource`] to describe what is specific to it; everything else is here.

use std::collections::HashMap;
use std::future::Future;

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::AdminState;
use super::auth::Principal;
use super::error::{ApiError, ApiResult};
use super::openapi::{OpenApi, page_schema};
use crate::model::{Doc, Meta, Obj};
use crate::snapshot::Snapshot;
use crate::store::{ParentFilter, Tx};

/// Where a resource lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// `/{plural}`
    Global,
    /// `/orgs/{org}/{plural}`
    Organisation,
    /// `/orgs/{org}/workspaces/{ws}/{plural}`
    Workspace,
}

/// The organisation and workspace a request addresses, validated against the snapshot.
#[derive(Debug, Clone, Default)]
pub struct ScopeIds {
    pub org: Option<String>,
    pub ws: Option<String>,
}

impl ScopeIds {
    pub fn org(&self) -> &str {
        self.org.as_deref().unwrap_or_default()
    }

    pub fn ws(&self) -> &str {
        self.ws.as_deref().unwrap_or_default()
    }
}

/// What `Resource::build` may look at.
pub struct BuildCtx<'a> {
    pub st: &'a AdminState,
    pub snap: &'a Snapshot,
    pub scope: &'a ScopeIds,
    /// The id of the object being created or updated.
    pub id: &'a str,
    /// The principal making the change.
    pub actor: &'a str,
    /// Whether that principal is a platform admin, for fields only the platform may change.
    pub platform_admin: bool,
}

pub trait Resource: Doc + Sized {
    /// The path segment, for example `providers`.
    const PLURAL: &'static str;
    /// The word used in messages, for example `provider`.
    const LABEL: &'static str;
    const SCOPE: ScopeKind;
    /// The name of the path parameter that carries this resource's id. Parameters at the same
    /// position of two routes must share a name, so organisations use `org` and workspaces
    /// `ws`, the names their child routes use.
    const ID_PARAM: &'static str = "id";
    /// Whether a field the input does not know, inside one of its members, is refused (`422`)
    /// instead of ignored. Unknown top-level fields are always ignored, so that an object read
    /// from the API can be sent back with its view's fields. Stored documents are always read
    /// leniently, so that an instance of the previous release keeps an object a newer one
    /// wrote with a field it adds; the strictness is here instead.
    const STRICT: bool = false;
    /// What the API accepts on create and replace.
    type Input: DeserializeOwned + JsonSchema + Send + 'static;
    /// What the API returns. Never contains secrets.
    type View: Serialize + JsonSchema + Send + 'static;

    /// The value that must be unique within the parent, if any.
    fn unique_name(doc: &Self) -> Option<String>;

    /// Validates the input and produces the document to store. `existing` is set on replace.
    fn build(input: Self::Input, existing: Option<&Obj<Self>>, ctx: &BuildCtx<'_>) -> ApiResult<Self>;

    fn view(obj: &Obj<Self>) -> Self::View;

    /// Checks that need the database, run inside the change's transaction.
    fn check(
        _tx: &mut Tx,
        _doc: &Self,
        _existing: Option<&Obj<Self>>,
        _ctx: &BuildCtx<'_>,
    ) -> impl Future<Output = ApiResult<()>> + Send {
        async { Ok(()) }
    }

    /// Runs after a create or replace was committed, for effects outside the database such as
    /// an email. `existing` is the object before a replace.
    fn after_commit(
        _st: &AdminState,
        _obj: &Obj<Self>,
        _existing: Option<&Obj<Self>>,
    ) -> impl Future<Output = ()> + Send {
        async {}
    }

    /// Refuses deletion while something depends on the object.
    fn before_delete(_tx: &mut Tx, _obj: &Obj<Self>) -> impl Future<Output = ApiResult<()>> + Send {
        async { Ok(()) }
    }

    fn can_read(p: &Principal, scope: &ScopeIds, _snap: &Snapshot) -> bool {
        match Self::SCOPE {
            ScopeKind::Global => true,
            ScopeKind::Organisation => p.can_read_org(scope.org()),
            ScopeKind::Workspace => p.can_read_workspace(scope.ws(), scope.org()),
        }
    }

    fn can_write(p: &Principal, scope: &ScopeIds, _snap: &Snapshot) -> bool {
        match Self::SCOPE {
            ScopeKind::Global => p.is_platform_admin(),
            ScopeKind::Organisation => p.can_admin_org(scope.org()),
            ScopeKind::Workspace => p.can_admin_workspace(scope.ws(), scope.org()),
        }
    }

    /// Whether the principal may replace the object `id`. By default whoever may create one.
    fn can_replace(p: &Principal, scope: &ScopeIds, _id: &str, snap: &Snapshot) -> bool {
        Self::can_write(p, scope, snap)
    }

    /// A per-object check on top of `can_write`, for resources whose permission depends on
    /// what the object says (a role binding names the scope it applies to).
    fn authorize(_p: &Principal, _doc: &Self, _snap: &Snapshot) -> bool {
        true
    }

    /// Hides single objects from a principal who may read the collection, for example
    /// organisations the principal has no role in.
    fn visible(_p: &Principal, _obj: &Obj<Self>, _snap: &Snapshot) -> bool {
        true
    }
}

/// An object as the API returns it: the common columns plus the resource's view.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Item<V> {
    #[serde(flatten)]
    pub meta: Meta,
    #[serde(flatten)]
    pub data: V,
}

#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

impl ListQuery {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(100).clamp(1, 500)
    }
}

/// Path parameters as a map. Unlike `Path`, this also works on routes without parameters.
pub struct Params(pub HashMap<String, String>);

impl<S: Send + Sync> FromRequestParts<S> for Params {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Ok(Params(
            Path::<HashMap<String, String>>::from_request_parts(parts, state).await.map(|p| p.0).unwrap_or_default(),
        ))
    }
}

/// Validates the organisation and workspace named in the path.
pub fn resolve_scope(kind: ScopeKind, params: &HashMap<String, String>, snap: &Snapshot) -> ApiResult<ScopeIds> {
    let mut scope = ScopeIds::default();
    if kind == ScopeKind::Global {
        return Ok(scope);
    }
    let org = params.get("org").ok_or_else(|| ApiError::not_found("organisation"))?;
    if !snap.organisations.contains_key(org) {
        return Err(ApiError::not_found("organisation"));
    }
    scope.org = Some(org.clone());
    if kind == ScopeKind::Workspace {
        let ws = params.get("ws").ok_or_else(|| ApiError::not_found("workspace"))?;
        match snap.workspaces.get(ws) {
            Some(w) if &w.organisation_id == org => scope.ws = Some(ws.clone()),
            _ => return Err(ApiError::not_found("workspace")),
        }
    }
    Ok(scope)
}

/// The parent column for objects of a scope.
pub fn parent_of(kind: ScopeKind, scope: &ScopeIds) -> Option<String> {
    match kind {
        ScopeKind::Global => None,
        ScopeKind::Organisation => scope.org.clone(),
        ScopeKind::Workspace => scope.ws.clone(),
    }
}

fn item<R: Resource>(obj: &Obj<R>) -> Item<R::View> {
    Item { meta: obj.meta(), data: R::view(obj) }
}

/// Reads an object of this resource within the scope, or answers 404.
pub async fn load<R: Resource>(st: &AdminState, id: &str, scope: &ScopeIds) -> ApiResult<Obj<R>> {
    let obj: Obj<R> = st.store.get_obj(id).await.map_err(|_| ApiError::not_found(R::LABEL))?;
    if obj.parent != parent_of(R::SCOPE, scope) {
        return Err(ApiError::not_found(R::LABEL));
    }
    Ok(obj)
}

fn parse_input<I: DeserializeOwned>(mut body: Value, strict: bool) -> ApiResult<(I, Option<i32>)> {
    let version = body
        .as_object_mut()
        .and_then(|o| o.remove("version"))
        .and_then(|v| v.as_i64())
        .and_then(|v| i32::try_from(v).ok());
    // The common columns are returned by the API; accept them back and ignore them.
    if let Some(o) = body.as_object_mut() {
        for k in ["id", "created_at", "updated_at"] {
            o.remove(k);
        }
    }
    if !strict {
        let input = serde_json::from_value(body).map_err(|e| ApiError::bad_request(e.to_string()))?;
        return Ok((input, version));
    }
    let mut unknown = Vec::new();
    let input = serde_ignored::deserialize(body, |path| {
        let path = path.to_string();
        if path.contains('.') {
            unknown.push(path);
        }
    })
    .map_err(|e: serde_json::Error| ApiError::bad_request(e.to_string()))?;
    if !unknown.is_empty() {
        return Err(ApiError::invalid(format!("unknown field: {}", unknown.join(", "))));
    }
    Ok((input, version))
}

async fn list<R: Resource>(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Page<Item<R::View>>>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(R::SCOPE, &params, &snap)?;
    if !R::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let parent = parent_of(R::SCOPE, &scope);
    let filter = match &parent {
        Some(pid) => ParentFilter::Is(pid),
        None => ParentFilter::None,
    };
    let limit = q.limit();
    let rows = st.store.list(R::KIND, filter, q.cursor.as_deref(), limit + 1).await?;
    let more = rows.len() as i64 > limit;
    let mut items = Vec::new();
    let mut last = None;
    for row in rows.into_iter().take(limit as usize) {
        last = Some(row.id.clone());
        let obj: Obj<R> = row.into_obj()?;
        if R::visible(&p, &obj, &snap) {
            items.push(item(&obj));
        }
    }
    Ok(Json(Page { items, next_cursor: if more { last } else { None } }))
}

async fn create<R: Resource>(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(body): Json<Value>,
) -> ApiResult<(StatusCode, Json<Item<R::View>>)> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(R::SCOPE, &params, &snap)?;
    if !R::can_write(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let (input, _) = parse_input::<R::Input>(body, R::STRICT)?;
    let id = crate::model::new_id(R::PREFIX);
    let ctx =
        BuildCtx { st: &st, snap: &snap, scope: &scope, id: &id, actor: &p.id, platform_admin: p.is_platform_admin() };
    let doc = R::build(input, None, &ctx)?;
    if !R::authorize(&p, &doc, &snap) {
        return Err(ApiError::forbidden());
    }

    let mut tx = st.store.begin().await?;
    R::check(&mut tx, &doc, None, &ctx).await?;
    let value = serde_json::to_value(&doc).map_err(ApiError::internal)?;
    let row = tx
        .create(
            R::KIND,
            &id,
            parent_of(R::SCOPE, &scope).as_deref(),
            R::unique_name(&doc).as_deref(),
            &value,
            p.audit(scope.org.as_deref(), scope.ws.as_deref()),
        )
        .await?;
    st.commit(tx).await?;
    let obj = row.into_obj()?;
    R::after_commit(&st, &obj, None).await;
    Ok((StatusCode::CREATED, Json(item::<R>(&obj))))
}

async fn read<R: Resource>(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<Json<Item<R::View>>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(R::SCOPE, &params, &snap)?;
    if !R::can_read(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get(R::ID_PARAM).ok_or_else(|| ApiError::not_found(R::LABEL))?;
    let obj = load::<R>(&st, id, &scope).await?;
    if !R::visible(&p, &obj, &snap) {
        return Err(ApiError::not_found(R::LABEL));
    }
    Ok(Json(item(&obj)))
}

async fn replace<R: Resource>(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
    Json(body): Json<Value>,
) -> ApiResult<Json<Item<R::View>>> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(R::SCOPE, &params, &snap)?;
    let id = params.get(R::ID_PARAM).ok_or_else(|| ApiError::not_found(R::LABEL))?.clone();
    if !R::can_replace(&p, &scope, &id, &snap) {
        return Err(ApiError::forbidden());
    }
    let (input, expected_version) = parse_input::<R::Input>(body, R::STRICT)?;
    let existing = load::<R>(&st, &id, &scope).await?;
    let ctx =
        BuildCtx { st: &st, snap: &snap, scope: &scope, id: &id, actor: &p.id, platform_admin: p.is_platform_admin() };
    let doc = R::build(input, Some(&existing), &ctx)?;
    if !R::authorize(&p, &existing.doc, &snap) || !R::authorize(&p, &doc, &snap) {
        return Err(ApiError::forbidden());
    }

    let mut tx = st.store.begin().await?;
    R::check(&mut tx, &doc, Some(&existing), &ctx).await?;
    let value = serde_json::to_value(&doc).map_err(ApiError::internal)?;
    let row = tx
        .update(
            &id,
            expected_version,
            R::unique_name(&doc).as_deref(),
            &value,
            p.audit(scope.org.as_deref(), scope.ws.as_deref()),
        )
        .await?;
    st.commit(tx).await?;
    let obj = row.into_obj()?;
    R::after_commit(&st, &obj, Some(&existing)).await;
    Ok(Json(item::<R>(&obj)))
}

async fn remove<R: Resource>(
    State(st): State<AdminState>,
    p: Principal,
    Params(params): Params,
) -> ApiResult<StatusCode> {
    let snap = st.config.snapshot();
    let scope = resolve_scope(R::SCOPE, &params, &snap)?;
    if !R::can_write(&p, &scope, &snap) {
        return Err(ApiError::forbidden());
    }
    let id = params.get(R::ID_PARAM).ok_or_else(|| ApiError::not_found(R::LABEL))?;
    let existing = load::<R>(&st, id, &scope).await?;
    if !R::authorize(&p, &existing.doc, &snap) {
        return Err(ApiError::forbidden());
    }
    let mut tx = st.store.begin().await?;
    R::before_delete(&mut tx, &existing).await?;
    tx.delete(id, p.audit(scope.org.as_deref(), scope.ws.as_deref())).await?;
    st.commit(tx).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The collection path of a resource, in axum and OpenAPI syntax (they are the same).
pub fn collection_path(scope: ScopeKind, plural: &str) -> String {
    match scope {
        ScopeKind::Global => format!("/{plural}"),
        ScopeKind::Organisation => format!("/orgs/{{org}}/{plural}"),
        ScopeKind::Workspace => format!("/orgs/{{org}}/workspaces/{{ws}}/{plural}"),
    }
}

/// Which of the generic operations a resource exposes.
#[derive(Debug, Clone, Copy)]
pub struct Ops {
    pub list: bool,
    pub create: bool,
    pub read: bool,
    pub replace: bool,
    pub delete: bool,
}

impl Ops {
    pub const ALL: Ops = Ops { list: true, create: true, read: true, replace: true, delete: true };
    /// For resources whose creation returns a secret and therefore has its own handler.
    pub const NO_CREATE: Ops = Ops { create: false, ..Ops::ALL };
}

/// Registers the generic routes of `R` and describes them in the OpenAPI document.
pub fn register<R: Resource>(router: Router<AdminState>, api: &mut OpenApi, ops: Ops) -> Router<AdminState> {
    let coll = collection_path(R::SCOPE, R::PLURAL);
    let one = format!("{coll}/{{{}}}", R::ID_PARAM);
    let tag = R::PLURAL;
    let name = pascal(R::KIND);
    let view = api.schema::<Item<R::View>>();
    let input = api.schema::<R::Input>();

    let mut coll_routes = axum::routing::MethodRouter::new();
    if ops.list {
        coll_routes = coll_routes.get(list::<R>);
        api.add(
            api.op("get", &coll, &format!("list{name}"), tag, &format!("List {}", R::PLURAL.replace('-', " ")))
                .query("cursor", "Continue a listing: the `next_cursor` of the previous page.")
                .query_int("limit", "Page size, 1 to 500. Default 100.")
                .ok(page_schema(view.clone())),
        );
    }
    if ops.create {
        coll_routes = coll_routes.post(create::<R>);
        api.add(
            api.op("post", &coll, &format!("create{name}"), tag, &format!("Create a {}", R::LABEL))
                .body(input.clone())
                .created(view.clone()),
        );
    }
    let mut one_routes = axum::routing::MethodRouter::new();
    if ops.read {
        one_routes = one_routes.get(read::<R>);
        api.add(api.op("get", &one, &format!("get{name}"), tag, &format!("Get a {}", R::LABEL)).ok(view.clone()));
    }
    if ops.replace {
        one_routes = one_routes.put(replace::<R>);
        api.add(
            api.op("put", &one, &format!("replace{name}"), tag, &format!("Replace a {}", R::LABEL))
                .body_with_version(input)
                .ok(view),
        );
    }
    if ops.delete {
        one_routes = one_routes.delete(remove::<R>);
        api.add(api.op("delete", &one, &format!("delete{name}"), tag, &format!("Delete a {}", R::LABEL)).no_content());
    }
    router.route(&coll, coll_routes).route(&one, one_routes)
}

/// `api_key` → `ApiKey`
pub fn pascal(s: &str) -> String {
    s.split(['_', '-'])
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_per_scope() {
        assert_eq!(collection_path(ScopeKind::Global, "providers"), "/providers");
        assert_eq!(collection_path(ScopeKind::Organisation, "workspaces"), "/orgs/{org}/workspaces");
        assert_eq!(collection_path(ScopeKind::Workspace, "keys"), "/orgs/{org}/workspaces/{ws}/keys");
    }

    #[test]
    fn pascal_case() {
        assert_eq!(pascal("api_key"), "ApiKey");
        assert_eq!(pascal("provider"), "Provider");
    }

    #[test]
    fn input_parsing_separates_the_version_and_ignores_common_columns() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct In {
            name: String,
        }
        let (input, v) = parse_input::<In>(
            serde_json::json!({
                "id": "x_1", "version": 3, "created_at": "2026-01-01T00:00:00Z", "name": "a"
            }),
            true,
        )
        .unwrap();
        assert_eq!((input, v), (In { name: "a".into() }, Some(3)));
        assert!(parse_input::<In>(serde_json::json!({"nope": 1}), false).is_err());
        // Strict: a field a member does not know is refused; one at the top is a view's field
        // sent back, and ignored.
        #[derive(Deserialize, Debug)]
        struct Outer {
            #[allow(dead_code)]
            inner: In,
        }
        let e = parse_input::<Outer>(serde_json::json!({"inner": {"name": "a", "extra": 1}}), true).unwrap_err();
        assert!(e.message.contains("inner.extra"), "{}", e.message);
        assert!(parse_input::<Outer>(serde_json::json!({"inner": {"name": "a"}, "versions": []}), true).is_ok());
        assert!(parse_input::<Outer>(serde_json::json!({"inner": {"name": "a", "extra": 1}}), false).is_ok());
    }
}
