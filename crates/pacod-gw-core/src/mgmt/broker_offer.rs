//! What an organisation offers on pacod-broker, in one place (runbook section 20.7): its
//! granted models, by the grant's `broker_offer`, and its templates, the organisation's own and
//! its workspaces', by the template's `broker_offer`. gw-broker reads this and announces what is
//! offered under the organisation's broker token. The console's Broker page changes several
//! offers at once; each one is still stored where it was, so the template page and the grant
//! agree with it.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::AdminState;
use super::auth::Principal;
use super::crud::{BuildCtx, Resource, ScopeIds};
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use super::resources::repo::{offered_keys_clash, resolve_model_ref};
use super::resources::template::{TemplateInput, TemplateKind};
use crate::model::{
    BrokerOffer, Doc, ModelKind, ModelState, Obj, OrgGrant, OrgTemplate, Status, Template, Workspace, broker_model_key,
};
use crate::snapshot::Snapshot;
use crate::store::{Row, Tx};

/// A model granted to the organisation, and whether it is offered.
#[derive(Debug, Serialize, JsonSchema)]
pub struct OfferedModel {
    /// The grant's id.
    pub grant_id: String,
    pub model_id: String,
    /// The name callers use, and the broker's resource name in lower case.
    pub name: String,
    pub kind: ModelKind,
    pub state: ModelState,
    /// Set while the model is offered.
    pub offer: Option<BrokerOffer>,
    /// The broker resource key it is offered under; none when its name cannot be one.
    pub key: Option<String>,
    /// The workspaces, by id, whose keys may call it: each one's grants, if it has its own,
    /// include the model. Offered to none, it is not announced.
    pub reached_by: Vec<String>,
}

/// A workspace, by id and name.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WorkspaceRef {
    pub id: String,
    pub name: String,
}

/// A template of the organisation or of one of its workspaces, and whether it is offered.
#[derive(Debug, Serialize, JsonSchema)]
pub struct OfferedTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: Status,
    /// The workspace whose template it is; none for the organisation's own.
    pub workspace: Option<WorkspaceRef>,
    pub current_version: u32,
    /// Set while the template is offered (its current version's `broker_offer`).
    pub offer: Option<BrokerOffer>,
    /// The broker resource key: `<workspace>.<template>`, or `org:<template>`.
    pub key: String,
    /// The workspaces, by id, whose keys reach it as `@name`: its own workspace, or each of the
    /// organisation's that has no template of that name of its own.
    pub reached_by: Vec<String>,
}

/// What an organisation can offer on pacod-broker, and what it offers.
#[derive(Debug, Serialize, JsonSchema)]
pub struct BrokerOfferView {
    /// Every model granted to the organisation, by name.
    pub models: Vec<OfferedModel>,
    /// The organisation's templates first, then each workspace's, by name.
    pub templates: Vec<OfferedTemplate>,
}

/// One model's offer to set; an `offer` that is null or left out withdraws it.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelOfferChange {
    /// The model's id or name: a model granted to the organisation.
    pub model_id: String,
    pub offer: Option<BrokerOffer>,
}

/// One template's offer to set; an `offer` that is null or left out withdraws it.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TemplateOfferChange {
    /// The template's id: one of the organisation's or of one of its workspaces.
    pub id: String,
    pub offer: Option<BrokerOffer>,
}

/// Offers to change. What is not named stays as it is, so a page read earlier does not undo
/// a change made since.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrokerOfferChange {
    #[serde(default)]
    pub models: Vec<ModelOfferChange>,
    #[serde(default)]
    pub templates: Vec<TemplateOfferChange>,
}

fn objs<T: Doc>(rows: Vec<Row>) -> ApiResult<Vec<Obj<T>>> {
    rows.into_iter().map(|r| r.into_obj().map_err(ApiError::from)).collect()
}

/// The error of one item, with the item named: a change of several fails as a whole.
fn about(mut e: ApiError, what: &str) -> ApiError {
    e.message = format!("{what}: {}", e.message);
    e
}

/// Reads the organisation's offers in `tx`, so that a change answers with what it wrote.
async fn read(tx: &mut Tx, snap: &Snapshot, org: &str) -> ApiResult<BrokerOfferView> {
    let mut workspaces: Vec<Obj<Workspace>> = objs(tx.list(Workspace::KIND, Some(org)).await?)?;
    workspaces.sort_by(|a, b| a.doc.name.cmp(&b.doc.name));
    let mut own: HashMap<String, Vec<Obj<Template>>> = HashMap::new();
    for ws in &workspaces {
        own.insert(ws.id.clone(), objs(tx.list(Template::KIND, Some(&ws.id)).await?)?);
    }

    let mut models = Vec::new();
    for g in objs::<OrgGrant>(tx.list(OrgGrant::KIND, Some(org)).await?)? {
        let Some(m) = snap.models.get(&g.doc.model_id) else { continue };
        let reached_by = workspaces
            .iter()
            .filter(|w| snap.workspaces.get(&w.id).is_some_and(|rt| snap.grant_terms(rt, &m.id).is_some()))
            .map(|w| w.id.clone())
            .collect();
        models.push(OfferedModel {
            grant_id: g.id,
            model_id: m.id.clone(),
            name: m.doc.name.clone(),
            kind: m.doc.kind,
            state: m.doc.state,
            offer: g.doc.broker_offer,
            key: broker_model_key(&m.doc.name),
            reached_by,
        });
    }
    models.sort_by_cached_key(|m| m.name.to_ascii_lowercase());

    let mut shared: Vec<Obj<OrgTemplate>> = objs(tx.list(OrgTemplate::KIND, Some(org)).await?)?;
    shared.sort_by(|a, b| a.doc.0.name.cmp(&b.doc.0.name));
    let mut templates = Vec::new();
    for t in &shared {
        let name = &t.doc.0.name;
        // A workspace's own template of the same name hides the organisation's there, also
        // while it is disabled, as gw-broker's choice of offerings has it.
        let reached_by = workspaces
            .iter()
            .filter(|w| !own.get(&w.id).is_some_and(|ts| ts.iter().any(|o| o.doc.name == *name)))
            .map(|w| w.id.clone())
            .collect();
        templates.push(offered_template(t.id.clone(), t.doc.template(), None, format!("org:{name}"), reached_by));
    }
    for ws in &workspaces {
        let mut list: Vec<&Obj<Template>> = own.get(&ws.id).map(|v| v.iter().collect()).unwrap_or_default();
        list.sort_by(|a, b| a.doc.name.cmp(&b.doc.name));
        for t in list {
            let at = WorkspaceRef { id: ws.id.clone(), name: ws.doc.name.clone() };
            let key = format!("{}.{}", ws.doc.name, t.doc.name);
            templates.push(offered_template(t.id.clone(), &t.doc, Some(at), key, vec![ws.id.clone()]));
        }
    }
    Ok(BrokerOfferView { models, templates })
}

fn offered_template(
    id: String,
    t: &Template,
    workspace: Option<WorkspaceRef>,
    key: String,
    reached_by: Vec<String>,
) -> OfferedTemplate {
    OfferedTemplate {
        id,
        name: t.name.clone(),
        description: t.description.clone(),
        status: t.status,
        workspace,
        current_version: t.current_version,
        offer: t.current().and_then(|v| v.spec.broker_offer.clone()),
        key,
        reached_by,
    }
}

async fn view(State(st): State<AdminState>, p: Principal, Path(org): Path<String>) -> ApiResult<Json<BrokerOfferView>> {
    if !p.sees_org(&org) {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    if !snap.organisations.contains_key(&org) {
        return Err(ApiError::not_found("organisation"));
    }
    let mut tx = st.store.begin().await?;
    Ok(Json(read(&mut tx, &snap, &org).await?))
}

/// Writes a template's new version with `offer` as its `broker_offer`, through the template's
/// own rules. Nothing is written when the offer is already so.
async fn offer_template<R: TemplateKind>(
    tx: &mut Tx,
    ctx: &BuildCtx<'_>,
    p: &Principal,
    obj: Obj<R>,
    offer: &Option<BrokerOffer>,
) -> ApiResult<()> {
    let t = obj.doc.template();
    let current = t.current().ok_or_else(|| ApiError::invalid(format!("template {}: no current version", t.name)))?;
    if current.spec.broker_offer == *offer {
        return Ok(());
    }
    let mut spec = current.spec.clone();
    spec.broker_offer = offer.clone();
    let input = TemplateInput {
        name: t.name.clone(),
        description: t.description.clone(),
        spec,
        comment: if offer.is_some() { "offered on pacod-broker" } else { "withdrawn from pacod-broker" }.into(),
        status: t.status,
    };
    let name = t.name.clone();
    let doc = R::build(input, Some(&obj), ctx).map_err(|e| about(e, &format!("template {name}")))?;
    let mut audit = p.audit(ctx.scope.org.as_deref(), ctx.scope.ws.as_deref());
    audit.action = if offer.is_some() { "offer_on_broker" } else { "withdraw_from_broker" }.into();
    audit.detail = json!({ "template": name, "offer": offer });
    let value = serde_json::to_value(&doc).map_err(ApiError::internal)?;
    tx.update(&obj.id, Some(obj.version), R::unique_name(&doc).as_deref(), &value, audit).await?;
    Ok(())
}

/// Offers or withdraws several models and templates at once, in one transaction: an item
/// that cannot be changed fails the whole change, and nothing is written.
async fn change(
    State(st): State<AdminState>,
    p: Principal,
    Path(org): Path<String>,
    Json(input): Json<BrokerOfferChange>,
) -> ApiResult<Json<BrokerOfferView>> {
    // Model offers live on the organisation's grants, which only the platform writes.
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    if !snap.organisations.contains_key(&org) {
        return Err(ApiError::not_found("organisation"));
    }
    let mut named = HashSet::new();
    for n in input.models.iter().map(|c| &c.model_id).chain(input.templates.iter().map(|c| &c.id)) {
        if !named.insert(n) {
            return Err(ApiError::invalid(format!("{n} is named twice")));
        }
    }

    let mut tx = st.store.begin().await?;
    let org_scope = ScopeIds { org: Some(org.clone()), ws: None };
    let grants: Vec<Obj<OrgGrant>> = objs(tx.list(OrgGrant::KIND, Some(&org)).await?)?;
    for c in &input.models {
        let id = resolve_model_ref(&snap, &c.model_id);
        let Some(g) = id.and_then(|id| grants.iter().find(|g| g.doc.model_id == id)) else {
            return Err(ApiError::invalid(format!("the model \"{}\" is not granted to the organisation", c.model_id)));
        };
        if g.doc.broker_offer == c.offer {
            continue;
        }
        let name = snap.models.get(&g.doc.model_id).map_or(c.model_id.clone(), |m| m.doc.name.clone());
        let mut doc = g.doc.clone();
        doc.broker_offer = c.offer.clone();
        let ctx = BuildCtx { st: &st, snap: &snap, scope: &org_scope, id: &g.id, actor: &p.id, platform_admin: true };
        let doc = OrgGrant::build(doc, Some(g), &ctx).map_err(|e| about(e, &format!("model {name}")))?;
        let mut audit = p.audit(Some(&org), None);
        audit.action = if c.offer.is_some() { "offer_on_broker" } else { "withdraw_from_broker" }.into();
        audit.detail = json!({ "model": name, "offer": c.offer });
        let value = serde_json::to_value(&doc).map_err(ApiError::internal)?;
        tx.update(&g.id, Some(g.version), OrgGrant::unique_name(&doc).as_deref(), &value, audit).await?;
    }
    // Checked once all are written: a change may withdraw one model and offer another.
    offered_keys_clash(&mut tx, &snap, &org, None).await?;

    let workspaces: HashSet<String> =
        objs::<Workspace>(tx.list(Workspace::KIND, Some(&org)).await?)?.into_iter().map(|w| w.id).collect();
    for c in &input.templates {
        let not_ours = || ApiError::invalid(format!("the template {} is not the organisation's", c.id));
        let row = tx.get(&c.id).await?.ok_or_else(not_ours)?;
        if row.kind == OrgTemplate::KIND && row.parent.as_deref() == Some(org.as_str()) {
            let ctx =
                BuildCtx { st: &st, snap: &snap, scope: &org_scope, id: &c.id, actor: &p.id, platform_admin: true };
            offer_template::<OrgTemplate>(&mut tx, &ctx, &p, row.into_obj()?, &c.offer).await?;
        } else if row.kind == Template::KIND && row.parent.as_ref().is_some_and(|w| workspaces.contains(w)) {
            let scope = ScopeIds { org: Some(org.clone()), ws: row.parent.clone() };
            let ctx = BuildCtx { st: &st, snap: &snap, scope: &scope, id: &c.id, actor: &p.id, platform_admin: true };
            offer_template::<Template>(&mut tx, &ctx, &p, row.into_obj()?, &c.offer).await?;
        } else {
            return Err(not_ours());
        }
    }

    let answer = read(&mut tx, &snap, &org).await?;
    st.commit(tx).await?;
    Ok(Json(answer))
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let view_schema = api.schema::<BrokerOfferView>();
    let change_schema = api.schema::<BrokerOfferChange>();
    api.add(
        api.op(
            "get",
            "/orgs/{org}/broker-offer",
            "orgBrokerOffer",
            "broker",
            "The organisation's models and templates, and which of them it offers on pacod-broker",
        )
        .ok(view_schema.clone()),
    );
    api.add(
        api.op(
            "post",
            "/orgs/{org}/broker-offer",
            "changeOrgBrokerOffer",
            "broker",
            "Offer or withdraw several of the organisation's models and templates on pacod-broker at once",
        )
        .body(change_schema)
        .ok(view_schema),
    );
    router.route("/orgs/{org}/broker-offer", get(view).post(change))
}
