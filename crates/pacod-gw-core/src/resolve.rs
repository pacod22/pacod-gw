//! Decides which model serves a request (concept sections 6.3, 6.5 and 9.4).
//!
//! The `model` of a request is, in this order: a repository model id; `@template`; or the
//! provider's own model id when exactly one model the workspace may use has it. Grants, the
//! key's allow-lists and the model's life-cycle state are applied here, and nowhere else.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::dataplane::errors::GwError;
use crate::model::{Family, ModelState, Status};
use crate::snapshot::{GrantTerms, KeyRt, ModelRt, ProviderRt, Snapshot, TemplateRt, WorkspaceRt};

/// How long the successor chain of a retired model may be.
const MAX_SUCCESSOR_HOPS: usize = 5;

/// The outcome of resolving a request's `model`.
#[derive(Debug, Clone)]
pub struct Target {
    /// The model that serves the request.
    pub model: Arc<ModelRt>,
    pub provider: Arc<ProviderRt>,
    /// The model the request named, when a retired model was replaced by its successor.
    pub replaced: Option<Arc<ModelRt>>,
    pub template: Option<Arc<TemplateRt>>,
    pub terms: GrantTerms,
    /// Set when the model is deprecated: its sunset date, if one is set.
    pub deprecated: Option<Option<DateTime<Utc>>>,
}

impl Target {
    /// The value of the `x-gw-model-deprecated` header.
    pub fn deprecation_header(&self, snap: &Snapshot) -> Option<String> {
        let sunset = self.deprecated.as_ref()?;
        let mut out = match sunset {
            Some(t) => t.format("%Y-%m-%d").to_string(),
            None => "true".to_owned(),
        };
        if let Some(s) = self.model.doc.successor_id.as_ref().and_then(|id| snap.models.get(id)) {
            out.push_str(&format!("; successor={}", s.doc.name));
        }
        Some(out)
    }
}

/// What restricts the resolution.
pub struct ResolveRequest<'a> {
    /// The request's `model` value.
    pub model: &'a str,
    /// A template named in the `x-gw-template` header or the `gateway` object.
    pub template: Option<&'a str>,
    /// Proxy mode: the provider in the URL. The model must be at this provider.
    pub provider: Option<&'a ProviderRt>,
    /// Managed mode on a dialect interface: the families the interface can serve.
    pub families: Option<&'a [Family]>,
}

fn not_found(model: &str) -> GwError {
    GwError::not_found(
        "model_not_found",
        format!("the model \"{model}\" does not exist or is not available to this workspace"),
    )
}

/// Resolves a template by name for a key.
pub fn template_for(ws: &WorkspaceRt, key: &KeyRt, name: &str) -> Result<Arc<TemplateRt>, GwError> {
    let name = name.trim_start_matches('@');
    let t = ws.templates.get(name).ok_or_else(|| {
        GwError::not_found("template_not_found", format!("the template \"@{name}\" does not exist in this workspace"))
    })?;
    if !key.doc.allowed_templates.is_empty() && !key.doc.allowed_templates.iter().any(|a| a == name) {
        return Err(GwError::not_allowed(format!("this key may not use the template \"@{name}\"")));
    }
    Ok(t.clone())
}

/// The model that serves, the model it replaced (if any), and for a deprecated model its
/// sunset date.
type Served = (Arc<ModelRt>, Option<Arc<ModelRt>>, Option<Option<DateTime<Utc>>>);

/// Applies the life cycle to a model the workspace may use.
fn apply_life_cycle(
    snap: &Snapshot,
    model: Arc<ModelRt>,
    terms: GrantTerms,
    same_provider_only: bool,
) -> Result<Served, GwError> {
    let mut current = model.clone();
    let mut hops = 0;
    loop {
        match current.doc.state {
            ModelState::Active => break,
            ModelState::Deprecated => break,
            // A draft is not visible to applications.
            ModelState::Draft => return Err(not_found(&model.doc.name)),
            ModelState::Disabled => {
                return Err(GwError::unavailable(
                    "model_disabled",
                    format!("the model \"{}\" is switched off at the moment", current.doc.name),
                ));
            }
            ModelState::Retired => {
                let retired = || GwError::model_retired(format!("the model \"{}\" has been retired", model.doc.name));
                if !terms.follow_successor || hops >= MAX_SUCCESSOR_HOPS {
                    return Err(retired());
                }
                let next = current.doc.successor_id.as_ref().and_then(|id| snap.models.get(id)).ok_or_else(retired)?;
                if same_provider_only && next.doc.provider_id != model.doc.provider_id {
                    return Err(GwError::model_retired(format!(
                        "the model \"{}\" has been retired and its successor is at another provider, which proxy mode cannot follow",
                        model.doc.name
                    )));
                }
                current = next.clone();
                hops += 1;
            }
        }
    }
    let deprecated = (current.doc.state == ModelState::Deprecated).then_some(current.doc.sunset_at);
    let replaced = (current.id != model.id).then_some(model);
    Ok((current, replaced, deprecated))
}

fn key_allows_model(key: &KeyRt, model: &ModelRt) -> bool {
    key.doc.allowed_models.is_empty() || key.doc.allowed_models.iter().any(|m| *m == model.doc.name || *m == model.id)
}

/// Resolves the target of a request.
pub fn resolve(snap: &Snapshot, ws: &WorkspaceRt, key: &KeyRt, req: &ResolveRequest<'_>) -> Result<Target, GwError> {
    // 1. A template, named in the model field or separately.
    let template_name = req.template.or_else(|| req.model.strip_prefix('@'));
    let template = match template_name {
        Some(name) => Some(template_for(ws, key, name)?),
        None => None,
    };

    // 2. The model the request (or its template) names.
    let named: Arc<ModelRt> = match &template {
        Some(t) => snap.models.get(&t.model_id).cloned().ok_or_else(|| not_found(&t.spec.model))?,
        None => match snap.model_by_name(req.model) {
            Some(m) => m.clone(),
            // 3. The provider's own id, if exactly one usable model has it.
            None => {
                let mut candidates = snap.models.values().filter(|m| {
                    m.doc.upstream_id == req.model
                        && m.doc.state != ModelState::Draft
                        && snap.grant_terms(ws, &m.id).is_some()
                        && req.provider.is_none_or(|p| p.id == m.doc.provider_id)
                        && req.families.is_none_or(|f| {
                            snap.providers.get(&m.doc.provider_id).is_some_and(|p| f.contains(&p.doc.family))
                        })
                });
                let first = candidates.next().ok_or_else(|| not_found(req.model))?;
                if candidates.next().is_some() {
                    return Err(GwError::invalid_request(format!(
                        "\"{}\" matches more than one model of this workspace; use the model's full id",
                        req.model
                    )));
                }
                first.clone()
            }
        },
    };

    // Grants: the workspace must be allowed to use the model the request names.
    let terms = snap.grant_terms(ws, &named.id).ok_or_else(|| not_found(&named.doc.name))?;
    if !key_allows_model(key, &named) {
        return Err(GwError::not_allowed(format!("this key may not use the model \"{}\"", named.doc.name)));
    }

    let (model, replaced, deprecated) = apply_life_cycle(snap, named, terms, req.provider.is_some())?;

    let provider =
        snap.providers.get(&model.doc.provider_id).filter(|p| p.doc.status == Status::Active).cloned().ok_or_else(
            || GwError::unavailable("provider_unavailable", "the model's provider is disabled or missing"),
        )?;

    if let Some(p) = req.provider
        && p.id != provider.id
    {
        let message = format!("the model \"{}\" is not at the provider \"{}\"", model.doc.name, p.doc.name);
        return Err(match &template {
            Some(_) => GwError::template_provider_mismatch(message),
            None => GwError::invalid_request(message),
        });
    }
    if let Some(f) = req.families
        && !f.contains(&provider.doc.family)
    {
        return Err(GwError::unsupported_feature(format!(
            "the model \"{}\" cannot be reached through this interface",
            model.doc.name
        )));
    }

    Ok(Target { model, provider, replaced, template, terms, deprecated })
}

/// Proxy mode without a template: the request names the provider's own model id. It must
/// belong to a repository model the workspace may use. A retired model is not replaced here:
/// proxy mode follows a successor only through a template (concept section 4.1).
pub fn resolve_upstream(
    snap: &Snapshot,
    ws: &WorkspaceRt,
    key: &KeyRt,
    provider: &Arc<ProviderRt>,
    upstream_id: &str,
) -> Result<Target, GwError> {
    let mut candidates: Vec<(&Arc<ModelRt>, GrantTerms)> = snap
        .models
        .values()
        .filter(|m| {
            m.doc.provider_id == provider.id && m.doc.upstream_id == upstream_id && m.doc.state != ModelState::Draft
        })
        .filter_map(|m| snap.grant_terms(ws, &m.id).map(|t| (m, t)))
        .collect();
    // Two repository models may share a provider id; a callable one is preferred, and the
    // choice is stable.
    candidates
        .sort_by(|(a, _), (b, _)| (!a.doc.state.callable(), &a.doc.name).cmp(&(!b.doc.state.callable(), &b.doc.name)));
    let (model, terms) = candidates.into_iter().next().ok_or_else(|| not_found(upstream_id))?;
    if !key_allows_model(key, model) {
        return Err(GwError::not_allowed(format!("this key may not use the model \"{}\"", model.doc.name)));
    }
    match model.doc.state {
        ModelState::Active | ModelState::Deprecated => {}
        ModelState::Disabled => {
            return Err(GwError::unavailable(
                "model_disabled",
                format!("the model \"{}\" is switched off at the moment", model.doc.name),
            ));
        }
        ModelState::Retired => {
            return Err(GwError::model_retired(format!(
                "the model \"{}\" has been retired; proxy mode follows a successor only through a template",
                model.doc.name
            )));
        }
        ModelState::Draft => return Err(not_found(upstream_id)),
    }
    let deprecated = (model.doc.state == ModelState::Deprecated).then_some(model.doc.sunset_at);
    Ok(Target { model: model.clone(), provider: provider.clone(), replaced: None, template: None, terms, deprecated })
}

/// The models a key can call: granted to its workspace, callable, and allowed by the key.
pub fn callable_models<'a>(snap: &'a Snapshot, ws: &'a WorkspaceRt, key: &'a KeyRt) -> Vec<&'a Arc<ModelRt>> {
    let mut v: Vec<&Arc<ModelRt>> = snap
        .models
        .values()
        .filter(|m| m.doc.state.callable() && snap.grant_terms(ws, &m.id).is_some() && key_allows_model(key, m))
        .collect();
    v.sort_by(|a, b| a.doc.name.cmp(&b.doc.name));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::envelope::Kek;
    use crate::snapshot::build;
    use crate::store::Row;
    use serde_json::{Value, json};

    fn row(kind: &str, id: &str, parent: Option<&str>, doc: Value) -> Row {
        Row {
            id: id.into(),
            kind: kind.into(),
            parent: parent.map(Into::into),
            name: None,
            doc,
            version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn model(id: &str, name: &str, provider: &str, upstream: &str, extra: Value) -> Row {
        let mut doc = json!({ "name": name, "provider_id": provider, "upstream_id": upstream, "state": "active" });
        doc.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        row("model", id, None, doc)
    }

    fn template(id: &str, name: &str, model: &str) -> Row {
        row(
            "template",
            id,
            Some("ws_1"),
            json!({"name": name, "current_version": 1, "versions": [
                {"version": 1, "spec": {"model": model}, "created_at": "2026-10-01T00:00:00Z"}
            ]}),
        )
    }

    fn snapshot(extra: Vec<Row>, key_doc: Value) -> Snapshot {
        let mut key = json!({"name": "k", "secret_hmac": hex::encode([1u8; 32])});
        key.as_object_mut().unwrap().extend(key_doc.as_object().unwrap().clone());
        let mut rows = vec![
            row("organisation", "org_1", None, json!({"name": "acme"})),
            row("workspace", "ws_1", Some("org_1"), json!({"name": "prod"})),
            row("api_key", "key_1", Some("ws_1"), key),
            row(
                "provider",
                "prv_a",
                None,
                json!({"name": "anthropic", "family": "anthropic", "base_url": "https://a.example"}),
            ),
            row(
                "provider",
                "prv_o",
                None,
                json!({"name": "openai", "family": "openai", "base_url": "https://o.example/v1"}),
            ),
            model("mdl_sonnet", "anthropic/claude-sonnet", "prv_a", "claude-sonnet", json!({})),
            model("mdl_haiku", "anthropic/claude-haiku", "prv_a", "claude-haiku", json!({})),
            model("mdl_gpt", "openai/gpt-x", "prv_o", "gpt-x", json!({})),
            model("mdl_draft", "openai/draft", "prv_o", "draft", json!({"state": "draft"})),
            model("mdl_hidden", "openai/hidden", "prv_o", "hidden", json!({})),
        ];
        for m in ["mdl_sonnet", "mdl_haiku", "mdl_gpt", "mdl_draft"] {
            rows.push(row("org_grant", &format!("ogr_{m}"), Some("org_1"), json!({"model_id": m})));
        }
        rows.extend(extra);
        let snap = build(1, rows, &[Kek::new("k", [0u8; 32])]);
        assert!(snap.problems.is_empty(), "{:?}", snap.problems);
        snap
    }

    fn run(snap: &Snapshot, req: ResolveRequest<'_>) -> Result<Target, GwError> {
        let ws = &snap.workspaces["ws_1"];
        let key = snap.keys.values().next().unwrap();
        resolve(snap, ws, key, &req)
    }

    fn by_model(model: &str) -> ResolveRequest<'_> {
        ResolveRequest { model, template: None, provider: None, families: None }
    }

    #[test]
    fn resolves_by_id_by_template_and_by_the_providers_own_id() {
        let snap = snapshot(vec![template("tpl_1", "planner", "anthropic/claude-haiku")], json!({}));
        let t = run(&snap, by_model("anthropic/claude-sonnet")).unwrap();
        assert_eq!((t.model.id.as_str(), t.provider.doc.name.as_str()), ("mdl_sonnet", "anthropic"));
        assert!(t.template.is_none() && t.replaced.is_none() && t.deprecated.is_none());

        let t = run(&snap, by_model("@planner")).unwrap();
        assert_eq!(t.model.id, "mdl_haiku");
        assert_eq!(t.template.as_ref().unwrap().name, "planner");
        // The header names the template; the model field is then irrelevant.
        let t =
            run(&snap, ResolveRequest { model: "whatever", template: Some("planner"), provider: None, families: None })
                .unwrap();
        assert_eq!(t.model.id, "mdl_haiku");

        // The provider's own id works when it is unambiguous.
        assert_eq!(run(&snap, by_model("claude-sonnet")).unwrap().model.id, "mdl_sonnet");
    }

    #[test]
    fn unknown_ungranted_and_draft_models_look_the_same() {
        let snap = snapshot(vec![], json!({}));
        for m in ["no/such", "openai/hidden", "openai/draft", "hidden", "@missing"] {
            let e = run(&snap, by_model(m)).unwrap_err();
            assert_eq!(e.status.as_u16(), 404, "{m}");
        }
        assert_eq!(run(&snap, by_model("@missing")).unwrap_err().kind, "template_not_found");
        assert_eq!(run(&snap, by_model("openai/hidden")).unwrap_err().kind, "model_not_found");
    }

    #[test]
    fn an_ambiguous_provider_id_is_refused() {
        let snap = snapshot(
            vec![
                model("mdl_dup", "other/claude-sonnet", "prv_o", "claude-sonnet", json!({})),
                row("org_grant", "ogr_dup", Some("org_1"), json!({"model_id": "mdl_dup"})),
            ],
            json!({}),
        );
        let e = run(&snap, by_model("claude-sonnet")).unwrap_err();
        assert_eq!(e.kind, "invalid_request");
        // Proxy mode names the provider, which removes the ambiguity.
        let p = snap.provider_by_name("anthropic").unwrap().clone();
        let t =
            run(&snap, ResolveRequest { model: "claude-sonnet", template: None, provider: Some(&p), families: None })
                .unwrap();
        assert_eq!(t.model.id, "mdl_sonnet");
    }

    #[test]
    fn key_allow_lists_narrow_further() {
        let snap = snapshot(
            vec![
                template("tpl_1", "planner", "anthropic/claude-haiku"),
                template("tpl_2", "other", "anthropic/claude-haiku"),
            ],
            json!({"allowed_models": ["anthropic/claude-haiku"], "allowed_templates": ["planner"]}),
        );
        assert!(run(&snap, by_model("anthropic/claude-haiku")).is_ok());
        assert_eq!(run(&snap, by_model("anthropic/claude-sonnet")).unwrap_err().kind, "not_allowed");
        assert!(run(&snap, by_model("@planner")).is_ok());
        assert_eq!(run(&snap, by_model("@other")).unwrap_err().kind, "not_allowed");
        let key = snap.keys.values().next().unwrap();
        let names: Vec<&str> =
            callable_models(&snap, &snap.workspaces["ws_1"], key).iter().map(|m| m.doc.name.as_str()).collect();
        assert_eq!(names, ["anthropic/claude-haiku"]);
    }

    #[test]
    fn life_cycle_states() {
        let snap = snapshot(
            vec![
                model(
                    "mdl_dep",
                    "x/deprecated",
                    "prv_a",
                    "dep",
                    json!({"state": "deprecated", "sunset_at": "2027-01-31T00:00:00Z", "successor_id": "mdl_sonnet"}),
                ),
                model("mdl_off", "x/disabled", "prv_a", "off", json!({"state": "disabled"})),
                model("mdl_ret", "x/retired", "prv_a", "ret", json!({"state": "retired", "successor_id": "mdl_ret2"})),
                model(
                    "mdl_ret2",
                    "x/retired-too",
                    "prv_a",
                    "ret2",
                    json!({"state": "retired", "successor_id": "mdl_sonnet"}),
                ),
                model("mdl_dead", "x/dead-end", "prv_a", "dead", json!({"state": "retired"})),
                model("mdl_cross", "x/cross", "prv_a", "cross", json!({"state": "retired", "successor_id": "mdl_gpt"})),
                row("org_grant", "g1", Some("org_1"), json!({"model_id": "mdl_dep"})),
                row("org_grant", "g2", Some("org_1"), json!({"model_id": "mdl_off"})),
                row("org_grant", "g3", Some("org_1"), json!({"model_id": "mdl_ret"})),
                row("org_grant", "g4", Some("org_1"), json!({"model_id": "mdl_dead"})),
                row("org_grant", "g5", Some("org_1"), json!({"model_id": "mdl_cross"})),
                row("org_grant", "g6", Some("org_1"), json!({"model_id": "mdl_ret2", "follow_successor": false})),
            ],
            json!({}),
        );
        // Deprecated: served, with a notice.
        let t = run(&snap, by_model("x/deprecated")).unwrap();
        assert_eq!(t.deprecation_header(&snap).as_deref(), Some("2027-01-31; successor=anthropic/claude-sonnet"));
        // Disabled: 503.
        let e = run(&snap, by_model("x/disabled")).unwrap_err();
        assert_eq!((e.status.as_u16(), e.kind), (503, "model_disabled"));
        // Retired: the successor chain is followed; the grant on the retired model covers it.
        let t = run(&snap, by_model("x/retired")).unwrap();
        assert_eq!(t.model.id, "mdl_sonnet");
        assert_eq!(t.replaced.as_ref().unwrap().id, "mdl_ret");
        // Retired without a successor, or where the grant forbids following: 410.
        assert_eq!(run(&snap, by_model("x/dead-end")).unwrap_err().status.as_u16(), 410);
        assert_eq!(run(&snap, by_model("x/retired-too")).unwrap_err().status.as_u16(), 410);
        // Proxy mode cannot follow a successor to another provider; managed mode can.
        let p = snap.provider_by_name("anthropic").unwrap().clone();
        let e = run(&snap, ResolveRequest { model: "x/cross", template: None, provider: Some(&p), families: None })
            .unwrap_err();
        assert_eq!(e.status.as_u16(), 410);
        assert_eq!(run(&snap, by_model("x/cross")).unwrap().model.id, "mdl_gpt");
    }

    #[test]
    fn interface_and_provider_restrictions() {
        let snap = snapshot(vec![template("tpl_1", "planner", "anthropic/claude-haiku")], json!({}));
        // The OpenAI-compatible interface of stage 2 serves only the OpenAI family.
        let e = run(
            &snap,
            ResolveRequest {
                model: "anthropic/claude-sonnet",
                template: None,
                provider: None,
                families: Some(&[Family::Openai]),
            },
        )
        .unwrap_err();
        assert_eq!(e.kind, "unsupported_feature");
        assert!(
            run(
                &snap,
                ResolveRequest {
                    model: "openai/gpt-x",
                    template: None,
                    provider: None,
                    families: Some(&[Family::Openai])
                }
            )
            .is_ok()
        );
        // A template whose model is at another provider than the one in the proxy URL.
        let p = snap.provider_by_name("openai").unwrap().clone();
        let e = run(&snap, ResolveRequest { model: "@planner", template: None, provider: Some(&p), families: None })
            .unwrap_err();
        assert_eq!(e.kind, "template_provider_mismatch");
    }
}
