//! pacod-broker from the core (runbook section 20): the broker as one more source of model
//! discovery's proposals, and its catalogue for the console and the data plane's `/mcp`.
//!
//! The broker is never asked on a call's path. Discovery's run asks it, as it asks the
//! providers, and writes proposals; nothing in the repository changes until an admin accepts.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::AdminState;
use crate::model::{Doc, Model, Status, new_id};

/// The broker's answer to a GET, as JSON.
pub async fn get(st: &AdminState, path: &str, query: &[(&str, String)]) -> anyhow::Result<Value> {
    let link = st.boot.broker.as_ref().ok_or_else(|| anyhow::anyhow!("no broker is configured (PGW_BROKER_URL)"))?;
    let mut r = st.http.get(format!("{}{path}", link.url)).query(query).timeout(Duration::from_secs(20));
    if let Some(t) = &link.token {
        r = r.bearer_auth(t);
    }
    let resp = r.send().await?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    anyhow::ensure!(
        status.is_success(),
        "the broker answered {status}: {}",
        body["detail"].as_str().unwrap_or_default()
    );
    Ok(body)
}

/// Every resource of the broker of the kinds the repository holds.
async fn models_on_broker(st: &AdminState) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    for kind in ["model", "embedding"] {
        let mut cursor: Option<String> = None;
        loop {
            let mut q = vec![("kind", kind.to_string()), ("limit", "200".to_string())];
            if let Some(c) = &cursor {
                q.push(("cursor", c.clone()));
            }
            let page = get(st, "/v1/resources", &q).await?;
            out.extend(page["hits"].as_array().cloned().unwrap_or_default());
            match page["next_cursor"].as_str() {
                Some(c) if !c.is_empty() && out.len() < 10_000 => cursor = Some(c.to_owned()),
                _ => break,
            }
        }
    }
    Ok(out)
}

/// The host and port an address names, for matching a broker interface to a provider.
fn origin(url: &str) -> Option<(String, u16)> {
    let u = url::Url::parse(url).ok()?;
    Some((u.host_str()?.to_ascii_lowercase(), u.port_or_known_default()?))
}

/// The protocols a repository provider serves; others (MCP, A2A) are not models it can call.
const MODEL_PROTOCOLS: [&str; 4] = ["openai-chat", "openai-responses", "anthropic-messages", "gemini"];

/// One hit as (provider id, upstream id, broker resource id, details), when an active provider
/// of the repository serves it. Gateway offerings (tag `gateway`) are left out: proposing the
/// gateway's own templates back to it would loop.
fn matched(hit: &Value, providers: &HashMap<(String, u16), String>) -> Option<(String, String, String, Value)> {
    let r = &hit["resource"];
    let tags = r["tags"].as_array().cloned().unwrap_or_default();
    if tags.iter().any(|t| t == "gateway") {
        return None;
    }
    let provider_id = r["interfaces"].as_array()?.iter().find_map(|f| {
        let protocol = f["protocol"].as_str()?;
        MODEL_PROTOCOLS.contains(&protocol).then_some(())?;
        providers.get(&origin(f["url"].as_str()?)?).cloned()
    })?;
    let upstream = r["model"]["upstream_id"].as_str().filter(|s| !s.is_empty()).or_else(|| r["name"].as_str())?;
    let details = json!({
        "display_name": r["title"].as_str().or_else(|| r["summary"].as_str()).unwrap_or_default(),
        "context_window": r["model"]["context_window"],
        "max_output_tokens": r["model"]["max_output_tokens"],
        "broker_resource_id": hit["id"],
        "broker": {
            "provider": hit["provider"]["name"],
            "price": hit["price"],
            "reputation": hit["reputation"]["score"],
        },
    });
    Some((provider_id, upstream.to_owned(), hit["id"].as_str()?.to_owned(), details))
}

/// Asks the broker and writes its proposals: `new` for a model the broker lists at an active
/// provider that the repository lacks, `update` for a repository model that is a broker
/// resource but not linked to it yet. Returns what it found, for the run's result.
pub async fn propose(st: &AdminState) -> anyhow::Result<Value> {
    let started = st.store.discovery_clock().await?;
    let snap = st.config.snapshot();
    let providers: HashMap<(String, u16), String> = snap
        .providers
        .values()
        .filter(|p| p.doc.status == Status::Active)
        .filter_map(|p| Some((origin(&p.doc.base_url)?, p.id.clone())))
        .collect();
    let hits = models_on_broker(st).await?;
    let (mut new, mut link) = (0, 0);
    for hit in &hits {
        let Some((provider_id, upstream, rid, details)) = matched(hit, &providers) else { continue };
        let existing: Option<(&String, &Model)> = snap.models.iter().map(|(id, m)| (id, &m.doc)).find(|(_, m)| {
            m.provider_id == provider_id
                && m.upstream_id == upstream
                && !matches!(m.state, crate::model::ModelState::Retired)
        });
        match existing {
            None => {
                st.store.broker_proposal_seen(&new_id("dsp"), &provider_id, &upstream, "new", None, &details).await?;
                new += 1;
            }
            Some((id, m)) if m.broker_resource_id.as_deref() != Some(rid.as_str()) => {
                let current = json!(m.broker_resource_id);
                let change = json!({ "current": current, "reported": rid, "source": "broker" });
                let d = json!({
                    "model": m.name,
                    "changes": { "broker_resource_id": change },
                    "reported": { "broker_resource_id": rid },
                    "broker_resource_id": rid,
                    "broker": details["broker"],
                });
                st.store.broker_proposal_seen(&new_id("dsp"), &provider_id, &upstream, "update", Some(id), &d).await?;
                link += 1;
            }
            Some(_) => {}
        }
    }
    let resolved = st.store.broker_proposals_resolve_unseen(started).await?;
    Ok(json!({ "resources": hits.len(), "new": new, "links": link, "resolved": resolved }))
}

/// The broker's answer to a POST, as JSON.
pub async fn post(st: &AdminState, path: &str, body: &Value) -> anyhow::Result<Value> {
    let link = st.boot.broker.as_ref().ok_or_else(|| anyhow::anyhow!("no broker is configured (PGW_BROKER_URL)"))?;
    let mut r = st.http.post(format!("{}{path}", link.url)).json(body).timeout(Duration::from_secs(20));
    if let Some(t) = &link.token {
        r = r.bearer_auth(t);
    }
    let resp = r.send().await?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    anyhow::ensure!(status.is_success(), "the broker answered {status}: {}", v["detail"].as_str().unwrap_or_default());
    Ok(v)
}

// --- pools from the broker (R4) and leased upstreams (R5) ---------------------------------------

static POOLS_LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Whether templates' pools are due to be resolved again: every `PGW_BROKER_POOL_SECONDS`.
pub fn pools_due(st: &AdminState) -> bool {
    let Some(link) = &st.boot.broker else { return false };
    let mut last = POOLS_LAST.lock().unwrap_or_else(|e| e.into_inner());
    let due = last.is_none_or(|t| t.elapsed() >= Duration::from_secs(link.pool_seconds));
    if due {
        *last = Some(std::time::Instant::now());
    }
    due
}

fn system_audit(action: &str, detail: Value) -> crate::store::AuditEntry {
    crate::store::AuditEntry {
        actor: "broker".into(),
        actor_kind: "system".into(),
        action: action.into(),
        detail,
        ..Default::default()
    }
}

/// Resolves every template's `pool_from` against the broker and writes what changed.
pub async fn resolve_pools(st: &AdminState) -> anyhow::Result<usize> {
    let snap = st.config.snapshot();
    let mut changed = 0;
    let mut templates: Vec<(Arc<crate::snapshot::WorkspaceRt>, Arc<crate::snapshot::TemplateRt>)> = Vec::new();
    for ws in snap.workspaces.values() {
        for t in ws.templates.values() {
            if t.spec.pool_from.is_some() {
                templates.push((ws.clone(), t.clone()));
            }
        }
    }
    for (ws, t) in templates {
        match resolve_pool(st, &ws, &t).await {
            Ok(true) => changed += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(template = %t.name, error = %format!("{e:#}"), "a pool from the broker could not be resolved")
            }
        }
    }
    Ok(changed)
}

/// One template's pool from the broker. True when its record changed.
async fn resolve_pool(
    st: &AdminState,
    ws: &crate::snapshot::WorkspaceRt,
    t: &crate::snapshot::TemplateRt,
) -> anyhow::Result<bool> {
    let Some(from) = &t.spec.pool_from else { return Ok(false) };
    let mut query = from.query.clone();
    query["limit"] = json!((usize::from(from.max) * 4).min(100));
    let answer = post(st, "/v1/discover", &query).await?;
    let hits = answer["hits"].as_array().cloned().unwrap_or_default();
    let mut problems = Vec::new();
    if from.own_leases {
        lease_upstreams(st, ws, t, &hits, &mut problems).await?;
    }
    // The leased models just made are in the next snapshot.
    let snap = st.config.snapshot();
    let own: Vec<String> = t.spec.pool().iter().map(|e| e.model.clone()).collect();
    let mut entries = Vec::new();
    let mut resources = std::collections::BTreeMap::new();
    let mut unknown = 0;
    for hit in &hits {
        if entries.len() >= usize::from(from.max) {
            break;
        }
        let Some(rid) = hit["id"].as_str() else { continue };
        let model = snap.models.values().find(|m| {
            m.doc.broker_resource_id.as_deref() == Some(rid)
                && m.doc.state == crate::model::ModelState::Active
                && snap.grant_terms(ws, &m.id).is_some()
        });
        let Some(m) = model else {
            unknown += 1;
            continue;
        };
        if own.contains(&m.doc.name)
            || own.contains(&m.id)
            || entries.iter().any(|e: &crate::model::PoolEntry| e.model == m.doc.name)
        {
            continue;
        }
        entries.push(crate::model::PoolEntry::of(&m.doc.name));
        resources.insert(m.doc.name.clone(), rid.to_owned());
    }
    if unknown > 0 {
        problems.push(format!(
            "{unknown} broker resource(s) matched that are not active models of the repository granted to this workspace"
        ));
    }
    let old = snap.broker_pools.get(&t.id);
    let mut doc = old
        .map(|o| o.doc.clone())
        .unwrap_or_else(|| crate::model::BrokerPool { template_id: t.id.clone(), ..Default::default() });
    let before = doc.clone();
    match from.mode {
        crate::model::PoolMode::Follow => {
            doc.current = entries;
            doc.proposed = None;
        }
        crate::model::PoolMode::Propose => {
            doc.proposed = (entries != doc.current).then_some(entries);
        }
    }
    doc.resources = resources;
    doc.problems = problems;
    if doc == before && old.is_some() {
        return Ok(false);
    }
    doc.resolved_at = Some(chrono::Utc::now());
    let value = serde_json::to_value(&doc)?;
    let mut audit = system_audit("resolve_broker_pool", json!({ "template": t.name, "mode": from.mode }));
    audit.organisation_id = Some(ws.organisation_id.clone());
    audit.workspace_id = Some(ws.id.clone());
    let mut tx = st.store.begin().await?;
    match old {
        Some(o) => {
            tx.update(&o.id, Some(o.version), None, &value, audit).await?;
        }
        None => {
            tx.create(
                crate::model::BrokerPool::KIND,
                &new_id(crate::model::BrokerPool::PREFIX),
                Some(&ws.id),
                None,
                &value,
                audit,
            )
            .await?;
        }
    }
    st.commit(tx).await.map_err(|e| anyhow::anyhow!(e.message))?;
    if doc.proposed.is_some() {
        st.announce(
            "job",
            json!({ "job": "broker_pools", "status": "proposed", "template": t.name, "workspace": ws.id }),
        );
    }
    Ok(true)
}

/// The resource's OpenAI-compatible address, the way a self-hosted server announces itself.
fn openai_url(r: &Value) -> Option<String> {
    r["interfaces"].as_array()?.iter().find(|f| f["protocol"] == "openai-chat")?["url"].as_str().map(str::to_owned)
}

/// The repository's name of a leased endpoint: stable, from its broker resource id.
fn leased_name(rid: &str) -> String {
    let short: String = rid.trim_start_matches("res_").chars().take(12).collect();
    format!("leased-{short}")
}

/// R5: the organisation's own self-hosted endpoints that announce themselves to the broker.
/// Each becomes a provider (OpenAI-compatible, no credential) and a free, active model linked to
/// its resource, granted to the organisation; one whose lease ended has its provider disabled.
/// Only resources whose provider the broker says is this organisation's take part.
async fn lease_upstreams(
    st: &AdminState,
    ws: &crate::snapshot::WorkspaceRt,
    t: &crate::snapshot::TemplateRt,
    hits: &[Value],
    problems: &mut Vec<String>,
) -> anyhow::Result<()> {
    let snap = st.config.snapshot();
    let open_broker = st.boot.broker.as_ref().is_some_and(|b| b.token.is_none());
    let mut live: Vec<String> = Vec::new();
    let mut tx = st.store.begin().await?;
    let mut wrote = false;
    for hit in hits {
        let (Some(rid), r) = (hit["id"].as_str(), &hit["resource"]) else { continue };
        let provider_id = hit["provider"]["id"].as_str().unwrap_or_default();
        let owner = get(st, &format!("/v1/providers/{provider_id}"), &[])
            .await
            .ok()
            .and_then(|p| p["organisation_id"].as_str().map(str::to_owned));
        // A broker in open mode knows no organisations: there every provider counts as one's own.
        let own = match owner {
            Some(o) => o == ws.organisation_id,
            None => open_broker,
        };
        if !own {
            continue;
        }
        let Some(url) = openai_url(r) else { continue };
        if let Err(e) = st.egress.check_url(&url) {
            problems.push(format!("{rid}: {url} is not allowed ({e}); add its host to PGW_EGRESS_ALLOW_PRIVATE"));
            continue;
        }
        let name = leased_name(rid);
        live.push(name.clone());
        let upstream = r["model"]["upstream_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| r["name"].as_str())
            .unwrap_or("model");
        let provider = snap.providers.values().find(|p| p.doc.name == name);
        let pid = match provider {
            Some(p) if p.doc.status == Status::Active && p.doc.base_url == url => p.id.clone(),
            Some(p) => {
                let mut doc = p.doc.clone();
                doc.status = Status::Active;
                doc.base_url = url.clone();
                let obj = st.store.get_obj::<crate::model::Provider>(&p.id).await?;
                tx.update(
                    &p.id,
                    Some(obj.version),
                    Some(&name),
                    &serde_json::to_value(&doc)?,
                    system_audit("lease_provider", json!({ "resource": rid })),
                )
                .await?;
                wrote = true;
                p.id.clone()
            }
            None => {
                let doc: crate::model::Provider = serde_json::from_value(json!({
                    "name": name, "display_name": format!("Leased endpoint {rid}"), "family": "ollama", "base_url": url,
                    "managed_api": "openai_compatible", "status": "active",
                }))?;
                let id = new_id(crate::model::Provider::PREFIX);
                tx.create(
                    crate::model::Provider::KIND,
                    &id,
                    None,
                    Some(&name),
                    &serde_json::to_value(&doc)?,
                    system_audit("lease_provider", json!({ "resource": rid })),
                )
                .await?;
                wrote = true;
                id
            }
        };
        let model_name = format!("{name}/{upstream}");
        if !snap.models.values().any(|m| m.doc.name == model_name) {
            let model: Model = serde_json::from_value(json!({
                "name": model_name, "provider_id": pid, "upstream_id": upstream, "free": true, "state": "active",
                "broker_resource_id": rid, "context_window": r["model"]["context_window"],
                "notes": "a leased endpoint of pacod-broker (pool_from.own_leases)",
            }))?;
            let mid = new_id(Model::PREFIX);
            tx.create(
                Model::KIND,
                &mid,
                None,
                Some(&model.name),
                &serde_json::to_value(&model)?,
                system_audit("lease_model", json!({ "resource": rid })),
            )
            .await?;
            let grant: crate::model::OrgGrant = serde_json::from_value(json!({ "model_id": mid }))?;
            let gid = new_id(crate::model::OrgGrant::PREFIX);
            tx.create(
                crate::model::OrgGrant::KIND,
                &gid,
                Some(&ws.organisation_id),
                None,
                &serde_json::to_value(&grant)?,
                system_audit("lease_grant", json!({ "resource": rid })),
            )
            .await?;
            wrote = true;
        }
    }
    // Leases that ended: their providers stop taking calls, and drop out of the pool. Only this
    // template's own members: another template's query may still find its endpoints.
    let members: Vec<&str> = snap
        .broker_pools
        .get(&t.id)
        .map(|r| r.doc.current.iter().map(|e| e.model.as_str()).collect())
        .unwrap_or_default();
    for p in snap.providers.values().filter(|p| p.doc.name.starts_with("leased-") && p.doc.status == Status::Active) {
        let in_this_pool =
            snap.models.values().any(|m| m.doc.provider_id == p.id && members.contains(&m.doc.name.as_str()));
        if in_this_pool && !live.contains(&p.doc.name) {
            let mut doc = p.doc.clone();
            doc.status = Status::Disabled;
            let obj = st.store.get_obj::<crate::model::Provider>(&p.id).await?;
            tx.update(
                &p.id,
                Some(obj.version),
                Some(&p.doc.name),
                &serde_json::to_value(&doc)?,
                system_audit("lease_ended", json!({ "provider": p.doc.name })),
            )
            .await?;
            wrote = true;
        }
    }
    if wrote {
        st.commit(tx).await.map_err(|e| anyhow::anyhow!(e.message))?;
    }
    Ok(())
}

// --- the API -------------------------------------------------------------------------------------

use axum::extract::{Path, State};
use axum::routing::{get as get_route, post as post_route};
use axum::{Json, Router};

use super::auth::Principal;
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;

/// Whether the core has a broker, and whether it answers.
async fn status(State(st): State<AdminState>, _p: Principal) -> Json<Value> {
    let Some(link) = &st.boot.broker else {
        return Json(json!({ "configured": false }));
    };
    let reach = get(&st, "/v1/resources", &[("limit", "1".to_string())]).await;
    let snap = st.config.snapshot();
    let linked = snap.models.values().filter(|m| m.doc.broker_resource_id.is_some()).count();
    let pools = snap.broker_pools.len();
    Json(json!({
        "configured": true,
        "url": link.url,
        "reachable": reach.is_ok(),
        "error": reach.as_ref().err().map(|e| format!("{e:#}")),
        "resources": reach.as_ref().ok().and_then(|v| v["total"].as_u64()),
        "linked_models": linked,
        "pools": pools,
    }))
}

/// One broker resource as the broker shows it to the core: prices, live state, reputation. The
/// console shows it on a linked model's page; a person's token is for the gateway, not the
/// broker, so the core asks with its own.
async fn resource(State(st): State<AdminState>, _p: Principal, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if !(id.starts_with("res_") && id.len() <= 64 && id[4..].bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(ApiError::invalid("a broker resource id: res_ and hex digits"));
    }
    if st.boot.broker.is_none() {
        return Err(ApiError::not_found("broker (PGW_BROKER_URL is not set)"));
    }
    get(&st, &format!("/v1/resources/{id}"), &[])
        .await
        .map(|mut v| {
            // The core's own ticket is no business of the console's.
            if let Some(o) = v.as_object_mut() {
                o.remove("ticket");
            }
            Json(v)
        })
        .map_err(|e| ApiError::unavailable(format!("the broker could not answer: {e}")))
}

fn template_of<'a>(
    snap: &'a crate::snapshot::Snapshot,
    org: &str,
    ws: &str,
    id: &str,
) -> ApiResult<(&'a Arc<crate::snapshot::WorkspaceRt>, &'a Arc<crate::snapshot::TemplateRt>)> {
    let w =
        snap.workspaces.get(ws).filter(|w| w.organisation_id == org).ok_or_else(|| ApiError::not_found("workspace"))?;
    let t = w.templates.values().find(|t| t.id == id).ok_or_else(|| ApiError::not_found("template"))?;
    Ok((w, t))
}

/// What a template's pool from the broker resolved to.
async fn pool_view(
    State(st): State<AdminState>,
    p: Principal,
    Path((org, ws, id)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    if !p.can_read_workspace(&ws, &org) {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let (_, t) = template_of(&snap, &org, &ws, &id)?;
    let row = snap.broker_pools.get(&t.id);
    Ok(Json(json!({
        "pool_from": t.spec.pool_from,
        "pool": row.map(|r| &r.doc),
        "in_force": t.pool.iter().map(|m| m.model.doc.name.clone()).collect::<Vec<_>>(),
    })))
}

/// Takes the broker's proposed members into the pool (mode `propose`).
async fn pool_accept(
    State(st): State<AdminState>,
    p: Principal,
    Path((org, ws, id)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    if !p.can_admin_workspace(&ws, &org) {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let (_, t) = template_of(&snap, &org, &ws, &id)?;
    let row = snap
        .broker_pools
        .get(&t.id)
        .ok_or_else(|| ApiError::conflict("the broker has proposed nothing for this template"))?;
    let Some(proposed) = row.doc.proposed.clone() else {
        return Err(ApiError::conflict("the broker has proposed nothing for this template"));
    };
    let mut doc = row.doc.clone();
    doc.current = proposed;
    doc.proposed = None;
    let mut audit = p.audit(Some(&org), Some(&ws));
    audit.action = "accept_broker_pool".into();
    audit.detail = json!({ "template": t.name, "members": doc.current });
    let mut tx = st.store.begin().await?;
    tx.update(&row.id, Some(row.version), None, &serde_json::to_value(&doc).map_err(ApiError::internal)?, audit)
        .await?;
    st.commit(tx).await?;
    Ok(Json(serde_json::to_value(&doc).map_err(ApiError::internal)?))
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let free = api.schema::<Value>();
    api.add(
        api.op(
            "get",
            "/broker/status",
            "brokerStatus",
            "broker",
            "Whether the core has pacod-broker, and whether it answers",
        )
        .ok(free.clone()),
    );
    api.add(
        api.op(
            "get",
            "/broker/resources/{id}",
            "brokerResource",
            "broker",
            "One pacod-broker resource as the broker shows it",
        )
        .ok(free.clone()),
    );
    api.add(
        api.op(
            "get",
            "/orgs/{org}/workspaces/{ws}/templates/{id}/broker-pool",
            "templateBrokerPool",
            "templates",
            "What a template's pool from the broker resolved to",
        )
        .ok(free.clone()),
    );
    api.add(
        api.op(
            "post",
            "/orgs/{org}/workspaces/{ws}/templates/{id}/broker-pool/accept",
            "acceptTemplateBrokerPool",
            "templates",
            "Take the broker's proposed members into the pool",
        )
        .ok(free),
    );
    router
        .route("/broker/status", get_route(status))
        .route("/broker/resources/{id}", get_route(resource))
        .route("/orgs/{org}/workspaces/{ws}/templates/{id}/broker-pool", get_route(pool_view))
        .route("/orgs/{org}/workspaces/{ws}/templates/{id}/broker-pool/accept", post_route(pool_accept))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hit_matches_the_provider_that_serves_its_address() {
        let providers = HashMap::from([(("api.openai.com".to_string(), 443), "prv_openai".to_string())]);
        let hit = json!({
            "id": "res_1",
            "resource": { "name": "gpt-4o", "tags": [], "model": { "upstream_id": "gpt-4o", "context_window": 128000 },
                          "interfaces": [{ "protocol": "openai-chat", "url": "https://api.openai.com/v1" }] },
            "provider": { "name": "openai" }, "price": null, "reputation": null
        });
        let (p, u, r, d) = matched(&hit, &providers).unwrap();
        assert_eq!((p.as_str(), u.as_str(), r.as_str()), ("prv_openai", "gpt-4o", "res_1"));
        assert_eq!(d["context_window"], 128000);
        assert_eq!(d["broker_resource_id"], "res_1");

        let mut elsewhere = hit.clone();
        elsewhere["resource"]["interfaces"][0]["url"] = json!("https://other.example/v1");
        assert!(matched(&elsewhere, &providers).is_none(), "no provider of the repository serves it");
        let mut offering = hit.clone();
        offering["resource"]["tags"] = json!(["gateway"]);
        assert!(matched(&offering, &providers).is_none(), "the gateway's own offerings are not proposed back");
        let mut mcp = hit;
        mcp["resource"]["interfaces"][0]["protocol"] = json!("mcp-streamable-http");
        assert!(matched(&mcp, &providers).is_none(), "an MCP server is not a model");
    }
}
