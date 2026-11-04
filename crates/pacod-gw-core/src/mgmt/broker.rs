//! pacod-broker from the core (runbook section 20): the broker as one more source of model
//! discovery's proposals, and its catalogue for the console and the data plane's `/mcp`.
//!
//! The broker is never asked on a call's path. Discovery's run asks it, as it asks the
//! providers, and writes proposals; nothing in the repository changes until an admin accepts.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};

use super::AdminState;
use crate::model::{Model, Status, new_id};

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
