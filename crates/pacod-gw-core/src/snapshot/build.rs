//! Builds a snapshot from the rows of `config_objects`.

use std::collections::HashMap;
use std::sync::Arc;

use zeroize::Zeroizing;

use super::{Credentials, KeyRt, LimitRt, ModelRt, OrgRt, ProviderRt, Secret, Snapshot, TemplateRt, WorkspaceRt};
use crate::crypto::envelope::{self, Kek};
use crate::model::settings::{GLOBAL_SETTINGS_ID, ResolvedSettings};
use crate::model::{
    AdminToken, ApiKey, Credential, CredentialScope, CredentialState, Doc, EndUserOverride, GlobalSettings, Model, Obj,
    OrgGrant, OrgLimit, Organisation, Price, PriceStatus, Provider, RoleBinding, RouteSource, RoutingScheme, Status,
    Template, TemplateSpec, User, Workspace, WorkspaceGrant, WorkspaceLimit,
};
use crate::model::{Enforcement, LimitSpec, LimitSubject, Metric, RateClass, Window};
use crate::routing::{self, PoolMember, RouteRegistry, stable_hash};
use crate::store::Row;

fn decode_hmac(hex_str: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_str, &mut out).ok()?;
    Some(out)
}

fn typed<T: Doc>(rows: Vec<Row>, problems: &mut Vec<String>) -> Vec<Obj<T>> {
    rows.into_iter()
        .filter_map(|r| match r.into_obj::<T>() {
            Ok(o) => Some(o),
            Err(e) => {
                problems.push(e.to_string());
                None
            }
        })
        .collect()
}

/// Groups objects by their parent.
fn by_parent<T>(objs: Vec<Obj<T>>) -> HashMap<String, Vec<Obj<T>>> {
    let mut map: HashMap<String, Vec<Obj<T>>> = HashMap::new();
    for o in objs {
        map.entry(o.parent.clone().unwrap_or_default()).or_default().push(o);
    }
    map
}

/// Builds the snapshot for `version` from all configuration rows.
///
/// A row that cannot be used (a document that no longer matches its type, a credential that
/// does not decrypt, a template whose model is gone) is left out and reported in `problems`.
/// One bad object must not stop the rest of the configuration from loading.
pub fn build(version: i64, rows: Vec<Row>, keks: &[Kek]) -> Snapshot {
    build_with(version, rows, keks, &RouteRegistry::default())
}

/// [`build`], with the routing state that outlives snapshots: templates of the new snapshot
/// refer to the same per-model state as those of the old one.
pub fn build_with(version: i64, rows: Vec<Row>, keks: &[Kek], registry: &RouteRegistry) -> Snapshot {
    let mut snap = Snapshot { version, ..Default::default() };
    let mut by_kind: HashMap<String, Vec<Row>> = HashMap::new();
    for row in rows {
        by_kind.entry(row.kind.clone()).or_default().push(row);
    }
    let mut problems = Vec::new();
    let mut take = |kind: &str| by_kind.remove(kind).unwrap_or_default();

    // settings
    for o in typed::<GlobalSettings>(take(GlobalSettings::KIND), &mut problems) {
        if o.id == GLOBAL_SETTINGS_ID {
            snap.settings = o.doc;
        }
    }

    // the price tracker
    for o in typed::<crate::model::PriceJob>(take(crate::model::PriceJob::KIND), &mut problems) {
        if o.id == crate::model::PRICE_JOB_ID {
            snap.price_job = o.doc;
        }
    }

    // models
    for o in typed::<Model>(take(Model::KIND), &mut problems) {
        snap.model_names.insert(o.doc.name.clone(), o.id.clone());
        snap.models.insert(o.id.clone(), Arc::new(ModelRt { id: o.id, doc: o.doc }));
    }

    // prices: active versions per model, newest first
    for o in typed::<Price>(take(Price::KIND), &mut problems) {
        if o.doc.status == PriceStatus::Active {
            snap.prices.entry(o.doc.model_id.clone()).or_default().push(Arc::new(o.doc));
        }
    }
    for versions in snap.prices.values_mut() {
        versions.sort_by_key(|v| std::cmp::Reverse(v.version));
    }

    // providers with their credentials
    let mut creds: HashMap<String, Credentials> = HashMap::new();
    for o in typed::<Credential>(take(Credential::KIND), &mut problems) {
        if o.doc.state != CredentialState::Current {
            continue;
        }
        let value = match envelope::open(keks, &o.id, &o.doc.sealed) {
            Ok(bytes) => match String::from_utf8(bytes.to_vec()) {
                Ok(s) => Zeroizing::new(s),
                Err(_) => {
                    problems.push(format!("credential {} is not text", o.id));
                    continue;
                }
            },
            Err(e) => {
                problems.push(format!("credential {}: {e}", o.id));
                continue;
            }
        };
        let secret = Arc::new(Secret { id: o.id.clone(), value });
        let set = creds.entry(o.doc.provider_id.clone()).or_default();
        // Ids sort by creation time and rows arrive ordered by id, so with several current
        // credentials in one scope the newest wins.
        match o.doc.scope {
            CredentialScope::Global => set.global = Some(secret),
            CredentialScope::Organisation(id) => {
                set.organisations.insert(id, secret);
            }
            CredentialScope::Workspace(id) => {
                set.workspaces.insert(id, secret);
            }
        }
    }
    for o in typed::<Provider>(take(Provider::KIND), &mut problems) {
        let proxy_origin = match o.doc.proxy_origin() {
            Ok(u) => u,
            Err(e) => {
                problems.push(format!("provider {} has an invalid base URL: {e}", o.doc.name));
                continue;
            }
        };
        snap.provider_names.insert(o.doc.name.clone(), o.id.clone());
        let credentials = creds.remove(&o.id).unwrap_or_default();
        let usage_fields = Arc::new(crate::extract::UsageFields {
            cached: o.doc.quirks.cached_tokens_at.clone(),
            reasoning: o.doc.quirks.reasoning_tokens_at.clone(),
            cache_write: o.doc.quirks.cache_write_tokens_at.clone(),
        });
        snap.providers.insert(
            o.id.clone(),
            Arc::new(ProviderRt { id: o.id, doc: o.doc, proxy_origin, credentials, usage_fields }),
        );
    }

    // organisations with their grants, limits and routing schemes
    let mut org_grants = by_parent(typed::<OrgGrant>(take(OrgGrant::KIND), &mut problems));
    let mut org_limits = by_parent(typed::<OrgLimit>(take(OrgLimit::KIND), &mut problems));
    let mut org_schemes = by_parent(typed::<RoutingScheme>(take(RoutingScheme::KIND), &mut problems));
    let mut org_limit_rts: HashMap<String, Vec<Arc<LimitRt>>> = HashMap::new();
    for o in typed::<Organisation>(take(Organisation::KIND), &mut problems) {
        let mut grants: HashMap<String, OrgGrant> =
            org_grants.remove(&o.id).unwrap_or_default().into_iter().map(|g| (g.doc.model_id.clone(), g.doc)).collect();
        // The gateway's own calls may use every model in service.
        if o.id == crate::model::SYSTEM_ORG_ID {
            for m in snap.models.values().filter(|m| {
                matches!(m.doc.state, crate::model::ModelState::Active | crate::model::ModelState::Deprecated)
            }) {
                grants.entry(m.id.clone()).or_insert_with(|| OrgGrant {
                    model_id: m.id.clone(),
                    follow_successor: true,
                    max_output_tokens: None,
                    max_reasoning_tokens: None,
                    max_timeout_seconds: None,
                });
            }
        }
        let limits: Vec<Arc<LimitRt>> = org_limits
            .remove(&o.id)
            .unwrap_or_default()
            .into_iter()
            .filter(|l| l.doc.0.status == Status::Active)
            .map(|l| Arc::new(LimitRt { id: l.id, spec: l.doc.0, owner_id: o.id.clone() }))
            .collect();
        org_limit_rts.insert(o.id.clone(), limits.clone());
        let mut schemes = HashMap::new();
        for r in org_schemes.remove(&o.id).unwrap_or_default() {
            match routing::compile(&r.id, r.version, &r.doc) {
                Ok(c) => {
                    schemes.insert(r.doc.name.clone(), Arc::new(c));
                }
                Err(e) => problems.push(format!("routing scheme {}: {e}", r.doc.name)),
            }
        }
        snap.organisations.insert(o.id.clone(), Arc::new(OrgRt { id: o.id, doc: o.doc, grants, limits, schemes }));
    }

    // workspaces with their grants, templates, limits and end-user overrides
    let mut ws_grants = by_parent(typed::<WorkspaceGrant>(take(WorkspaceGrant::KIND), &mut problems));
    let mut ws_templates = by_parent(typed::<Template>(take(Template::KIND), &mut problems));
    let mut ws_limits = by_parent(typed::<WorkspaceLimit>(take(WorkspaceLimit::KIND), &mut problems));
    let mut ws_overrides = by_parent(typed::<EndUserOverride>(take(EndUserOverride::KIND), &mut problems));
    for o in typed::<Workspace>(take(Workspace::KIND), &mut problems) {
        let Some(org) = o.parent.as_ref().and_then(|p| snap.organisations.get(p)) else {
            problems.push(format!("workspace {} has no organisation", o.id));
            continue;
        };
        let settings = ResolvedSettings::resolve(&snap.settings, &org.doc.settings, &o.doc.settings, &o.doc.tag_keys);
        let enabled = o.doc.status == Status::Active && org.doc.status == Status::Active;

        let grants = ws_grants.remove(&o.id).map(|v| v.into_iter().map(|g| (g.doc.model_id.clone(), g.doc)).collect());

        let mut templates = HashMap::new();
        for t in ws_templates.remove(&o.id).unwrap_or_default() {
            if t.doc.status != Status::Active {
                continue;
            }
            let Some(current) = t.doc.current() else {
                problems.push(format!("template {} has no current version", t.doc.name));
                continue;
            };
            let Some(tpl) = template_rt(
                &snap,
                registry,
                org,
                &o.id,
                &o.doc.settings,
                t.id,
                &t.doc.name,
                current.version,
                &current.spec,
                &mut problems,
            ) else {
                continue;
            };
            templates.insert(t.doc.name.clone(), Arc::new(tpl));
        }

        let mut limits: Vec<Arc<LimitRt>> = ws_limits
            .remove(&o.id)
            .unwrap_or_default()
            .into_iter()
            .filter(|l| l.doc.0.status == Status::Active)
            .map(|l| Arc::new(LimitRt { id: l.id, spec: l.doc.0, owner_id: o.id.clone() }))
            .collect();
        limits.extend(org_limit_rts.get(&org.id).cloned().unwrap_or_default());

        let overrides = ws_overrides
            .remove(&o.id)
            .unwrap_or_default()
            .into_iter()
            .map(|ov| ((ov.doc.limit_id.clone(), ov.doc.end_user.clone()), ov.doc))
            .collect();

        snap.workspaces.insert(
            o.id.clone(),
            Arc::new(WorkspaceRt {
                id: o.id,
                organisation_id: org.id.clone(),
                doc: o.doc,
                settings,
                enabled,
                grants,
                templates,
                limits,
                overrides,
            }),
        );
    }

    // rate classes, with the per-minute limits each key in them gets
    let mut class_limits: HashMap<String, Vec<Arc<LimitRt>>> = HashMap::new();
    for o in typed::<RateClass>(take(RateClass::KIND), &mut problems) {
        if o.doc.status != Status::Active {
            continue;
        }
        let limit = |suffix: &str, metric: Metric, amount: f64| {
            Arc::new(LimitRt {
                // Stable, so that the counters survive a reload.
                id: format!("{}_{suffix}", o.id),
                spec: LimitSpec {
                    name: format!("rate class {} ({suffix})", o.doc.name),
                    description: String::new(),
                    subject: LimitSubject::Key,
                    metric,
                    window: Window::Minute,
                    amount,
                    enforcement: Enforcement::Request,
                    filter: Default::default(),
                    alert_thresholds: Vec::new(),
                    status: Status::Active,
                },
                owner_id: String::new(),
            })
        };
        let mut limits = Vec::new();
        if let Some(n) = o.doc.requests_per_minute {
            limits.push(limit("rpm", Metric::Requests, f64::from(n)));
        }
        if let Some(n) = o.doc.tokens_per_minute {
            limits.push(limit("tpm", Metric::Tokens, n as f64));
        }
        class_limits.insert(o.doc.name.clone(), limits);
        snap.rate_classes.insert(o.doc.name.clone(), Arc::new(o.doc));
    }

    // API keys
    for o in typed::<ApiKey>(take(ApiKey::KIND), &mut problems) {
        let Some(ws) = o.parent.as_ref().and_then(|p| snap.workspaces.get(p)) else {
            problems.push(format!("API key {} has no workspace", o.id));
            continue;
        };
        let Some(hmac) = decode_hmac(&o.doc.secret_hmac) else {
            problems.push(format!("API key {} has a malformed hash", o.id));
            continue;
        };
        let prev = o.doc.prev_hmac.as_deref().and_then(decode_hmac).zip(o.doc.prev_valid_until);
        let class = o.doc.rate_class.as_deref();
        if class.is_some_and(|c| !snap.rate_classes.contains_key(c)) {
            problems.push(format!("API key {} names a rate class that does not exist or is disabled", o.id));
        }
        let limits = class.and_then(|c| class_limits.get(c)).cloned().unwrap_or_default();
        let class_max = class.and_then(|c| snap.rate_classes.get(c)).and_then(|c| c.max_concurrency);
        let entry = |valid_until| {
            Arc::new(KeyRt {
                id: o.id.clone(),
                workspace_id: ws.id.clone(),
                organisation_id: ws.organisation_id.clone(),
                doc: o.doc.clone(),
                valid_until,
                class_limits: limits.clone(),
                class_max_concurrency: class_max,
            })
        };
        snap.keys.insert(hmac, entry(None));
        if let Some((prev_hmac, until)) = prev {
            snap.keys.insert(prev_hmac, entry(Some(until)));
        }
    }

    // access
    for o in typed::<User>(take(User::KIND), &mut problems) {
        snap.user_emails.insert(o.doc.email.to_lowercase(), o.id.clone());
        snap.users.insert(o.id.clone(), Arc::new(o));
    }
    for o in typed::<AdminToken>(take(AdminToken::KIND), &mut problems) {
        match decode_hmac(&o.doc.secret_hmac) {
            Some(h) => {
                snap.admin_tokens.insert(h, Arc::new(o));
            }
            None => problems.push(format!("admin token {} has a malformed hash", o.id)),
        }
    }
    snap.role_bindings = typed::<RoleBinding>(take(RoleBinding::KIND), &mut problems);

    snap.problems = problems;
    routing::live::configure(&snap.settings.breaker);
    registry.prune();
    snap
}

/// The template's pool and routing. `None` when none of its models exist.
#[allow(clippy::too_many_arguments)]
fn template_rt(
    snap: &Snapshot,
    registry: &RouteRegistry,
    org: &OrgRt,
    ws_id: &str,
    ws_settings: &crate::model::ScopeSettings,
    id: String,
    name: &str,
    version: u32,
    spec: &TemplateSpec,
    problems: &mut Vec<String>,
) -> Option<TemplateRt> {
    let now = chrono::Utc::now();
    let mut pool = Vec::new();
    for entry in spec.pool().iter() {
        // The spec names a model by its id or by its object id.
        let Some(model) = snap
            .model_names
            .get(&entry.model)
            .and_then(|id| snap.models.get(id))
            .or_else(|| snap.models.get(&entry.model))
        else {
            problems.push(format!("template {name} names the unknown model {}", entry.model));
            continue;
        };
        let credential = snap
            .providers
            .get(&model.doc.provider_id)
            .and_then(|p| p.credential(ws_id, &org.id))
            .map_or_else(|| "-".to_owned(), |(s, _)| s.id.clone());
        let price = if model.doc.free {
            Some(0.0)
        } else {
            snap.price_at(&model.id, now).map(|p| p.rates.input.unwrap_or(0.0) + p.rates.output.unwrap_or(0.0))
        };
        let mut entry = entry.clone();
        entry.model = model.id.clone();
        pool.push(PoolMember {
            state: registry.get(&credential, &model.id),
            id_hash: stable_hash(&model.id),
            model: model.clone(),
            entry,
            price,
        });
    }
    if pool.is_empty() {
        return None;
    }
    let model_id = pool
        .iter()
        .filter(|m| m.entry.weight > 0)
        .min_by_key(|m| m.entry.priority)
        .unwrap_or(&pool[0])
        .model
        .id
        .clone();

    // The most specific assignment of an existing, active scheme; an organisation that locks
    // the setting decides for its workspaces and templates.
    let locked = org.doc.settings.locked.iter().any(|l| l == "routing_scheme");
    let assigned = [
        (spec.routing_scheme.as_ref().filter(|_| !locked), RouteSource::Template),
        (ws_settings.routing_scheme.as_ref().filter(|_| !locked), RouteSource::Workspace),
        (org.doc.settings.routing_scheme.as_ref(), RouteSource::Organisation),
    ];
    let mut scheme = None;
    for (n, source) in assigned {
        let Some(n) = n else { continue };
        match org.schemes.get(n) {
            Some(s) if s.enabled => {
                scheme = Some((s.clone(), source));
                break;
            }
            Some(_) => {}
            None => {
                problems.push(format!("template {name}: the routing scheme {n} ({}) does not exist", source.name()))
            }
        }
    }
    let route = routing::bind(scheme, &pool, name, problems);
    Some(TemplateRt { id, name: name.to_owned(), version, spec: spec.clone(), model_id, pool: pool.into(), route })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Family, KeyStatus, tenancy::all_modes};
    use chrono::{Duration, Utc};
    use serde_json::json;

    fn row(kind: &str, id: &str, parent: Option<&str>, doc: serde_json::Value) -> Row {
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

    fn kek() -> Kek {
        Kek::new("k1", [3u8; 32])
    }

    fn base_rows() -> Vec<Row> {
        vec![
            row("organisation", "org_1", None, json!({"name": "acme"})),
            row("workspace", "ws_1", Some("org_1"), json!({"name": "prod"})),
        ]
    }

    #[test]
    fn keys_resolve_to_workspace_and_organisation() {
        let hmac = hex::encode([7u8; 32]);
        let mut rows = base_rows();
        rows.push(row("api_key", "key_1", Some("ws_1"), json!({"name": "svc", "secret_hmac": hmac})));
        let snap = build(5, rows, &[kek()]);
        assert_eq!(snap.version, 5);
        assert!(snap.problems.is_empty(), "{:?}", snap.problems);
        let k = snap.key(&[7u8; 32], Utc::now()).unwrap();
        assert_eq!((k.workspace_id.as_str(), k.organisation_id.as_str()), ("ws_1", "org_1"));
        assert_eq!(k.doc.allowed_modes, all_modes());
        assert!(snap.workspace_of(k).is_some());
    }

    #[test]
    fn revoked_expired_and_rotated_keys() {
        let now = Utc::now();
        let mut rows = base_rows();
        let key = |id: &str, hmac: u8, extra: serde_json::Value| {
            let mut doc = json!({"name": id, "secret_hmac": hex::encode([hmac; 32])});
            doc.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            row("api_key", id, Some("ws_1"), doc)
        };
        rows.push(key("key_revoked", 1, json!({"status": "revoked"})));
        rows.push(key("key_expired", 2, json!({"expires_at": now - Duration::hours(1)})));
        rows.push(key(
            "key_rotated",
            3,
            json!({"prev_hmac": hex::encode([4u8; 32]), "prev_valid_until": now + Duration::hours(1)}),
        ));
        rows.push(key(
            "key_rotated_old",
            5,
            json!({"prev_hmac": hex::encode([6u8; 32]), "prev_valid_until": now - Duration::hours(1)}),
        ));
        let snap = build(1, rows, &[kek()]);
        assert!(snap.key(&[1u8; 32], now).is_none(), "revoked");
        assert!(snap.key(&[2u8; 32], now).is_none(), "expired");
        assert!(snap.key(&[3u8; 32], now).is_some(), "new secret");
        assert_eq!(snap.key(&[4u8; 32], now).unwrap().id, "key_rotated", "old secret within grace");
        assert!(snap.key(&[6u8; 32], now).is_none(), "old secret after grace");
        assert_eq!(snap.keys.get(&[1u8; 32]).unwrap().doc.status, KeyStatus::Revoked);
    }

    #[test]
    fn disabled_organisation_disables_its_workspaces() {
        let rows = vec![
            row("organisation", "org_1", None, json!({"name": "acme", "status": "disabled"})),
            row("workspace", "ws_1", Some("org_1"), json!({"name": "prod"})),
            row("api_key", "key_1", Some("ws_1"), json!({"name": "k", "secret_hmac": hex::encode([9u8; 32])})),
        ];
        let snap = build(1, rows, &[kek()]);
        let k = snap.key(&[9u8; 32], Utc::now()).unwrap();
        assert!(snap.workspace_of(k).is_none());
    }

    #[test]
    fn credential_precedence_workspace_then_organisation_then_shared() {
        let k = kek();
        let cred = |id: &str, scope: serde_json::Value, secret: &str| {
            let sealed = envelope::seal(&k, id, secret.as_bytes());
            row(
                "credential",
                id,
                Some("prv_1"),
                json!({"provider_id": "prv_1", "scope": scope, "sealed": sealed, "fingerprint": "f"}),
            )
        };
        let mut rows = base_rows();
        rows.push(row(
            "provider",
            "prv_1",
            None,
            json!({"name": "openai", "family": "openai", "base_url": "https://api.openai.com/v1"}),
        ));
        rows.push(cred("cred_1", json!({"type": "global"}), "shared"));
        rows.push(cred("cred_2", json!({"type": "organisation", "id": "org_1"}), "org-own"));
        rows.push(cred("cred_3", json!({"type": "workspace", "id": "ws_9"}), "ws-own"));
        let snap = build(1, rows, &[k]);
        assert!(snap.problems.is_empty(), "{:?}", snap.problems);
        let p = snap.provider_by_name("openai").unwrap();
        assert_eq!(p.doc.family, Family::Openai);
        assert_eq!(p.proxy_origin, "https://api.openai.com");
        let (s, owner) = p.credential("ws_9", "org_1").unwrap();
        assert_eq!((s.value.as_str(), owner), ("ws-own", super::super::CredentialOwner::Own));
        let (s, owner) = p.credential("ws_1", "org_1").unwrap();
        assert_eq!((s.value.as_str(), owner), ("org-own", super::super::CredentialOwner::Own));
        let (s, owner) = p.credential("ws_x", "org_x").unwrap();
        assert_eq!((s.value.as_str(), owner), ("shared", super::super::CredentialOwner::Shared));
    }

    #[test]
    fn bad_objects_are_reported_not_fatal() {
        let k = kek();
        let other = Kek::new("k1", [4u8; 32]);
        let sealed = envelope::seal(&other, "cred_1", b"x");
        let mut rows = base_rows();
        rows.push(row("provider", "prv_1", None, json!({"name": "a", "family": "anthropic", "base_url": "https://a"})));
        rows.push(row(
            "credential",
            "cred_1",
            Some("prv_1"),
            json!({"provider_id": "prv_1", "scope": {"type": "global"}, "sealed": sealed, "fingerprint": "f"}),
        ));
        rows.push(row("provider", "prv_2", None, json!({"name": "broken"}))); // missing fields
        rows.push(row("some_future_kind", "x_1", None, json!({})));
        let snap = build(1, rows, &[k]);
        assert_eq!(snap.problems.len(), 2, "{:?}", snap.problems);
        assert!(snap.provider_by_name("a").unwrap().credential("ws_1", "org_1").is_none());
        assert!(snap.provider_by_name("broken").is_none());
        assert_eq!(snap.workspaces.len(), 1, "the rest of the configuration still loads");
    }

    fn model_row(id: &str, name: &str, extra: serde_json::Value) -> Row {
        let mut doc = json!({ "name": name, "provider_id": "prv_1", "upstream_id": name, "state": "active" });
        doc.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        row("model", id, None, doc)
    }

    #[test]
    fn grants_intersect_and_ceilings_take_the_minimum() {
        let mut rows = base_rows();
        rows.push(row("workspace", "ws_2", Some("org_1"), json!({"name": "restricted"})));
        rows.push(model_row("mdl_a", "a", json!({})));
        rows.push(model_row("mdl_b", "b", json!({})));
        rows.push(model_row("mdl_c", "c", json!({})));
        rows.push(row("org_grant", "ogr_a", Some("org_1"), json!({"model_id": "mdl_a", "max_output_tokens": 4000})));
        rows.push(row("org_grant", "ogr_b", Some("org_1"), json!({"model_id": "mdl_b", "follow_successor": false})));
        // ws_2 restricts itself to model a, with a lower ceiling; and names model c, which the
        // organisation does not have.
        rows.push(row("ws_grant", "wgr_a", Some("ws_2"), json!({"model_id": "mdl_a", "max_output_tokens": 1000})));
        rows.push(row("ws_grant", "wgr_c", Some("ws_2"), json!({"model_id": "mdl_c"})));
        let snap = build(1, rows, &[kek()]);
        assert!(snap.problems.is_empty(), "{:?}", snap.problems);
        let ws1 = &snap.workspaces["ws_1"];
        let ws2 = &snap.workspaces["ws_2"];

        // A workspace without grants of its own may use everything its organisation has.
        let t = snap.grant_terms(ws1, "mdl_a").unwrap();
        assert_eq!((t.follow_successor, t.max_output_tokens), (true, Some(4000)));
        assert!(!snap.grant_terms(ws1, "mdl_b").unwrap().follow_successor);
        assert!(snap.grant_terms(ws1, "mdl_c").is_none(), "not granted to the organisation");

        // A workspace with grants is restricted to them, within the organisation's.
        assert_eq!(snap.grant_terms(ws2, "mdl_a").unwrap().max_output_tokens, Some(1000));
        assert!(snap.grant_terms(ws2, "mdl_b").is_none(), "not in the workspace's own list");
        assert!(snap.grant_terms(ws2, "mdl_c").is_none(), "a workspace cannot grant what its organisation lacks");
    }

    #[test]
    fn templates_resolve_their_model_and_use_the_current_version() {
        let mut rows = base_rows();
        rows.push(model_row("mdl_a", "anthropic/claude-haiku-4-5", json!({})));
        let version = |n: u32, max: u32, model: &str| json!({"version": n, "spec": {"model": model, "max_output_tokens": max}, "created_at": "2026-10-01T00:00:00Z"});
        rows.push(row(
            "template",
            "tpl_1",
            Some("ws_1"),
            json!({"name": "planner", "current_version": 2, "versions": [
                version(1, 100, "anthropic/claude-haiku-4-5"),
                version(2, 400, "mdl_a"),
                version(3, 900, "anthropic/claude-haiku-4-5"),
            ]}),
        ));
        rows.push(row(
            "template",
            "tpl_2",
            Some("ws_1"),
            json!({"name": "orphan", "current_version": 1, "versions": [version(1, 1, "no/such-model")]}),
        ));
        rows.push(row(
            "template",
            "tpl_3",
            Some("ws_1"),
            json!({"name": "off", "status": "disabled", "current_version": 1, "versions": [version(1, 1, "mdl_a")]}),
        ));
        let snap = build(1, rows, &[kek()]);
        let ws = &snap.workspaces["ws_1"];
        let t = &ws.templates["planner"];
        assert_eq!((t.version, t.spec.max_output_tokens, t.model_id.as_str()), (2, Some(400), "mdl_a"));
        assert!(!ws.templates.contains_key("orphan") && !ws.templates.contains_key("off"));
        assert_eq!(snap.problems.len(), 1);
        assert!(snap.problems[0].contains("no/such-model"));
        assert_eq!(snap.model_by_name("anthropic/claude-haiku-4-5").unwrap().id, "mdl_a");
    }

    #[test]
    fn limits_of_the_workspace_and_its_organisation_both_apply() {
        let mut rows = base_rows();
        let limit = |name: &str, subject: &str, status: &str| json!({"name": name, "subject": subject, "metric": "tokens", "window": "day", "amount": 100.0, "status": status});
        rows.push(row("org_limit", "olm_1", Some("org_1"), limit("org-cap", "organisation", "active")));
        rows.push(row("limit", "lim_1", Some("ws_1"), limit("per-user", "end_user", "active")));
        rows.push(row("limit", "lim_2", Some("ws_1"), limit("paused", "workspace", "disabled")));
        rows.push(row(
            "end_user_override",
            "euo_1",
            Some("ws_1"),
            json!({"limit_id": "lim_1", "end_user": "u1", "exempt": true}),
        ));
        let snap = build(1, rows, &[kek()]);
        assert!(snap.problems.is_empty(), "{:?}", snap.problems);
        let ws = &snap.workspaces["ws_1"];
        let names: Vec<&str> = ws.limits.iter().map(|l| l.spec.name.as_str()).collect();
        assert_eq!(names, ["per-user", "org-cap"], "disabled limits are left out");
        assert_eq!(ws.limits[1].owner_id, "org_1");
        assert!(ws.overrides[&("lim_1".to_owned(), "u1".to_owned())].exempt);
    }

    #[test]
    fn the_price_in_effect_is_the_newest_active_version_already_valid() {
        let mut rows = base_rows();
        let price = |v: u32, status: &str, from: &str, input: f64| {
            row(
                "price",
                &format!("prc_{v}"),
                Some("mdl_a"),
                json!({"model_id": "mdl_a", "version": v, "currency": "USD", "input": input, "output": 10.0,
                       "effective_from": from, "status": status}),
            )
        };
        rows.push(price(1, "superseded", "2026-01-01T00:00:00Z", 3.0));
        rows.push(price(2, "active", "2026-06-01T00:00:00Z", 2.0));
        rows.push(price(3, "proposed", "2026-09-01T00:00:00Z", 9.0));
        rows.push(price(4, "active", "2027-01-01T00:00:00Z", 1.0));
        let snap = build(1, rows, &[kek()]);
        let at = |s: &str| s.parse::<chrono::DateTime<Utc>>().unwrap();
        assert_eq!(snap.price_at("mdl_a", at("2026-10-04T00:00:00Z")).unwrap().version, 2);
        assert_eq!(snap.price_at("mdl_a", at("2027-02-01T00:00:00Z")).unwrap().version, 4);
        assert!(snap.price_at("mdl_a", at("2026-03-01T00:00:00Z")).is_none(), "the superseded version is not used");
        assert!(snap.price_at("mdl_x", at("2026-10-04T00:00:00Z")).is_none());
    }
}
