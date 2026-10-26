//! The configuration as the full gateway's configuration rows (`config_objects`), from which
//! the core builds its snapshot. Nothing is stored: the rows live as long as the process.
//!
//! One organisation and one workspace hold everything. Provider keys are sealed under a
//! key-encryption key made for this process, and client keys are hashed with a pepper made for
//! this process; both are forgotten when it ends.

use chrono::{DateTime, Utc};
use pacod_gw_core::crypto::envelope::{self, Kek};
use pacod_gw_core::crypto::keys::Pepper;
use pacod_gw_core::model::settings::GLOBAL_SETTINGS_ID;
use pacod_gw_core::model::{
    ApiKey, AuditAttachments, AuditGuarantee, Credential, CredentialScope, CredentialState, Doc, GlobalSettings, Model,
    OrgGrant, Organisation, PluginBinding, PluginInstall, Price, Provider, RateClass, RoutingScheme, Template,
    UsagePolicy, Workspace, WorkspaceIntentCatalogue, WorkspacePluginBinding,
};
use pacod_gw_core::store::Row;
use pacod_gw_plugin_intent::settings::EngineChoice;
use serde_json::{Value, json};

use crate::config::{Config, PriceCfg};

pub const ORG_ID: &str = "org_lite";
pub const WS_ID: &str = "ws_lite";

pub fn provider_id(name: &str) -> String {
    format!("prv_{name}")
}

pub fn model_id(label: &str) -> String {
    format!("mdl_{}", label.to_ascii_lowercase())
}

fn scheme_name(pool: &str) -> String {
    format!("pool-{pool}")
}

fn rate_class_name(client: &str) -> String {
    format!("client-{client}")
}

/// A client key's allow-list of pools when it may use none: no pool has this name.
const NO_POOL: &str = "-";

fn row<T: Doc>(id: impl Into<String>, parent: Option<&str>, doc: Value, now: DateTime<Utc>) -> Row {
    Row {
        id: id.into(),
        kind: T::KIND.to_owned(),
        parent: parent.map(str::to_owned),
        name: None,
        doc,
        version: 1,
        created_at: now,
        updated_at: now,
    }
}

/// Every row of the configuration.
pub fn rows(cfg: &Config, kek: &Kek, pepper: &Pepper) -> anyhow::Result<Vec<Row>> {
    let now = Utc::now();
    let mut rows = Vec::new();

    let settings = GlobalSettings {
        unsupported_params: cfg.unsupported_params,
        reporting_currency: cfg.currency.clone(),
        audit_max_body_bytes: cfg.log_body_max_bytes,
        // No audit rule: `forbid` would switch the response cache off as well. Bodies off still
        // means never, also for a call an intent rule flags: the server's sink drops its record.
        ..Default::default()
    };
    rows.push(row::<GlobalSettings>(GLOBAL_SETTINGS_ID, None, serde_json::to_value(settings)?, now));

    rows.push(row::<Organisation>(ORG_ID, None, json!({ "name": "lite", "display_name": "pacod-gw-lite" }), now));
    // The body log is the full gateway's audit mode, kept on the log instead of in storage. A
    // record that cannot be kept never refuses a call.
    rows.push(row::<Workspace>(
        WS_ID,
        Some(ORG_ID),
        json!({
            "name": "default",
            "settings": { "audit": {
                "mode": cfg.log_bodies,
                "attachments": AuditAttachments::Reference,
                "guarantee": AuditGuarantee::BestEffort,
            } },
        }),
        now,
    ));

    for p in &cfg.providers {
        let id = provider_id(&p.name);
        let mut doc = serde_json::to_value(&p.preset)?;
        let obj = doc.as_object_mut().expect("a preset is an object");
        obj.remove("notes");
        obj.insert("name".into(), json!(p.name));
        obj.insert("base_url".into(), json!(p.base_url));
        obj.insert("default_timeout_seconds".into(), json!(p.timeout_seconds));
        // What a preset says its provider serves beyond chat holds for the provider's own
        // address only: the preset `openai` pointed at another OpenAI-compatible server (vLLM,
        // a proxy) serves chat completions, not necessarily the Responses API.
        if p.base_url.trim_end_matches('/') != p.preset.base_url.trim_end_matches('/')
            && let Some(quirks) = obj.get_mut("quirks").and_then(|q| q.as_object_mut())
        {
            quirks.insert("responses_api".into(), json!(false));
            quirks.insert("openai_objects".into(), json!(false));
        }
        rows.push(row::<Provider>(id.clone(), None, doc, now));

        // Every provider gets a credential of the organisation, an empty one when it takes
        // none, so that the gateway treats it as the operator's own throughout.
        let cred_id = format!("cred_{}", p.name);
        let secret = p.key.as_ref().map_or("", |k| k.0.as_str());
        let credential = Credential {
            provider_id: id,
            scope: CredentialScope::Organisation(ORG_ID.to_owned()),
            label: "pacod-gw-lite".into(),
            sealed: envelope::seal(kek, &cred_id, secret.as_bytes()),
            fingerprint: if secret.is_empty() { String::new() } else { envelope::fingerprint(secret) },
            state: CredentialState::Current,
            retire_at: None,
        };
        rows.push(row::<Credential>(cred_id, None, serde_json::to_value(credential)?, now));
    }

    for m in &cfg.models {
        let id = model_id(&m.label);
        let capabilities = pacod_gw_core::model::repo::capability_presets()
            .into_iter()
            .find(|p| p.name == m.capabilities)
            .map(|p| p.capabilities)
            .unwrap_or_default();
        rows.push(row::<Model>(
            id.clone(),
            None,
            json!({
                "name": m.name,
                "provider_id": provider_id(&m.provider),
                "upstream_id": m.upstream,
                "kind": m.kind,
                "context_window": m.context,
                "max_output_tokens": m.max_output,
                "capabilities": capabilities,
                "state": "active",
                "free": m.price == PriceCfg::Free,
            }),
            now,
        ));
        if let PriceCfg::Rates { rates, currency } = &m.price {
            // The rates sit at the top of a price, next to its other fields.
            let mut doc = serde_json::to_value(rates)?;
            let obj = doc.as_object_mut().expect("rates are an object");
            obj.extend([
                ("model_id".to_owned(), json!(id)),
                ("version".to_owned(), json!(1)),
                ("currency".to_owned(), json!(currency)),
                ("effective_from".to_owned(), json!("2000-01-01T00:00:00Z")),
                ("source".to_owned(), json!("manual")),
                ("status".to_owned(), json!("active")),
            ]);
            rows.push(row::<Price>(format!("prc_{}", m.label.to_ascii_lowercase()), None, doc, now));
        }
        rows.push(row::<OrgGrant>(
            format!("ogr_{}", m.label.to_ascii_lowercase()),
            Some(ORG_ID),
            json!({ "model_id": id }),
            now,
        ));
    }

    for p in &cfg.pools {
        // A scheme of its own carries the pool's algorithm and lets a call try every model of
        // the pool, not only the first three.
        let scheme = scheme_name(&p.name);
        rows.push(row::<RoutingScheme>(
            format!("rsc_{}", p.name),
            Some(ORG_ID),
            json!({
                "name": scheme,
                "description": format!("the pool @{}", p.name),
                "default": { "algorithm": p.algorithm },
                "failover": { "max_targets": p.models.len() },
            }),
            now,
        ));
        let models: Vec<Value> = p
            .models
            .iter()
            .zip(&p.weights)
            .enumerate()
            .map(|(i, (m, w))| json!({ "model": m, "weight": w, "priority": i }))
            .collect();
        rows.push(row::<Template>(
            format!("tpl_{}", p.name),
            Some(WS_ID),
            json!({
                "name": p.name,
                "current_version": 1,
                "versions": [{
                    "version": 1,
                    "spec": {
                        "models": models,
                        "routing_scheme": scheme,
                        "timeout_seconds": p.timeout_seconds,
                        "retry_max_attempts": p.retries,
                    },
                    "created_at": now,
                }],
            }),
            now,
        ));
    }

    for c in &cfg.clients {
        let class = (c.rpm.is_some() || c.tpm.is_some()).then(|| rate_class_name(&c.name));
        if let Some(class) = &class {
            rows.push(row::<RateClass>(
                format!("rcl_{}", c.name),
                None,
                json!({ "name": class, "requests_per_minute": c.rpm, "tokens_per_minute": c.tpm }),
                now,
            ));
        }
        let (models, templates) = match &c.allowed {
            None => (Vec::new(), Vec::new()),
            Some(a) => {
                // Listing a pool allows its models: a pool calls them as the key.
                let mut models = a.models.clone();
                for pool in cfg.pools.iter().filter(|p| a.pools.contains(&p.name)) {
                    models.extend(pool.models.iter().filter(|m| !a.models.contains(m)).cloned());
                }
                models.dedup();
                let templates = if a.pools.is_empty() { vec![NO_POOL.to_owned()] } else { a.pools.clone() };
                (models, templates)
            }
        };
        let hint: String = c.key.0.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        // The key's id is the client's name: it is what the log shows as `key`.
        rows.push(row::<ApiKey>(
            c.name.clone(),
            Some(WS_ID),
            json!({
                "name": c.name,
                "secret_hmac": hex::encode(pepper.hash(&c.key.0)),
                "hint": hint,
                "allowed_models": models,
                "allowed_templates": templates,
                "rate_class": class,
                "max_concurrency": c.concurrency,
                // Proxy mode estimates what a response does not report rather than refusing
                // calls that cannot report it, even under a tokens-per-minute limit.
                "usage_policy": UsagePolicy::Estimate,
            }),
            now,
        ));
    }

    if let Some(i) = &cfg.intent {
        // The platform's install approves the third party when the engine sends text to one:
        // setting the variables is the operator's approval.
        let uses_jev = matches!(i.engine, EngineChoice::Jev | EngineChoice::Cascade);
        let install = PluginInstall {
            plugin: pacod_gw_plugin_intent::ID.into(),
            enabled: true,
            engines: Vec::new(),
            third_parties: if uses_jev { vec![pacod_gw_plugin_intent::TYPESAFE.into()] } else { Vec::new() },
            default_binding: None,
        };
        rows.push(row::<PluginInstall>("pin_intent", None, serde_json::to_value(install)?, now));
        rows.push(row::<WorkspaceIntentCatalogue>("icg_lite", Some(WS_ID), serde_json::to_value(&i.catalogue)?, now));
        let mut settings = json!({ "catalogue": i.catalogue.name, "engine": i.engine });
        if let Some(m) = &i.model {
            settings["llm"] = json!({ "model": m });
        }
        if let Some(m) = &i.jev_model {
            settings["jev"] = json!({ "model": m });
        }
        let binding = PluginBinding {
            plugin: pacod_gw_plugin_intent::ID.into(),
            mode: i.mode,
            priority: 100,
            timeout_ms: i.timeout_ms,
            on_error: i.on_error,
            expose: i.expose,
            locked: false,
            settings,
        };
        rows.push(row::<WorkspacePluginBinding>("pbd_intent", Some(WS_ID), serde_json::to_value(binding)?, now));
    }

    // The response cache, as the full gateway installs it: the platform's binding applies to
    // the one workspace.
    if let Some(c) = &cfg.cache {
        let settings = pacod_gw_plugin_cache::Settings {
            calls: c.calls,
            ttl_seconds: c.ttl_seconds,
            max_answer_bytes: c.max_answer_bytes,
        };
        let install = PluginInstall {
            plugin: pacod_gw_plugin_cache::ID.into(),
            enabled: true,
            engines: Vec::new(),
            third_parties: Vec::new(),
            default_binding: Some(PluginBinding {
                plugin: pacod_gw_plugin_cache::ID.into(),
                mode: c.mode,
                priority: 100,
                timeout_ms: None,
                on_error: Default::default(),
                expose: false,
                locked: false,
                settings: serde_json::to_value(settings)?,
            }),
        };
        rows.push(row::<PluginInstall>("pin_response_cache", None, serde_json::to_value(install)?, now));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use pacod_gw_core::model::{Algorithm, CredentialScope, Metric, ModelKind, PluginMode};
    use pacod_gw_core::snapshot::{ConfigState, CredentialOwner};

    use super::*;
    use crate::config::read;
    use crate::env::Vars;

    const KEY: &str = "pgw_portal_0123456789abcdefghij";
    const OTHER: &str = "pgw_ci_abcdefghij0123456789";

    fn holder(pairs: &[(&str, &str)]) -> (Arc<ConfigState>, Pepper) {
        let cfg = read(&Vars::from_pairs(pairs.iter().copied())).unwrap();
        let kek = Kek::new("test", [9u8; 32]);
        let pepper = Pepper::new(b"0123456789abcdef-test".to_vec()).unwrap();
        let state = ConfigState::from_rows(rows(&cfg, &kek, &pepper).unwrap(), vec![kek]);
        assert!(state.snapshot().problems.is_empty(), "{:?}", state.snapshot().problems);
        (state, pepper)
    }

    fn example() -> Vec<(&'static str, &'static str)> {
        vec![
            ("PGW_PROVIDER_ANTHROPIC_KEY", "sk-ant-test"),
            ("PGW_PROVIDER_OPENAI_KEY", "sk-test"),
            ("PGW_PROVIDER_LOCAL_PRESET", "ollama"),
            ("PGW_MODEL_SONNET", "anthropic/claude-sonnet-5-5"),
            ("PGW_MODEL_SONNET_PRICE", "input=3,output=15,cache_read=0.3"),
            ("PGW_MODEL_MINI", "openai/gpt-5.6-luna"),
            ("PGW_MODEL_LLAMA", "local/llama3"),
            ("PGW_MODEL_LLAMA_PRICE", "free"),
            ("PGW_MODEL_EMB", "openai/text-embedding-3-small"),
            (
                "PGW_POOL_FAST",
                "openai/gpt-5.6-luna,anthropic/claude-sonnet-5-5,local/llama3,openai/text-embedding-3-small",
            ),
            ("PGW_POOL_FAST_ALGORITHM", "weighted"),
            ("PGW_POOL_FAST_WEIGHTS", "3,1,1,0"),
            ("PGW_CLIENT_PORTAL_KEY", KEY),
            ("PGW_CLIENT_PORTAL_RPM", "600"),
            ("PGW_CLIENT_PORTAL_TPM", "100000"),
            ("PGW_CLIENT_PORTAL_CONCURRENCY", "8"),
            ("PGW_CLIENT_CI_KEY", OTHER),
            ("PGW_CLIENT_CI_MODELS", "local/llama3"),
            ("PGW_LOG_BODIES", "on_request"),
        ]
    }

    #[test]
    fn the_snapshot_holds_what_the_variables_say() {
        let (state, pepper) = holder(&example());
        let snap = state.snapshot();
        assert_eq!(snap.version, 1);
        assert_eq!(snap.settings.reporting_currency, "USD");

        // Keys are found by the hash of the presented secret, under the client's name.
        let key = snap.key(&pepper.hash(KEY), Utc::now()).expect("the client key is known");
        assert_eq!((key.id.as_str(), key.workspace_id.as_str()), ("portal", WS_ID));
        assert_eq!(key.max_concurrency(), Some(8));
        let metrics: Vec<Metric> = key.class_limits.iter().map(|l| l.spec.metric).collect();
        assert_eq!(metrics, [Metric::Requests, Metric::Tokens]);
        assert_eq!(key.class_limits[0].spec.amount, 600.0);
        let ws = snap.workspace_of(key).expect("the workspace is enabled");
        assert_eq!(key.usage_policy(ws), UsagePolicy::Estimate, "even under a tokens-per-minute limit");
        assert!(snap.key(&pepper.hash("pgw_portal_wrong"), Utc::now()).is_none());

        // Every model is granted, and the provider's credential is the operator's own.
        let sonnet = snap.model_by_name("anthropic/claude-sonnet-5-5").unwrap();
        assert!(snap.grant_terms(ws, &sonnet.id).is_some());
        assert_eq!(sonnet.doc.upstream_id, "claude-sonnet-5-5");
        let price = snap.price_at(&sonnet.id, Utc::now()).expect("a price in effect");
        assert_eq!((price.rates.input, price.rates.output, price.currency.as_str()), (Some(3.0), Some(15.0), "USD"));
        let anthropic = snap.provider_by_name("anthropic").unwrap();
        let (secret, owner) = anthropic.credential(WS_ID, ORG_ID).unwrap();
        assert_eq!((secret.value.as_str(), owner), ("sk-ant-test", CredentialOwner::Own));
        let local = snap.provider_by_name("local").unwrap();
        assert_eq!(
            local.credential(WS_ID, ORG_ID).map(|(s, o)| (s.value.is_empty(), o)),
            Some((true, CredentialOwner::Own))
        );
        assert_eq!(snap.model_by_name("openai/text-embedding-3-small").unwrap().doc.kind, ModelKind::Embedding);
        assert!(snap.model_by_name("local/llama3").unwrap().doc.free);
        // OpenAI at its own address serves the Responses API; the same preset elsewhere does not.
        assert!(snap.provider_by_name("openai").unwrap().doc.quirks.responses_api);
        let (elsewhere, _) =
            holder(&[example(), vec![("PGW_PROVIDER_OPENAI_BASE_URL", "http://vllm:8000/v1")]].concat());
        assert!(!elsewhere.snapshot().provider_by_name("openai").unwrap().doc.quirks.responses_api);

        // The pool: its own scheme, all four models tried, weights and order kept.
        let fast = &ws.templates["fast"];
        assert_eq!(fast.pool.len(), 4);
        let weights: Vec<u32> = fast.pool.iter().map(|m| m.entry.weight).collect();
        assert_eq!(weights, [3, 1, 1, 0]);
        assert_eq!(fast.route.scheme.as_ref().map(|s| &*s.name), Some("pool-fast"));
        assert_eq!(fast.route.default.algorithm, Algorithm::Weighted);
        assert_eq!(fast.route.default.failover.max_targets, 4);

        // Body logging is the workspace's audit mode, best effort.
        assert_eq!(ws.settings.audit.mode, pacod_gw_core::model::AuditMode::OnRequest);
        assert_eq!(ws.settings.audit.guarantee, pacod_gw_core::model::AuditGuarantee::BestEffort);
        assert_eq!(ws.settings.audit_max_body_bytes, 16 << 10);
    }

    #[test]
    fn a_restricted_client_gets_only_what_it_lists() {
        let (state, pepper) = holder(&example());
        let snap = state.snapshot();
        let ci = snap.key(&pepper.hash(OTHER), Utc::now()).unwrap();
        assert_eq!(ci.doc.allowed_models, ["local/llama3"]);
        assert_eq!(ci.doc.allowed_templates, [NO_POOL], "no pool is allowed");
        assert!(ci.class_limits.is_empty());
        assert_eq!(ci.max_concurrency(), None);

        let mut pairs = example();
        pairs.push(("PGW_CLIENT_CI_MODELS", "@fast"));
        let (state, pepper) = holder(&pairs);
        let snap = state.snapshot();
        let ci = snap.key(&pepper.hash(OTHER), Utc::now()).unwrap();
        assert_eq!(ci.doc.allowed_templates, ["fast"]);
        assert_eq!(ci.doc.allowed_models.len(), 4, "a listed pool allows its models: {:?}", ci.doc.allowed_models);
    }

    #[test]
    fn the_intent_plugin_is_installed_and_bound_to_the_workspace() {
        const INTENTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/lite/intents.example.yaml");
        let mut pairs: Vec<_> = example().into_iter().filter(|(k, _)| *k != "PGW_LOG_BODIES").collect();
        pairs.extend([
            ("PGW_PROVIDER_TYPESAFE_KEY", "ts-test"),
            ("PGW_MODEL_JEV", "typesafe/jev-1.13.0"),
            ("PGW_INTENT_MODE", "enforce"),
            ("PGW_INTENT_FILE", INTENTS),
            ("PGW_INTENT_ENGINE", "cascade"),
            ("PGW_INTENT_MODEL", "@fast"),
        ]);
        let (state, pepper) = holder(&pairs);
        let snap = state.snapshot();
        let ws = snap.workspace_of(snap.key(&pepper.hash(KEY), Utc::now()).unwrap()).unwrap();
        assert_eq!(ws.plugins.len(), 2, "the intent plugin and the response cache");
        let b = ws.plugins.iter().find(|b| b.plugin == "intent").unwrap();
        assert_eq!(b.mode, PluginMode::Enforce);
        // Bound: the third party is approved and both models are found.
        assert!(b.bound.is_ok(), "{:?}", b.bound.as_ref().err());
        assert!(ws.catalogues.contains_key("support") && ws.enforces_plugins());
        // Bodies off is no audit rule, which would switch the response cache off too.
        assert_eq!(ws.settings.audit_rule, None);

        // Without the plugin nothing of it is there, and bodies asked for can be logged.
        let (state, pepper) = holder(&example());
        let snap = state.snapshot();
        let ws = snap.workspace_of(snap.key(&pepper.hash(KEY), Utc::now()).unwrap()).unwrap();
        assert!(ws.plugins.iter().all(|b| b.plugin != "intent") && ws.settings.audit_rule.is_none());
        assert!(!ws.enforces_plugins(), "the response cache checks nothing");
    }

    #[test]
    fn the_response_cache_is_on_unless_switched_off() {
        let (state, pepper) = holder(&example());
        let snap = state.snapshot();
        let ws = snap.workspace_of(snap.key(&pepper.hash(KEY), Utc::now()).unwrap()).unwrap();
        let b = ws.answering().expect("the response cache is bound");
        assert_eq!(
            (b.plugin.as_str(), b.mode, b.id.as_str()),
            ("response_cache", PluginMode::Enforce, "platform:response_cache")
        );
        assert_eq!(b.settings["calls"], "deterministic");
        assert!(b.bound.is_ok(), "{:?}", b.bound.as_ref().err());

        let mut pairs = example();
        pairs.extend([("PGW_CACHE_MODE", "shadow"), ("PGW_CACHE_CALLS", "all"), ("PGW_CACHE_TTL_SECONDS", "60")]);
        let (state, pepper) = holder(&pairs);
        let snap = state.snapshot();
        let ws = snap.workspace_of(snap.key(&pepper.hash(KEY), Utc::now()).unwrap()).unwrap();
        let b = ws.answering().unwrap();
        assert_eq!(
            (b.mode, b.settings["calls"].as_str(), b.settings["ttl_seconds"].as_u64()),
            (PluginMode::Shadow, Some("all"), Some(60))
        );

        let mut pairs = example();
        pairs.push(("PGW_CACHE_MODE", "off"));
        let (state, pepper) = holder(&pairs);
        let snap = state.snapshot();
        let ws = snap.workspace_of(snap.key(&pepper.hash(KEY), Utc::now()).unwrap()).unwrap();
        assert!(ws.answering().is_none());
    }

    #[test]
    fn the_credentials_are_sealed_and_scoped_to_the_organisation() {
        let cfg = read(&Vars::from_pairs(example())).unwrap();
        let kek = Kek::new("test", [9u8; 32]);
        let pepper = Pepper::new(b"0123456789abcdef-test".to_vec()).unwrap();
        let rows = rows(&cfg, &kek, &pepper).unwrap();
        for r in rows.iter().filter(|r| r.kind == Credential::KIND) {
            let text = r.doc.to_string();
            assert!(!text.contains("sk-test") && !text.contains("sk-ant-test"), "{text}");
            let c: Credential = serde_json::from_value(r.doc.clone()).unwrap();
            assert_eq!(c.scope, CredentialScope::Organisation(ORG_ID.into()));
        }
        assert!(rows.iter().all(|r| !r.doc.to_string().contains(KEY)), "no client key in any row");
    }
}
