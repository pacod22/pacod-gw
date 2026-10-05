//! Configuration export and import (concept section 17): the configuration as one JSON
//! document, to carry it to another installation or to keep it under version control.
//!
//! What is exported is what describes the gateway's behaviour: providers, models, prices,
//! organisations, workspaces, grants, templates, limits, the price job and the settings.
//! What is left out is everything secret or personal: provider credentials, API keys, users,
//! admin tokens and role bindings. Those are entered again where the document lands.

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::AdminState;
use super::auth::Principal;
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use crate::model::{
    Doc, EndUserOverride, GlobalSettings, Model, OrgGrant, OrgLimit, Organisation, Price, PriceJob, Provider, Template,
    Workspace, WorkspaceGrant, WorkspaceLimit,
};

/// The kinds that are exported, parents before children, so that an import can run in order.
const KINDS: &[&str] = &[
    GlobalSettings::KIND,
    Provider::KIND,
    Model::KIND,
    Price::KIND,
    Organisation::KIND,
    OrgGrant::KIND,
    OrgLimit::KIND,
    Workspace::KIND,
    WorkspaceGrant::KIND,
    Template::KIND,
    WorkspaceLimit::KIND,
    EndUserOverride::KIND,
    PriceJob::KIND,
];

/// Whether a document is what its kind says. An import never stores what the gateway could
/// not read back.
/// The document as the gateway itself writes it: read into its type and written back. `None`
/// when it is not a valid document of the kind.
///
/// Two documents that mean the same compare equal in this form. A document that passed
/// through other tools rarely comes back byte for byte: `10.0` becomes `10`, defaults are
/// left out or spelled out.
fn canonical(kind: &str, doc: &Value) -> Option<Value> {
    fn via<T: serde::de::DeserializeOwned + Serialize>(doc: &Value) -> Option<Value> {
        serde_json::to_value(serde_json::from_value::<T>(doc.clone()).ok()?).ok()
    }
    match kind {
        k if k == GlobalSettings::KIND => via::<GlobalSettings>(doc),
        k if k == Provider::KIND => via::<Provider>(doc),
        k if k == Model::KIND => via::<Model>(doc),
        k if k == Price::KIND => via::<Price>(doc),
        k if k == Organisation::KIND => via::<Organisation>(doc),
        k if k == OrgGrant::KIND => via::<OrgGrant>(doc),
        k if k == OrgLimit::KIND => via::<OrgLimit>(doc),
        k if k == Workspace::KIND => via::<Workspace>(doc),
        k if k == WorkspaceGrant::KIND => via::<WorkspaceGrant>(doc),
        k if k == Template::KIND => via::<Template>(doc),
        k if k == WorkspaceLimit::KIND => via::<WorkspaceLimit>(doc),
        k if k == EndUserOverride::KIND => via::<EndUserOverride>(doc),
        k if k == PriceJob::KIND => via::<PriceJob>(doc),
        _ => None,
    }
}

/// One configuration object in an export.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExportedObject {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The name the object is unique under within its parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub doc: Value,
}

/// The configuration of an installation, without secrets and without people.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ConfigExport {
    /// The format of this document.
    pub format: String,
    pub exported_at: DateTime<Utc>,
    /// The configuration version the export was taken at.
    pub config_version: i64,
    pub objects: Vec<ExportedObject>,
}

const FORMAT: &str = "pacod-gw-config/1";

async fn export(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<ConfigExport>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let (config_version, rows) = st.store.load_all().await?;
    let mut objects: Vec<ExportedObject> = rows
        .into_iter()
        .filter(|r| KINDS.contains(&r.kind.as_str()))
        .map(|r| ExportedObject { id: r.id, kind: r.kind, parent: r.parent, name: r.name, doc: r.doc })
        .collect();
    let order = |kind: &str| KINDS.iter().position(|k| *k == kind).unwrap_or(KINDS.len());
    objects.sort_by(|a, b| order(&a.kind).cmp(&order(&b.kind)).then_with(|| a.id.cmp(&b.id)));

    let mut entry = p.audit(None, None);
    entry.action = "export_configuration".into();
    entry.detail = serde_json::json!({ "objects": objects.len(), "config_version": config_version });
    st.store.audit(&entry).await?;
    Ok(Json(ConfigExport { format: FORMAT.into(), exported_at: Utc::now(), config_version, objects }))
}

#[derive(Debug, Default, Deserialize)]
struct ImportQuery {
    dry_run: Option<bool>,
}

/// What an import did, or would do.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct ImportResult {
    /// False for a dry run.
    pub applied: bool,
    pub created: Vec<String>,
    pub updated: Vec<String>,
    /// How many objects were already as the document has them.
    pub unchanged: usize,
    /// Objects that were not taken, each with the reason.
    pub refused: Vec<String>,
}

/// Brings the configuration in line with a document: objects it has and the installation
/// lacks are created, objects that differ are replaced. Nothing is deleted, and credentials,
/// keys and users are never part of it. With refusals nothing at all is changed.
async fn import(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<ImportQuery>,
    Json(input): Json<ConfigExport>,
) -> ApiResult<Json<ImportResult>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    if input.format != FORMAT {
        return Err(ApiError::invalid(format!("this is not a configuration export in the format {FORMAT}")));
    }
    if input.objects.len() > 100_000 {
        return Err(ApiError::invalid("the document holds more objects than an import takes at once"));
    }
    let dry_run = q.dry_run == Some(true);
    let mut out = ImportResult::default();
    let mut objects = input.objects;
    let order = |kind: &str| KINDS.iter().position(|k| *k == kind).unwrap_or(KINDS.len());
    objects.sort_by(|a, b| order(&a.kind).cmp(&order(&b.kind)).then_with(|| a.id.cmp(&b.id)));

    // Names are unique within a parent. A clash is found here, before anything is written:
    // a failed insert would end the transaction and with it the report on the other objects.
    let (_, rows) = st.store.load_all().await?;
    type NameKey = (String, Option<String>, String);
    let mut names: std::collections::HashMap<NameKey, String> =
        rows.iter().filter_map(|r| Some(((r.kind.clone(), r.parent.clone(), r.name.clone()?), r.id.clone()))).collect();

    let mut tx = st.store.begin().await?;
    let mut entry = p.audit(None, None);
    entry.action = "import".into();
    // An object is named with its parent: "price 2 of model openai/gpt-x". Version numbers
    // and limit names repeat across parents.
    let mut titles: std::collections::HashMap<&str, String> =
        rows.iter().map(|r| (r.id.as_str(), format!("{} {}", r.kind, r.name.as_deref().unwrap_or(&r.id)))).collect();
    for o in &objects {
        titles.insert(&o.id, format!("{} {}", o.kind, o.name.as_deref().unwrap_or(&o.id)));
    }
    for o in &objects {
        let own = &titles[o.id.as_str()];
        let label = match o.parent.as_deref().and_then(|p| titles.get(p)) {
            Some(parent) => format!("{own} of {parent}"),
            None => own.clone(),
        };
        if !KINDS.contains(&o.kind.as_str()) {
            out.refused.push(format!("{label}: objects of this kind are not imported"));
            continue;
        }
        let Some(doc) = canonical(&o.kind, &o.doc) else {
            out.refused.push(format!("{label}: the document is not a valid {}", o.kind));
            continue;
        };
        if let Some(name) = &o.name {
            let key = (o.kind.clone(), o.parent.clone(), name.clone());
            if names.get(&key).is_some_and(|id| *id != o.id) {
                out.refused.push(format!("{label}: another object already has this name"));
                continue;
            }
            names.insert(key, o.id.clone());
        }
        match tx.get(&o.id).await? {
            Some(existing) if existing.kind != o.kind => {
                out.refused.push(format!("{label}: the id belongs to an object of another kind"));
            }
            Some(existing)
                if existing.name == o.name
                    && canonical(&existing.kind, &existing.doc).as_ref().unwrap_or(&existing.doc) == &doc =>
            {
                out.unchanged += 1
            }
            Some(_) => {
                // A dry run reports; only a real import writes.
                if !dry_run {
                    tx.update(&o.id, None, o.name.as_deref(), &doc, entry.clone()).await?;
                }
                out.updated.push(label);
            }
            None => {
                if !dry_run {
                    tx.create(&o.kind, &o.id, o.parent.as_deref(), o.name.as_deref(), &doc, entry.clone()).await?;
                }
                out.created.push(label);
            }
        }
    }
    // All or nothing: a document that is refused in part would leave references dangling.
    if dry_run || !out.refused.is_empty() {
        drop(tx);
        return Ok(Json(out));
    }
    st.commit(tx).await?;
    out.applied = true;
    Ok(Json(out))
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let doc = api.schema::<ConfigExport>();
    let result = api.schema::<ImportResult>();
    api.add(
        api.op(
            "get",
            "/system/config-export",
            "exportConfiguration",
            "system",
            "The configuration as one document, without secrets and without people",
        )
        .ok(doc.clone()),
    );
    api.add(
        api.op(
            "post",
            "/system/config-import",
            "importConfiguration",
            "system",
            "Create and replace configuration objects from an export",
        )
        .query("dry_run", "Set to true to see what would change without changing anything.")
        .body(doc)
        .ok(result),
    );
    router.route("/system/config-export", get(export)).route("/system/config-import", post(import))
}
