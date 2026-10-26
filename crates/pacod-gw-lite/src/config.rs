//! The configuration of the minimal gateway, read from variables (docs/lite.md, section 3).
//!
//! Objects are named `PGW_<KIND>_<LABEL>[_<ATTRIBUTE>]`: kinds `PROVIDER`, `MODEL`, `POOL` and
//! `CLIENT`, labels of capital letters and digits only. Because a label has no underscore,
//! whatever follows the first underscore after it is the attribute, and a misspelt attribute
//! is an error instead of a new object. Empty values count as unset.
//!
//! Every problem is collected before the start is refused, so that one run lists them all.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::str::FromStr;

use pacod_gw_core::crypto::keys::{API_KEY_PREFIX, id_of};
use pacod_gw_core::model::repo::{capability_presets, is_model_name, suggested_preset, suggestion};
use pacod_gw_core::model::{
    Algorithm, AuditMode, AuthScheme, Family, ModelKind, Platform, ProviderPreset, Rates, UnsupportedParams, is_slug,
    provider_presets,
};

use pacod_gw_core::model::{OnError, PluginMode};
use pacod_gw_plugin_cache::settings::{Calls, MAX_TTL_SECONDS};
use pacod_gw_plugin_intent::Catalogue;
use pacod_gw_plugin_intent::settings::EngineChoice;

use crate::env::Vars;

/// A secret from the configuration. Never shown by `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// `json` or `text`, as every pacod-gw binary reads it (pacod-demo/docs/LOGGING.md).
pub use pacod_gw_logging::Format as LogFormat;

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub log_format: LogFormat,
    /// When request and response bodies are logged as well.
    pub log_bodies: AuditMode,
    pub log_body_max_bytes: u64,
    pub unsupported_params: UnsupportedParams,
    /// The currency of prices that name none.
    pub currency: String,
    pub max_body_bytes: usize,
    pub max_in_flight: usize,
    pub stream_idle_seconds: u64,
    pub drain_seconds: u64,
    pub drain_delay_seconds: u64,
    pub egress_allow_private: Vec<String>,
    pub trusted_proxies: Vec<String>,
    /// Where Vertex AI access tokens are minted; for tests against a mock only.
    pub google_token_url: Option<String>,
    pub providers: Vec<ProviderCfg>,
    pub models: Vec<ModelCfg>,
    pub pools: Vec<PoolCfg>,
    pub clients: Vec<ClientCfg>,
    /// The intent plugin, when `PGW_INTENT_MODE` switches it on.
    pub intent: Option<IntentCfg>,
    /// The response cache, unless `PGW_CACHE_MODE` switches it off.
    pub cache: Option<CacheCfg>,
    /// The most the response cache holds, in bytes.
    pub cache_memory_bytes: usize,
    /// Where the record of files and batches is kept. Without it, their endpoints answer 501.
    pub objects_dir: Option<std::path::PathBuf>,
    /// Accepted, but probably not meant.
    pub warnings: Vec<String>,
}

/// The intent plugin (docs/lite.md, section 12): one binding for all clients.
#[derive(Debug, Clone)]
pub struct IntentCfg {
    pub mode: PluginMode,
    pub engine: EngineChoice,
    /// From `PGW_INTENT_FILE`.
    pub catalogue: Catalogue,
    /// The LLM engine's model or `@pool`.
    pub model: Option<String>,
    /// The Jev engine's decision model.
    pub jev_model: Option<String>,
    pub on_error: OnError,
    pub timeout_ms: Option<u32>,
    pub expose: bool,
}

/// The response cache (docs/lite.md, section 13): on by default, for every client, in this
/// instance's memory.
#[derive(Debug, Clone, PartialEq)]
pub struct CacheCfg {
    pub mode: PluginMode,
    pub calls: Calls,
    pub ttl_seconds: u32,
    pub max_answer_bytes: u32,
}

#[derive(Debug, Clone)]
pub struct ProviderCfg {
    /// The label of its variables, `OPENAI` in `PGW_PROVIDER_OPENAI_KEY`.
    pub label: String,
    /// The name in model names and proxy paths: `openai`.
    pub name: String,
    pub preset: ProviderPreset,
    pub base_url: String,
    /// None for a provider that takes no credential (a local Ollama).
    pub key: Option<Secret>,
    pub timeout_seconds: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PriceCfg {
    /// No price: calls are logged without a cost.
    None,
    Free,
    Rates {
        // Boxed: sixteen rates make it far larger than the other variants.
        rates: Box<Rates>,
        currency: String,
    },
}

#[derive(Debug, Clone)]
pub struct ModelCfg {
    pub label: String,
    /// What clients send as `model`.
    pub name: String,
    /// The provider's name.
    pub provider: String,
    /// The model's id at the provider.
    pub upstream: String,
    pub kind: ModelKind,
    /// The capability preset.
    pub capabilities: &'static str,
    pub context: Option<u32>,
    pub max_output: Option<u32>,
    pub price: PriceCfg,
}

#[derive(Debug, Clone)]
pub struct PoolCfg {
    pub label: String,
    /// Called as `@name`.
    pub name: String,
    /// Model names, in priority order.
    pub models: Vec<String>,
    pub algorithm: Algorithm,
    /// One per model.
    pub weights: Vec<u32>,
    pub timeout_seconds: Option<u32>,
    pub retries: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct ClientCfg {
    pub label: String,
    /// In every log line of the client's calls.
    pub name: String,
    pub key: Secret,
    pub rpm: Option<u32>,
    pub tpm: Option<u64>,
    pub concurrency: Option<u32>,
    /// What the client may call, when restricted: model names and pool names (without `@`).
    pub allowed: Option<Allowed>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Allowed {
    pub models: Vec<String>,
    pub pools: Vec<String>,
}

/// Everything wrong with a configuration.
#[derive(Debug)]
pub struct Errors(pub Vec<String>);

impl std::fmt::Display for Errors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "the configuration has {} problem(s):", self.0.len())?;
        for e in &self.0 {
            writeln!(f, "  - {e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Errors {}

const PLAIN: &[&str] = &[
    "PGW_LISTEN",
    "PGW_LOG_FORMAT",
    "PGW_LOG_BODIES",
    "PGW_LOG_BODY_MAX_BYTES",
    "PGW_UNSUPPORTED_PARAMS",
    "PGW_CURRENCY",
    "PGW_TIMEOUT_SECONDS",
    "PGW_MAX_BODY_BYTES",
    "PGW_MAX_IN_FLIGHT",
    "PGW_STREAM_IDLE_SECONDS",
    "PGW_DRAIN_SECONDS",
    "PGW_DRAIN_DELAY_SECONDS",
    "PGW_EGRESS_ALLOW_PRIVATE",
    "PGW_TRUSTED_PROXIES",
    "PGW_GOOGLE_TOKEN_URL",
    "PGW_INTENT_MODE",
    "PGW_INTENT_FILE",
    "PGW_INTENT_ENGINE",
    "PGW_INTENT_MODEL",
    "PGW_INTENT_JEV_MODEL",
    "PGW_INTENT_ON_ERROR",
    "PGW_INTENT_TIMEOUT_MS",
    "PGW_INTENT_EXPOSE",
    "PGW_CACHE_MODE",
    "PGW_CACHE_CALLS",
    "PGW_CACHE_TTL_SECONDS",
    "PGW_CACHE_MAX_ANSWER_BYTES",
    "PGW_CACHE_MEMORY_MB",
    "PGW_OBJECTS_DIR",
];

/// Read by Compose for the image and the published port, not by the gateway.
fn compose_only(name: &str) -> bool {
    name.starts_with("PGW_LITE_") || name == "PGW_VERSION" || name == "PGW_REGISTRY"
}

/// Read by the binary itself (`--env-file`), not part of the configuration.
const OWN: &[&str] = &["PGW_ENV_FILE"];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    Provider,
    Model,
    Pool,
    Client,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::Provider, Kind::Model, Kind::Pool, Kind::Client];

    fn prefix(self) -> &'static str {
        match self {
            Kind::Provider => "PGW_PROVIDER_",
            Kind::Model => "PGW_MODEL_",
            Kind::Pool => "PGW_POOL_",
            Kind::Client => "PGW_CLIENT_",
        }
    }

    /// The attributes; `""` is the variable without one.
    fn attributes(self) -> &'static [&'static str] {
        match self {
            Kind::Provider => &["KEY", "KEY_FILE", "BASE_URL", "PRESET", "NAME", "TIMEOUT_SECONDS"],
            Kind::Model => {
                &["", "PROVIDER", "UPSTREAM", "KIND", "CAPABILITIES", "CONTEXT", "MAX_OUTPUT", "PRICE", "CURRENCY"]
            }
            Kind::Pool => &["", "NAME", "ALGORITHM", "WEIGHTS", "TIMEOUT_SECONDS", "RETRIES"],
            Kind::Client => &["KEY", "KEY_FILE", "RPM", "TPM", "CONCURRENCY", "MODELS"],
        }
    }
}

/// The variables of one object, by attribute.
#[derive(Default)]
struct Object(BTreeMap<&'static str, String>);

impl Object {
    fn get(&self, attr: &str) -> Option<&str> {
        self.0.get(attr).map(String::as_str)
    }
}

fn var(kind: Kind, label: &str, attr: &str) -> String {
    if attr.is_empty() { format!("{}{label}", kind.prefix()) } else { format!("{}{label}_{attr}", kind.prefix()) }
}

struct Reader<'a> {
    errors: Vec<String>,
    warnings: Vec<String>,
    plain: BTreeMap<&'a str, &'a str>,
    objects: BTreeMap<Kind, BTreeMap<String, Object>>,
}

impl<'a> Reader<'a> {
    fn sort(vars: &'a Vars) -> Self {
        let mut r =
            Reader { errors: Vec::new(), warnings: Vec::new(), plain: BTreeMap::new(), objects: BTreeMap::new() };
        for (name, value) in vars.iter() {
            if !name.starts_with("PGW_") || value.trim().is_empty() || compose_only(name) || OWN.contains(&name) {
                continue;
            }
            if PLAIN.contains(&name) {
                r.plain.insert(name, value.trim());
                continue;
            }
            let Some((kind, rest)) = Kind::ALL.into_iter().find_map(|k| name.strip_prefix(k.prefix()).map(|r| (k, r)))
            else {
                r.warnings.push(format!("{name} is not a setting of the minimal gateway and is ignored"));
                continue;
            };
            let (label, attr) = rest.split_once('_').unwrap_or((rest, ""));
            if label.is_empty() || !label.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
                r.errors.push(format!("{name}: the label after {} must be capital letters and digits", kind.prefix()));
                continue;
            }
            let Some(attr) = kind.attributes().iter().find(|a| **a == attr) else {
                let known: Vec<String> =
                    kind.attributes().iter().filter(|a| !a.is_empty()).map(|a| format!("_{a}")).collect();
                r.errors.push(if attr.is_empty() {
                    format!("{name} takes no value of its own: use one of {}", known.join(", "))
                } else {
                    format!("{name}: {kind:?} has no attribute _{attr}; known: {}", known.join(", "))
                });
                continue;
            };
            let value = if *attr == "KEY_FILE" {
                // The file's content, as if given in place. A trailing line break is not part
                // of a key.
                match std::fs::read_to_string(value.trim()) {
                    Ok(text) => text.trim_end().to_owned(),
                    Err(e) => {
                        r.errors.push(format!("{name}: reading {}: {e}", value.trim()));
                        continue;
                    }
                }
            } else {
                value.trim().to_owned()
            };
            r.objects.entry(kind).or_default().entry(label.to_owned()).or_default().0.insert(attr, value);
        }
        r
    }

    fn take(&mut self, kind: Kind) -> BTreeMap<String, Object> {
        self.objects.remove(&kind).unwrap_or_default()
    }

    fn number<T: FromStr + PartialOrd + std::fmt::Display + Copy>(
        &mut self,
        name: &str,
        value: Option<&str>,
        min: T,
        max: T,
    ) -> Option<T> {
        let v = value?;
        match v.parse::<T>() {
            Ok(n) if n >= min && n <= max => Some(n),
            _ => {
                self.errors.push(format!("{name}: \"{v}\" is not a whole number from {min} to {max}"));
                None
            }
        }
    }

    fn plain_number<T: FromStr + PartialOrd + std::fmt::Display + Copy>(
        &mut self,
        name: &str,
        default: T,
        min: T,
        max: T,
    ) -> T {
        let value = self.plain.get(name).copied();
        self.number(name, value, min, max).unwrap_or(default)
    }

    /// A value from a fixed list, named as in the JSON of the full gateway.
    fn choice<T: serde::de::DeserializeOwned>(&mut self, name: &str, value: &str, allowed: &str) -> Option<T> {
        match serde_json::from_value(serde_json::Value::String(value.to_owned())) {
            Ok(v) => Some(v),
            Err(_) => {
                self.errors.push(format!("{name}: \"{value}\" is not one of {allowed}"));
                None
            }
        }
    }
}

fn list(value: Option<&str>) -> Vec<String> {
    value.map(|v| v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

/// Reads the configuration. Problems in the variables are all reported together.
pub fn read(vars: &Vars) -> Result<Config, Errors> {
    let mut r = Reader::sort(vars);

    let listen = match r.plain.get("PGW_LISTEN").copied() {
        Some(v) => v.parse().unwrap_or_else(|_| {
            r.errors.push(format!("PGW_LISTEN: \"{v}\" is not an address like 0.0.0.0:8080"));
            ([0, 0, 0, 0], 8080).into()
        }),
        None => ([0, 0, 0, 0], 8080).into(),
    };
    // The logging convention of every pacod product: json by default, any case, and an unknown
    // value falls back to the default rather than stopping the gateway.
    let log_format = match r.plain.get("PGW_LOG_FORMAT").copied() {
        None => LogFormat::Json,
        Some(v) => LogFormat::parse(v).unwrap_or_else(|| {
            r.warnings.push(format!("PGW_LOG_FORMAT: \"{v}\" is not one of json, text; json is used"));
            LogFormat::Json
        }),
    };
    let log_bodies = match r.plain.get("PGW_LOG_BODIES").copied() {
        Some(v) => r.choice("PGW_LOG_BODIES", v, "off, on, on_request").unwrap_or_default(),
        None => AuditMode::Off,
    };
    let log_body_max_bytes = r.plain_number("PGW_LOG_BODY_MAX_BYTES", 16 << 10, 256, 8 << 20);
    let unsupported_params = match r.plain.get("PGW_UNSUPPORTED_PARAMS").copied() {
        Some(v) => r.choice("PGW_UNSUPPORTED_PARAMS", v, "drop, reject").unwrap_or_default(),
        None => UnsupportedParams::Drop,
    };
    let currency = r.plain.get("PGW_CURRENCY").map_or("USD", |v| v).to_owned();
    if !is_currency(&currency) {
        r.errors.push(format!("PGW_CURRENCY: \"{currency}\" is not a currency code like USD"));
    }
    let timeout = r.plain_number("PGW_TIMEOUT_SECONDS", 180u32, 1, 600);
    let max_body_bytes = r.plain_number("PGW_MAX_BODY_BYTES", 32usize << 20, 1 << 10, 1 << 30);
    let max_in_flight = r.plain_number("PGW_MAX_IN_FLIGHT", 10_000usize, 1, 1_000_000);
    let stream_idle_seconds = r.plain_number("PGW_STREAM_IDLE_SECONDS", 120u64, 1, 3600);
    let drain_seconds = r.plain_number("PGW_DRAIN_SECONDS", 30u64, 0, 3600);
    let drain_delay_seconds = r.plain_number("PGW_DRAIN_DELAY_SECONDS", 0u64, 0, 300);
    let egress_allow_private = list(r.plain.get("PGW_EGRESS_ALLOW_PRIVATE").copied());
    let trusted_proxies = list(r.plain.get("PGW_TRUSTED_PROXIES").copied());
    let google_token_url = r.plain.get("PGW_GOOGLE_TOKEN_URL").map(|v| (*v).to_owned());

    let providers = providers(&mut r, timeout);
    let models = models(&mut r, &providers, &currency);
    let pools = pools(&mut r, &models);
    let clients = clients(&mut r, &models, &pools);
    let intent = intent(&mut r, &models, &pools);
    let cache = cache(&mut r);
    let cache_memory_bytes = r.plain_number("PGW_CACHE_MEMORY_MB", 64usize, 1, 64 << 10) << 20;
    let objects_dir =
        r.plain.get("PGW_OBJECTS_DIR").filter(|d| !d.trim().is_empty()).map(|d| std::path::PathBuf::from(d.trim()));
    if objects_dir.as_ref().is_some_and(|d| d.is_relative()) {
        r.errors.push("PGW_OBJECTS_DIR: name the directory by its absolute path".into());
    }

    if providers.is_empty() {
        r.errors.push("no provider: set at least one PGW_PROVIDER_<LABEL>_KEY".into());
    }
    if clients.is_empty() {
        r.errors
            .push("no client: set at least one PGW_CLIENT_<LABEL>_KEY (make one with `pacod-gw-lite keygen`)".into());
    }
    if !providers.is_empty() && models.is_empty() {
        r.warnings.push(
            "no model is configured: only proxy-mode paths that name no model (model lists, files) can be used".into(),
        );
    }

    if !r.errors.is_empty() {
        return Err(Errors(r.errors));
    }
    Ok(Config {
        listen,
        log_format,
        log_bodies,
        log_body_max_bytes,
        unsupported_params,
        currency,
        max_body_bytes,
        max_in_flight,
        stream_idle_seconds,
        drain_seconds,
        drain_delay_seconds,
        egress_allow_private,
        trusted_proxies,
        google_token_url,
        providers,
        models,
        pools,
        clients,
        intent,
        cache,
        cache_memory_bytes,
        objects_dir,
        warnings: r.warnings,
    })
}

/// The response cache's variables. On (`enforce`) unless `PGW_CACHE_MODE` says otherwise.
fn cache(r: &mut Reader<'_>) -> Option<CacheCfg> {
    let get = |r: &Reader<'_>, name: &str| r.plain.get(name).map(|v| (*v).to_owned());
    let mode = match get(r, "PGW_CACHE_MODE").as_deref() {
        None => PluginMode::Enforce,
        Some(v) => r.choice("PGW_CACHE_MODE", v, "off, shadow, enforce").unwrap_or(PluginMode::Enforce),
    };
    let defaults = pacod_gw_plugin_cache::Settings::default();
    let calls = match get(r, "PGW_CACHE_CALLS").as_deref() {
        None => defaults.calls,
        Some(v) => r.choice("PGW_CACHE_CALLS", v, "deterministic, all").unwrap_or_default(),
    };
    let ttl_seconds = r.number("PGW_CACHE_TTL_SECONDS", get(r, "PGW_CACHE_TTL_SECONDS").as_deref(), 1, MAX_TTL_SECONDS);
    let max_answer_bytes =
        r.number("PGW_CACHE_MAX_ANSWER_BYTES", get(r, "PGW_CACHE_MAX_ANSWER_BYTES").as_deref(), 1_024, 16 << 20);
    if mode == PluginMode::Off {
        if r.plain
            .keys()
            .any(|k| k.starts_with("PGW_CACHE_") && !matches!(*k, "PGW_CACHE_MODE" | "PGW_CACHE_MEMORY_MB"))
        {
            r.warnings.push("PGW_CACHE_* is set, but PGW_CACHE_MODE is off: the response cache is off".into());
        }
        return None;
    }
    Some(CacheCfg {
        mode,
        calls,
        ttl_seconds: ttl_seconds.unwrap_or(defaults.ttl_seconds),
        max_answer_bytes: max_answer_bytes.unwrap_or(defaults.max_answer_bytes),
    })
}

/// The intent plugin's variables. Off unless `PGW_INTENT_MODE` says `shadow` or `enforce`.
fn intent(r: &mut Reader<'_>, models: &[ModelCfg], pools: &[PoolCfg]) -> Option<IntentCfg> {
    let get = |r: &Reader<'_>, name: &str| r.plain.get(name).map(|v| (*v).to_owned());
    let mode = match get(r, "PGW_INTENT_MODE").as_deref() {
        None => PluginMode::Off,
        Some(v) => r.choice("PGW_INTENT_MODE", v, "off, shadow, enforce").unwrap_or(PluginMode::Off),
    };
    if mode == PluginMode::Off {
        if r.plain.keys().any(|k| k.starts_with("PGW_INTENT_") && *k != "PGW_INTENT_MODE") {
            r.warnings
                .push("PGW_INTENT_* is set, but PGW_INTENT_MODE is not shadow or enforce: the plugin is off".into());
        }
        return None;
    }
    let engine = match get(r, "PGW_INTENT_ENGINE").as_deref() {
        None => EngineChoice::Llm,
        Some(v) => r.choice("PGW_INTENT_ENGINE", v, "llm, jev, cascade").unwrap_or_default(),
    };
    let on_error = match get(r, "PGW_INTENT_ON_ERROR").as_deref() {
        None => OnError::Closed,
        Some(v) => r.choice("PGW_INTENT_ON_ERROR", v, "closed, open").unwrap_or_default(),
    };
    let timeout_ms = r.number("PGW_INTENT_TIMEOUT_MS", get(r, "PGW_INTENT_TIMEOUT_MS").as_deref(), 50u32, 60_000);
    let expose = match get(r, "PGW_INTENT_EXPOSE").as_deref() {
        None | Some("false") => false,
        Some("true") => true,
        Some(v) => {
            r.errors.push(format!("PGW_INTENT_EXPOSE: \"{v}\" is not true or false"));
            false
        }
    };

    let catalogue = match get(r, "PGW_INTENT_FILE") {
        None => {
            r.errors.push("PGW_INTENT_FILE is not set: it names the file of the intent catalogue".into());
            None
        }
        Some(path) => match std::fs::read_to_string(&path) {
            Err(e) => {
                r.errors.push(format!("PGW_INTENT_FILE: reading {path}: {e}"));
                None
            }
            Ok(text) => {
                let parsed: Result<Catalogue, String> = if path.ends_with(".json") {
                    serde_json::from_str(&text).map_err(|e| e.to_string())
                } else {
                    // Strict booleans: `alert: no` is an error, not false.
                    let options = serde_saphyr::options! { strict_booleans: true };
                    serde_saphyr::from_str_with_options(&text, options).map_err(|e| e.to_string())
                };
                match parsed {
                    Err(e) => {
                        r.errors.push(format!("PGW_INTENT_FILE: {path} is not an intent catalogue: {e}"));
                        None
                    }
                    Ok(c) => {
                        for p in c.problems() {
                            r.errors.push(format!("PGW_INTENT_FILE: {p}"));
                        }
                        Some(c)
                    }
                }
            }
        },
    };

    let uses_llm = matches!(engine, EngineChoice::Llm | EngineChoice::Cascade);
    let uses_jev = matches!(engine, EngineChoice::Jev | EngineChoice::Cascade);
    let model = get(r, "PGW_INTENT_MODEL");
    match (&model, uses_llm) {
        (None, true) => r.errors.push(format!(
            "PGW_INTENT_MODEL is not set: the engine {} classifies with a model or @pool",
            engine.name()
        )),
        (Some(m), true) => {
            let known = match m.strip_prefix('@') {
                Some(pool) => pools.iter().any(|p| p.name == pool),
                None => models.iter().any(|x| x.name == *m && x.kind == ModelKind::Chat),
            };
            if !known {
                r.errors.push(format!("PGW_INTENT_MODEL: \"{m}\" is not a chat model or @pool of this configuration"));
            }
        }
        (Some(_), false) => r.warnings.push("PGW_INTENT_MODEL is set, but the engine jev does not use it".into()),
        (None, false) => {}
    }
    let decision: Vec<&ModelCfg> = models.iter().filter(|m| m.kind == ModelKind::Decision).collect();
    let jev_model = match get(r, "PGW_INTENT_JEV_MODEL") {
        Some(m) if uses_jev && !decision.iter().any(|x| x.name == m) => {
            r.errors.push(format!("PGW_INTENT_JEV_MODEL: \"{m}\" is not a decision model of this configuration"));
            None
        }
        Some(m) => Some(m),
        // The one decision model there is.
        None if uses_jev => match decision.as_slice() {
            [one] => Some(one.name.clone()),
            _ => {
                r.errors.push(format!(
                    "PGW_INTENT_JEV_MODEL is not set: the engine {} needs a decision model, for example \
                     PGW_MODEL_JEV=typesafe/jev-1.13.0 with PGW_PROVIDER_TYPESAFE_KEY",
                    engine.name()
                ));
                None
            }
        },
        None => None,
    };
    Some(IntentCfg { mode, engine, catalogue: catalogue?, model, jev_model, on_error, timeout_ms, expose })
}

fn is_currency(s: &str) -> bool {
    s.len() == 3 && s.bytes().all(|b| b.is_ascii_uppercase())
}

fn providers(r: &mut Reader, default_timeout: u32) -> Vec<ProviderCfg> {
    let presets = provider_presets();
    let mut out: Vec<ProviderCfg> = Vec::new();
    for (label, o) in r.take(Kind::Provider) {
        let v = |attr: &str| var(Kind::Provider, &label, attr);
        let name = o.get("NAME").map_or_else(|| label.to_ascii_lowercase(), str::to_owned);
        if !is_slug(&name) {
            r.errors.push(format!("{}: \"{name}\" is not a name of lower-case letters, digits and '-'", v("NAME")));
            continue;
        }
        if out.iter().any(|p| p.name == name) {
            r.errors.push(format!("{}: another provider is already named \"{name}\"", v("NAME")));
            continue;
        }
        let preset_name = o.get("PRESET").unwrap_or(&name);
        let Some(preset) = presets.iter().find(|p| p.name == preset_name) else {
            let names: Vec<&str> = presets.iter().map(|p| p.name).collect();
            r.errors.push(if o.get("PRESET").is_some() {
                format!("{}: no preset is named \"{preset_name}\"; presets: {}", v("PRESET"), names.join(", "))
            } else {
                format!("{}: \"{name}\" is not a preset, so set {} to one of: {}", v(""), v("PRESET"), names.join(", "))
            });
            continue;
        };
        if o.get("KEY").is_some() && o.get("KEY_FILE").is_some() {
            r.errors.push(format!("{} and {} are both set: keep one", v("KEY"), v("KEY_FILE")));
            continue;
        }
        let key = o.get("KEY").or(o.get("KEY_FILE")).map(|k| Secret(k.to_owned()));
        let needs_key = preset.auth.unwrap_or(preset.family.default_auth()) != AuthScheme::None;
        if needs_key && key.is_none() {
            r.errors.push(format!("{}: provider \"{name}\" needs a key", v("KEY")));
            continue;
        }
        let base_url = o.get("BASE_URL").unwrap_or(preset.base_url).trim_end_matches('/').to_owned();
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            r.errors.push(format!("{}: \"{base_url}\" is not an http(s) URL", v("BASE_URL")));
            continue;
        }
        if preset.platform == Platform::Vertex && base_url.contains("YOUR-PROJECT") {
            r.errors.push(format!(
                "{}: name your Google Cloud project and location in the base URL (see the vertex preset)",
                v("BASE_URL")
            ));
            continue;
        }
        let timeout_seconds =
            r.number(&v("TIMEOUT_SECONDS"), o.get("TIMEOUT_SECONDS"), 1, 600).unwrap_or(default_timeout);
        out.push(ProviderCfg { label, name, preset: preset.clone(), base_url, key, timeout_seconds });
    }
    out
}

const RATE_NAMES: &[&str] = &[
    "input",
    "output",
    "cache_read",
    "cache_write_5m",
    "cache_write_1h",
    "reasoning",
    "per_character",
    "per_request",
    "per_server_tool_call",
    "input_audio",
    "output_audio",
    "input_image",
    "output_image",
    "per_image",
    "per_audio_second",
    "per_search_unit",
];

fn price(r: &mut Reader, name: &str, value: Option<&str>, currency: String) -> PriceCfg {
    let Some(value) = value else { return PriceCfg::None };
    if value.eq_ignore_ascii_case("free") {
        return PriceCfg::Free;
    }
    let mut rates = serde_json::Map::new();
    for pair in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let parsed = pair.split_once('=').map(|(k, v)| (k.trim(), v.trim().parse::<f64>()));
        match parsed {
            Some((k, Ok(n))) if RATE_NAMES.contains(&k) && n.is_finite() && n >= 0.0 => {
                rates.insert(k.to_owned(), serde_json::json!(n));
            }
            Some((k, _)) if !RATE_NAMES.contains(&k) => {
                r.errors.push(format!("{name}: \"{k}\" is not a rate; rates: {}", RATE_NAMES.join(", ")));
                return PriceCfg::None;
            }
            _ => {
                r.errors.push(format!("{name}: \"{pair}\" is not rate=number (per million tokens), or \"free\""));
                return PriceCfg::None;
            }
        }
    }
    if rates.is_empty() {
        r.errors.push(format!("{name}: no rate given"));
        return PriceCfg::None;
    }
    match serde_json::from_value(serde_json::Value::Object(rates)) {
        Ok(rates) => PriceCfg::Rates { rates: Box::new(rates), currency },
        Err(e) => {
            r.errors.push(format!("{name}: {e}"));
            PriceCfg::None
        }
    }
}

fn models(r: &mut Reader, providers: &[ProviderCfg], default_currency: &str) -> Vec<ModelCfg> {
    let presets = capability_presets();
    let mut out: Vec<ModelCfg> = Vec::new();
    for (label, o) in r.take(Kind::Model) {
        let v = |attr: &str| var(Kind::Model, &label, attr);
        let Some(name) = o.get("") else {
            r.errors.push(format!("{} is not set: it holds the model name clients use", v("")));
            continue;
        };
        if !is_model_name(name) {
            r.errors.push(format!("{}: \"{name}\" is not a usable model name", v("")));
            continue;
        }
        if out.iter().any(|m| m.name == name) {
            r.errors.push(format!("{}: the model name \"{name}\" is used twice", v("")));
            continue;
        }
        let names: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
        let prefix = name.split_once('/').filter(|(p, _)| names.contains(p));
        let provider = match (o.get("PROVIDER"), prefix) {
            (Some(p), _) if names.contains(&p) => p,
            (Some(p), _) => {
                r.errors.push(format!(
                    "{}: no provider is named \"{p}\"; providers: {}",
                    v("PROVIDER"),
                    names.join(", ")
                ));
                continue;
            }
            (None, Some((p, _))) => p,
            (None, None) => {
                r.errors.push(format!(
                    "{}: \"{name}\" does not start with a provider name; set {} (providers: {})",
                    v(""),
                    v("PROVIDER"),
                    names.join(", ")
                ));
                continue;
            }
        };
        let Some(p) = providers.iter().find(|x| x.name == provider) else { continue };
        let upstream = match o.get("UPSTREAM") {
            Some(u) => u.to_owned(),
            None => name.strip_prefix(provider).and_then(|s| s.strip_prefix('/')).unwrap_or(name).to_owned(),
        };
        let (guessed_kind, mut guessed_preset) = suggestion(p.preset.family, &upstream, None, None, None);
        if guessed_kind == ModelKind::Chat
            && p.preset.family == Family::Anthropic
            && let Some(preset) = anthropic_preset(&upstream)
        {
            guessed_preset = preset;
        }
        let kind = match o.get("KIND") {
            Some(k) => match r.choice::<ModelKind>(
                &v("KIND"),
                k,
                "chat, embedding, speech, decision, image, transcription, rerank, realtime",
            ) {
                Some(k) => k,
                None => continue,
            },
            None => guessed_kind,
        };
        let capabilities = match o.get("CAPABILITIES") {
            Some(c) => match presets.iter().find(|x| x.name == c) {
                Some(x) => x.name,
                None => {
                    let all: Vec<&str> = presets.iter().map(|x| x.name).collect();
                    r.errors.push(format!(
                        "{}: no preset is named \"{c}\"; presets: {}",
                        v("CAPABILITIES"),
                        all.join(", ")
                    ));
                    continue;
                }
            },
            None => match kind {
                _ if kind == guessed_kind => guessed_preset,
                ModelKind::Chat => suggested_preset(p.preset.family, &upstream, None, None),
                kind => kind.preset(),
            },
        };
        let context = r.number(&v("CONTEXT"), o.get("CONTEXT"), 1, u32::MAX);
        let max_output = r.number(&v("MAX_OUTPUT"), o.get("MAX_OUTPUT"), 1, u32::MAX);
        let currency = o.get("CURRENCY").unwrap_or(default_currency).to_owned();
        if !is_currency(&currency) {
            r.errors.push(format!("{}: \"{currency}\" is not a currency code like USD", v("CURRENCY")));
        }
        let price = price(r, &v("PRICE"), o.get("PRICE"), currency);
        out.push(ModelCfg {
            label,
            name: name.to_owned(),
            provider: provider.to_owned(),
            upstream,
            kind,
            capabilities,
            context,
            max_output,
            price,
        });
    }
    out
}

/// The capability preset of a Claude model, by its generation, as the presets describe them:
/// thinking with a token budget up to Haiku 4.5, Sonnet 4.x and Opus 4.6; adaptive thinking
/// from Opus 4.7 and Sonnet 5; Sonnet 5.5's own; always thinking for Opus 5.5 and Fable 5.1.
/// `None` for an id it cannot read. The full gateway's suggestion knows only the adaptive one.
fn anthropic_preset(upstream: &str) -> Option<&'static str> {
    let id = upstream.to_ascii_lowercase();
    if id.starts_with("claude-3") {
        return Some("anthropic-budget");
    }
    let (family, rest) = ["opus", "sonnet", "haiku", "fable"].into_iter().find_map(|f| {
        id.strip_prefix("claude-").and_then(|r| r.strip_prefix(f)).and_then(|r| r.strip_prefix('-')).map(|r| (f, r))
    })?;
    let mut parts = rest.split('-');
    let major: u32 = parts.next()?.parse().ok()?;
    // A second number of one or two digits is the minor version; eight digits are a date.
    let minor: u32 = parts.next().filter(|p| p.len() <= 2).and_then(|p| p.parse().ok()).unwrap_or(0);
    let v = major * 10 + minor;
    Some(match family {
        "haiku" | "sonnet" if v < 50 => "anthropic-budget",
        "opus" if v < 47 => "anthropic-budget",
        "sonnet" if v == 55 => "anthropic-sonnet-5-5",
        "opus" if v >= 55 => "anthropic-always-thinking",
        "fable" if v >= 51 => "anthropic-always-thinking",
        _ => "anthropic-adaptive",
    })
}

/// The most models a pool may hold; the full gateway tries at most this many in one call.
pub const MAX_POOL: usize = 8;

fn pools(r: &mut Reader, models: &[ModelCfg]) -> Vec<PoolCfg> {
    let mut out: Vec<PoolCfg> = Vec::new();
    for (label, o) in r.take(Kind::Pool) {
        let v = |attr: &str| var(Kind::Pool, &label, attr);
        let name = o.get("NAME").map_or_else(|| label.to_ascii_lowercase(), str::to_owned);
        if !is_slug(&name) {
            r.errors.push(format!("{}: \"{name}\" is not a name of lower-case letters, digits and '-'", v("NAME")));
            continue;
        }
        if out.iter().any(|p| p.name == name) {
            r.errors.push(format!("{}: another pool is already named \"{name}\"", v("NAME")));
            continue;
        }
        let members = list(o.get(""));
        if members.is_empty() {
            r.errors.push(format!("{} is not set: it lists the pool's models in order", v("")));
            continue;
        }
        if members.len() > MAX_POOL {
            r.errors.push(format!("{}: a pool holds at most {MAX_POOL} models", v("")));
            continue;
        }
        let unknown: Vec<&String> = members.iter().filter(|m| !models.iter().any(|x| &x.name == *m)).collect();
        if !unknown.is_empty() {
            r.errors.push(format!(
                "{}: no model is named {}",
                v(""),
                unknown.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", ")
            ));
            continue;
        }
        if members.iter().collect::<BTreeSet<_>>().len() != members.len() {
            r.errors.push(format!("{}: a model is listed twice", v("")));
            continue;
        }
        let algorithm = match o.get("ALGORITHM") {
            Some(a) => match r.choice::<Algorithm>(
                &v("ALGORITHM"),
                a,
                "priority, weighted, round_robin, weighted_round_robin, least_in_flight, lowest_latency, cheapest",
            ) {
                Some(a) => a,
                None => continue,
            },
            None => Algorithm::Priority,
        };
        let weights = match o.get("WEIGHTS") {
            Some(w) => {
                let parsed: Result<Vec<u32>, _> = list(Some(w)).iter().map(|n| n.parse::<u32>()).collect();
                match parsed {
                    Ok(ws) if ws.len() == members.len() && ws.iter().any(|n| *n > 0) => ws,
                    _ => {
                        r.errors.push(format!(
                            "{}: give one whole number per model ({}), at least one above 0",
                            v("WEIGHTS"),
                            members.len()
                        ));
                        continue;
                    }
                }
            }
            None => vec![1; members.len()],
        };
        let timeout_seconds = r.number(&v("TIMEOUT_SECONDS"), o.get("TIMEOUT_SECONDS"), 1, 600);
        let retries = r.number(&v("RETRIES"), o.get("RETRIES"), 1, 5);
        out.push(PoolCfg { label, name, models: members, algorithm, weights, timeout_seconds, retries });
    }
    out
}

/// The shortest client key accepted: `pgw_<name>_` and 16 characters of secret.
const MIN_KEY_LEN: usize = 24;

fn clients(r: &mut Reader, models: &[ModelCfg], pools: &[PoolCfg]) -> Vec<ClientCfg> {
    let mut out: Vec<ClientCfg> = Vec::new();
    for (label, o) in r.take(Kind::Client) {
        let v = |attr: &str| var(Kind::Client, &label, attr);
        let name = label.to_ascii_lowercase();
        if o.get("KEY").is_some() && o.get("KEY_FILE").is_some() {
            r.errors.push(format!("{} and {} are both set: keep one", v("KEY"), v("KEY_FILE")));
            continue;
        }
        let Some(key) = o.get("KEY").or(o.get("KEY_FILE")) else {
            r.errors.push(format!(
                "{}: client \"{name}\" needs a key; make one with `pacod-gw-lite keygen {name}`",
                v("KEY")
            ));
            continue;
        };
        let well_formed = id_of(key, API_KEY_PREFIX).is_some()
            && (MIN_KEY_LEN..=256).contains(&key.len())
            && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !well_formed {
            // The key itself is never repeated in a message.
            r.errors.push(format!(
                "{}: a client key looks like pgw_<name>_<secret>, {MIN_KEY_LEN} to 256 letters, digits, '_' and '-'; make one with `pacod-gw-lite keygen {name}`",
                v("KEY")
            ));
            continue;
        }
        if let Some(other) = out.iter().find(|c| c.key.0 == key) {
            r.errors.push(format!("{}: client \"{}\" has the same key", v("KEY"), other.name));
            continue;
        }
        let rpm = r.number(&v("RPM"), o.get("RPM"), 1, u32::MAX);
        let tpm = r.number(&v("TPM"), o.get("TPM"), 1, u64::MAX);
        let concurrency = r.number(&v("CONCURRENCY"), o.get("CONCURRENCY"), 1, u32::MAX);
        let allowed = match o.get("MODELS") {
            None => None,
            Some(m) => {
                let mut allowed = Allowed::default();
                for entry in list(Some(m)) {
                    match entry.strip_prefix('@') {
                        Some(pool) if pools.iter().any(|p| p.name == pool) => allowed.pools.push(pool.to_owned()),
                        None if models.iter().any(|x| x.name == entry) => allowed.models.push(entry),
                        _ => r.errors.push(format!("{}: no model or pool is named \"{entry}\"", v("MODELS"))),
                    }
                }
                Some(allowed)
            }
        };
        out.push(ClientCfg { label, name, key: Secret(key.to_owned()), rpm, tpm, concurrency, allowed });
    }
    out
}

/// Whether a variable is one the minimal gateway reads (or Compose reads for it). For checking
/// documentation and example files against the parser.
pub fn is_known(name: &str) -> bool {
    if PLAIN.contains(&name) || compose_only(name) || OWN.contains(&name) || name == "RUST_LOG" {
        return true;
    }
    Kind::ALL.into_iter().any(|k| {
        name.strip_prefix(k.prefix()).is_some_and(|rest| {
            let (label, attr) = rest.split_once('_').unwrap_or((rest, ""));
            !label.is_empty()
                && label.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                && k.attributes().contains(&attr)
        })
    })
}

/// A value's name as the full gateway's JSON writes it.
fn name_of<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

/// What `pacod-gw-lite check` prints: the configuration as read, without any secret.
pub fn summary(cfg: &Config) -> Vec<String> {
    let format = cfg.log_format.as_str();
    let mut out = vec![format!(
        "listen {}, log {format}, bodies {} (at most {} bytes), unsupported parameters {}",
        cfg.listen,
        name_of(&cfg.log_bodies),
        cfg.log_body_max_bytes,
        name_of(&cfg.unsupported_params)
    )];
    for p in &cfg.providers {
        let key = if p.key.is_some() { "key set" } else { "no key" };
        out.push(format!(
            "provider {}: preset {}, {}, {key}, timeout {} s",
            p.name, p.preset.name, p.base_url, p.timeout_seconds
        ));
    }
    for m in &cfg.models {
        let price = match &m.price {
            PriceCfg::None => "no price".to_owned(),
            PriceCfg::Free => "free".to_owned(),
            PriceCfg::Rates { rates, currency } => {
                let named: Vec<String> =
                    rates.named().into_iter().filter_map(|(n, v)| v.map(|v| format!("{n} {v}"))).collect();
                format!("{currency} per million: {}", named.join(", "))
            }
        };
        out.push(format!(
            "model {}: {} at {}, {}, capabilities {}, {price}",
            m.name,
            m.upstream,
            m.provider,
            name_of(&m.kind),
            m.capabilities
        ));
    }
    for p in &cfg.pools {
        let members: Vec<String> = p.models.iter().zip(&p.weights).map(|(m, w)| format!("{m} ({w})")).collect();
        out.push(format!("pool @{}: {}, {}", p.name, p.algorithm.name(), members.join(", ")));
    }
    for c in &cfg.clients {
        let mut terms = Vec::new();
        if let Some(n) = c.rpm {
            terms.push(format!("{n} requests/min"));
        }
        if let Some(n) = c.tpm {
            terms.push(format!("{n} tokens/min"));
        }
        if let Some(n) = c.concurrency {
            terms.push(format!("{n} at once"));
        }
        terms.push(match &c.allowed {
            None => "every model and pool".to_owned(),
            Some(a) => {
                let all: Vec<String> =
                    a.models.iter().cloned().chain(a.pools.iter().map(|p| format!("@{p}"))).collect();
                format!("only {}", all.join(", "))
            }
        });
        out.push(format!("client {}: {}", c.name, terms.join(", ")));
    }
    if let Some(i) = &cfg.intent {
        let models: Vec<&str> = i.model.iter().chain(&i.jev_model).map(String::as_str).collect();
        out.push(format!(
            "intent plugin: {}, engine {} ({}), catalogue {} ({} intents, {} rules), on error {}",
            i.mode.name(),
            i.engine.name(),
            models.join(", "),
            i.catalogue.name,
            i.catalogue.intents.len(),
            i.catalogue.rules.len(),
            name_of(&i.on_error)
        ));
    }
    for w in &cfg.warnings {
        out.push(format!("warning: {w}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_pairs(pairs: &[(&str, &str)]) -> Result<Config, Errors> {
        read(&Vars::from_pairs(pairs.iter().copied()))
    }

    const KEY: &str = "pgw_portal_0123456789abcdefghij";

    #[test]
    fn every_rate_of_the_core_can_be_set() {
        let core: Vec<&str> = pacod_gw_core::model::Rates::default().named().iter().map(|(n, _)| *n).collect();
        assert_eq!(RATE_NAMES, core.as_slice());
    }

    fn minimal() -> Vec<(&'static str, &'static str)> {
        vec![
            ("PGW_PROVIDER_ANTHROPIC_KEY", "sk-ant-test"),
            ("PGW_PROVIDER_OPENAI_KEY", "sk-test"),
            ("PGW_MODEL_SONNET", "anthropic/claude-sonnet-5-5"),
            ("PGW_MODEL_SONNET_PRICE", "input=3,output=15,cache_read=0.3"),
            ("PGW_MODEL_MINI", "openai/gpt-5.6-luna"),
            ("PGW_POOL_FAST", "openai/gpt-5.6-luna, anthropic/claude-sonnet-5-5"),
            ("PGW_CLIENT_PORTAL_KEY", KEY),
            ("PGW_CLIENT_PORTAL_RPM", "600"),
        ]
    }

    fn errors(pairs: &[(&str, &str)]) -> Vec<String> {
        read_pairs(pairs).unwrap_err().0
    }

    #[test]
    fn the_minimal_example_reads_with_defaults() {
        let c = read_pairs(&minimal()).unwrap();
        assert_eq!(c.listen, "0.0.0.0:8080".parse().unwrap());
        assert_eq!((c.log_format, c.log_bodies, c.log_body_max_bytes), (LogFormat::Json, AuditMode::Off, 16384));
        assert_eq!(c.currency, "USD");

        let names: Vec<&str> = c.providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["anthropic", "openai"]);
        let anthropic = &c.providers[0];
        assert_eq!((anthropic.base_url.as_str(), anthropic.timeout_seconds), ("https://api.anthropic.com", 180));

        let sonnet = c.models.iter().find(|m| m.label == "SONNET").unwrap();
        assert_eq!((sonnet.provider.as_str(), sonnet.upstream.as_str()), ("anthropic", "claude-sonnet-5-5"));
        assert_eq!((sonnet.kind, sonnet.capabilities), (ModelKind::Chat, "anthropic-sonnet-5-5"));
        let PriceCfg::Rates { rates, currency } = &sonnet.price else { panic!("{:?}", sonnet.price) };
        assert_eq!(
            (rates.input, rates.output, rates.cache_read, currency.as_str()),
            (Some(3.0), Some(15.0), Some(0.3), "USD")
        );
        let mini = c.models.iter().find(|m| m.label == "MINI").unwrap();
        assert_eq!((mini.capabilities, &mini.price), ("openai-gpt-5-6", &PriceCfg::None));

        let fast = &c.pools[0];
        assert_eq!((fast.name.as_str(), fast.models.len(), fast.algorithm), ("fast", 2, Algorithm::Priority));
        assert_eq!(fast.weights, [1, 1]);

        let portal = &c.clients[0];
        assert_eq!((portal.name.as_str(), portal.rpm, portal.allowed.as_ref()), ("portal", Some(600), None));
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }

    #[test]
    fn every_attribute_is_read() {
        let dir = std::env::temp_dir().join(format!("pgw-lite-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key_file = dir.join("client.key");
        std::fs::write(&key_file, format!("{KEY}\n")).unwrap();
        let key_file = key_file.to_str().unwrap().to_owned();
        let c = read_pairs(&[
            ("PGW_LISTEN", "127.0.0.1:9999"),
            ("PGW_LOG_FORMAT", "json"),
            ("PGW_LOG_BODIES", "on_request"),
            ("PGW_LOG_BODY_MAX_BYTES", "4096"),
            ("PGW_UNSUPPORTED_PARAMS", "reject"),
            ("PGW_CURRENCY", "EUR"),
            ("PGW_TIMEOUT_SECONDS", "60"),
            ("PGW_EGRESS_ALLOW_PRIVATE", "gw-mock, 10.0.0.0/8"),
            ("PGW_PROVIDER_LOCAL_PRESET", "ollama"),
            ("PGW_PROVIDER_LOCAL_BASE_URL", "http://ollama:11434/v1/"),
            ("PGW_PROVIDER_GOAI_NAME", "gemini-openai"),
            ("PGW_PROVIDER_GOAI_KEY", "g-key"),
            ("PGW_PROVIDER_GOAI_TIMEOUT_SECONDS", "30"),
            ("PGW_MODEL_LLAMA", "llama3"),
            ("PGW_MODEL_LLAMA_PROVIDER", "local"),
            ("PGW_MODEL_LLAMA_PRICE", "free"),
            ("PGW_MODEL_EMB", "gemini-openai/text-embedding-004"),
            ("PGW_MODEL_EMB_KIND", "embedding"),
            ("PGW_MODEL_TTS", "local/tts-1"),
            ("PGW_MODEL_TTS_UPSTREAM", "gpt-4o-mini-tts"),
            ("PGW_MODEL_TTS_CAPABILITIES", "speech"),
            ("PGW_MODEL_TTS_CONTEXT", "4096"),
            ("PGW_MODEL_TTS_MAX_OUTPUT", "1024"),
            ("PGW_MODEL_TTS_PRICE", "per_character=0.000015"),
            ("PGW_MODEL_TTS_CURRENCY", "USD"),
            ("PGW_POOL_ALL_NAME", "every-one"),
            ("PGW_POOL_ALL", "llama3,local/tts-1"),
            ("PGW_POOL_ALL_ALGORITHM", "weighted"),
            ("PGW_POOL_ALL_WEIGHTS", "3,0"),
            ("PGW_POOL_ALL_TIMEOUT_SECONDS", "20"),
            ("PGW_POOL_ALL_RETRIES", "2"),
            ("PGW_CLIENT_A_KEY_FILE", &key_file),
            ("PGW_CLIENT_A_TPM", "100000"),
            ("PGW_CLIENT_A_CONCURRENCY", "4"),
            ("PGW_CLIENT_A_MODELS", "llama3,@every-one"),
        ])
        .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(c.listen, "127.0.0.1:9999".parse().unwrap());
        assert_eq!((c.log_format, c.log_bodies, c.log_body_max_bytes), (LogFormat::Json, AuditMode::OnRequest, 4096));
        assert_eq!((c.unsupported_params, c.currency.as_str()), (UnsupportedParams::Reject, "EUR"));
        assert_eq!(c.egress_allow_private, ["gw-mock", "10.0.0.0/8"]);

        let local = c.providers.iter().find(|p| p.name == "local").unwrap();
        assert_eq!(
            (local.base_url.as_str(), local.key.is_none(), local.timeout_seconds),
            ("http://ollama:11434/v1", true, 60)
        );
        let goai = c.providers.iter().find(|p| p.label == "GOAI").unwrap();
        assert_eq!(
            (goai.name.as_str(), goai.preset.name, goai.timeout_seconds),
            ("gemini-openai", "gemini-openai", 30)
        );

        let llama = c.models.iter().find(|m| m.label == "LLAMA").unwrap();
        assert_eq!((llama.upstream.as_str(), &llama.price), ("llama3", &PriceCfg::Free));
        let emb = c.models.iter().find(|m| m.label == "EMB").unwrap();
        assert_eq!(
            (emb.kind, emb.capabilities, emb.upstream.as_str()),
            (ModelKind::Embedding, "embedding", "text-embedding-004")
        );
        let tts = c.models.iter().find(|m| m.label == "TTS").unwrap();
        assert_eq!(
            (tts.kind, tts.upstream.as_str(), tts.context, tts.max_output),
            (ModelKind::Speech, "gpt-4o-mini-tts", Some(4096), Some(1024))
        );
        assert!(
            matches!(&tts.price, PriceCfg::Rates { rates, currency } if rates.per_character == Some(0.000015) && currency == "USD")
        );

        let pool = &c.pools[0];
        assert_eq!(
            (pool.name.as_str(), pool.algorithm, pool.weights.as_slice()),
            ("every-one", Algorithm::Weighted, &[3, 0][..])
        );
        assert_eq!((pool.timeout_seconds, pool.retries), (Some(20), Some(2)));

        let a = &c.clients[0];
        assert_eq!((a.key.0.as_str(), a.tpm, a.concurrency), (KEY, Some(100_000), Some(4)));
        assert_eq!(a.allowed, Some(Allowed { models: vec!["llama3".into()], pools: vec!["every-one".into()] }));
    }

    #[test]
    fn mistakes_are_all_reported_and_never_show_a_key() {
        let mut pairs = minimal();
        pairs.extend([
            ("PGW_MODEL_SONNET_PRCE", "input=1"),
            ("PGW_provider_x", "ignored, not upper case"),
            ("PGW_PROVIDER_OPEN_AI_KEY", "x"),
            ("PGW_PROVIDER_MYSTERY_KEY", "x"),
            ("PGW_PROVIDER_VERTEX_KEY", "{}"),
            ("PGW_MODEL_BAD", "nowhere/model"),
            ("PGW_MODEL_DUP", "openai/gpt-5.6-luna"),
            ("PGW_MODEL_PRICED", "openai/x"),
            ("PGW_MODEL_PRICED_PRICE", "input=abc"),
            ("PGW_POOL_EMPTYISH", "openai/unknown"),
            ("PGW_CLIENT_WEAK_KEY", "sk-secret-not-a-pgw-key"),
            ("PGW_CLIENT_TWIN_KEY", KEY),
            ("PGW_CLIENT_PORTAL_RPM", "-1"),
            ("PGW_LISTEN", "everywhere"),
        ]);
        let e = errors(&pairs);
        let has = |part: &str| assert!(e.iter().any(|m| m.contains(part)), "no error about {part}: {e:#?}");
        has("PGW_MODEL_SONNET_PRCE: Model has no attribute _PRCE");
        has("PGW_PROVIDER_OPEN_AI_KEY: Provider has no attribute _AI_KEY");
        has("\"mystery\" is not a preset");
        has("name your Google Cloud project");
        has("\"nowhere/model\" does not start with a provider name");
        has("the model name \"openai/gpt-5.6-luna\" is used twice");
        has("\"input=abc\" is not rate=number");
        has("no model is named \"openai/unknown\"");
        has("PGW_CLIENT_WEAK_KEY: a client key looks like");
        has("client \"portal\" has the same key");
        has("PGW_CLIENT_PORTAL_RPM: \"-1\" is not a whole number");
        has("PGW_LISTEN");
        assert!(e.iter().all(|m| !m.contains("sk-secret") && !m.contains(KEY)), "{e:#?}");
    }

    #[test]
    fn empty_values_count_as_unset_and_compose_settings_are_ignored() {
        let mut pairs = minimal();
        pairs.extend([
            ("PGW_PROVIDER_DEEPSEEK_KEY", ""),
            ("PGW_LISTEN", ""),
            ("PGW_LITE_PORT", "14080"),
            ("PGW_VERSION", "0.1.0"),
            ("PGW_KEY_PEPPER", "from the full gateway's file"),
            ("PGW_ENV_FILE", "deploy/lite/.env"),
        ]);
        let c = read_pairs(&pairs).unwrap();
        assert_eq!(c.providers.len(), 2);
        assert_eq!(c.listen.port(), 8080);
        assert_eq!(c.warnings, ["PGW_KEY_PEPPER is not a setting of the minimal gateway and is ignored"]);
    }

    #[test]
    fn the_log_format_is_any_case_and_an_unknown_one_is_json_with_a_warning() {
        let mut pairs = minimal();
        pairs.push(("PGW_LOG_FORMAT", "TEXT"));
        let c = read_pairs(&pairs).unwrap();
        assert_eq!((c.log_format, c.warnings.len()), (LogFormat::Text, 0));
        let mut pairs = minimal();
        pairs.push(("PGW_LOG_FORMAT", "xml"));
        let c = read_pairs(&pairs).unwrap();
        assert_eq!(c.log_format, LogFormat::Json);
        assert_eq!(c.warnings, ["PGW_LOG_FORMAT: \"xml\" is not one of json, text; json is used"]);
    }

    #[test]
    fn nothing_to_serve_is_an_error() {
        let e = errors(&[]);
        assert!(e.iter().any(|m| m.starts_with("no provider")));
        assert!(e.iter().any(|m| m.starts_with("no client")));
        let e = errors(&[("PGW_PROVIDER_OPENAI_KEY", "k"), ("PGW_PROVIDER_OPENAI", "x")]);
        assert!(e.iter().any(|m| m.contains("PGW_PROVIDER_OPENAI takes no value of its own")), "{e:?}");
    }

    #[test]
    fn claude_models_get_the_preset_of_their_generation() {
        for (id, preset) in [
            ("claude-3-5-haiku-20241022", "anthropic-budget"),
            ("claude-haiku-4-5-20251001", "anthropic-budget"),
            ("claude-haiku-4-5", "anthropic-budget"),
            ("claude-sonnet-4-5-20250929", "anthropic-budget"),
            ("claude-sonnet-4-20250514", "anthropic-budget"),
            ("claude-opus-4-6", "anthropic-budget"),
            ("claude-opus-4-7", "anthropic-adaptive"),
            ("claude-opus-5", "anthropic-adaptive"),
            ("claude-sonnet-5", "anthropic-adaptive"),
            ("claude-haiku-5-5", "anthropic-adaptive"),
            ("claude-fable-5", "anthropic-adaptive"),
            ("claude-sonnet-5-5", "anthropic-sonnet-5-5"),
            ("claude-opus-5-5", "anthropic-always-thinking"),
            ("claude-fable-5-1", "anthropic-always-thinking"),
        ] {
            assert_eq!(anthropic_preset(id), Some(preset), "{id}");
        }
        assert_eq!(anthropic_preset("my-fine-tune"), None);
        // Through the configuration, and an explicit preset still wins.
        let c = read_pairs(&[
            ("PGW_PROVIDER_ANTHROPIC_KEY", "k"),
            ("PGW_MODEL_HAIKU", "anthropic/claude-haiku-4-5-20251001"),
            ("PGW_MODEL_OWN", "anthropic/claude-haiku-4-5"),
            ("PGW_MODEL_OWN_CAPABILITIES", "anthropic-adaptive"),
            ("PGW_CLIENT_A_KEY", KEY),
        ])
        .unwrap();
        let preset = |label: &str| c.models.iter().find(|m| m.label == label).unwrap().capabilities;
        assert_eq!((preset("HAIKU"), preset("OWN")), ("anthropic-budget", "anthropic-adaptive"));
    }

    #[test]
    fn known_variables_are_told_apart_from_unknown_ones() {
        for name in
            ["PGW_LISTEN", "RUST_LOG", "PGW_LITE_PORT", "PGW_MODEL_X1", "PGW_MODEL_X1_PRICE", "PGW_CLIENT_A_KEY_FILE"]
        {
            assert!(is_known(name), "{name}");
        }
        for name in ["PGW_KEY_PEPPER", "PGW_MODEL_X_Y_PRICE", "PGW_CLIENT_A", "PGW_POOL_p", "PGW_PROVIDER_A_MODELS"] {
            assert!(!is_known(name), "{name}");
        }
    }

    #[test]
    fn the_summary_names_everything_and_shows_no_secret() {
        let mut pairs = minimal();
        pairs.push(("PGW_CLIENT_PORTAL_MODELS", "anthropic/claude-sonnet-5-5,@fast"));
        let text = summary(&read_pairs(&pairs).unwrap()).join("\n");
        for part in [
            "listen 0.0.0.0:8080, log json, bodies off (at most 16384 bytes), unsupported parameters drop",
            "provider anthropic: preset anthropic, https://api.anthropic.com, key set, timeout 180 s",
            "model anthropic/claude-sonnet-5-5: claude-sonnet-5-5 at anthropic, chat, capabilities anthropic-sonnet-5-5, USD per million: input 3, output 15, cache_read 0.3",
            "model openai/gpt-5.6-luna: gpt-5.6-luna at openai, chat, capabilities openai-gpt-5-6, no price",
            "pool @fast: priority, openai/gpt-5.6-luna (1), anthropic/claude-sonnet-5-5 (1)",
            "client portal: 600 requests/min, only anthropic/claude-sonnet-5-5, @fast",
        ] {
            assert!(text.contains(part), "{part}\nnot in\n{text}");
        }
        assert!(!text.contains("sk-") && !text.contains(KEY), "{text}");
    }

    /// The example file names only variables the parser knows, and reads once its keys are
    /// filled in.
    #[test]
    fn the_example_env_file_matches_the_parser() {
        let text = include_str!("../../../deploy/lite/.env.example");
        for line in text.lines() {
            let line = line.trim_start_matches(['#', ' ']);
            let Some((name, _)) = line.split_once('=') else { continue };
            if name.starts_with("PGW_") || name == "RUST_LOG" {
                assert!(is_known(name), "the example names {name}, which the parser does not know");
            }
        }
        let mut vars: Vec<(String, String)> =
            crate::env::parse(text).unwrap().iter().map(|(k, v)| (k.into(), v.into())).collect();
        assert!(read(&Vars::from_pairs(vars.clone())).is_err(), "an unfilled example does not start");
        for (name, value) in &mut vars {
            if name.ends_with("_KEY") && value.is_empty() {
                *value = if name.starts_with("PGW_CLIENT_") { KEY.into() } else { "provider-key".into() };
            }
        }
        let c = read(&Vars::from_pairs(vars)).unwrap();
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!((c.providers.len(), c.models.len(), c.pools.len(), c.clients.len()), (2, 2, 1, 1));
    }

    const INTENTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/lite/intents.example.yaml");

    /// A file of its own for one test, in the temporary directory.
    fn file(name: &str, text: &str) -> String {
        let path = std::env::temp_dir().join(format!("pgw-lite-{}-{name}", std::process::id()));
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn the_intent_plugin_reads_its_catalogue_file() {
        let mut pairs = minimal();
        pairs.extend([("PGW_INTENT_MODE", "enforce"), ("PGW_INTENT_FILE", INTENTS), ("PGW_INTENT_MODEL", "@fast")]);
        let c = read_pairs(&pairs).unwrap();
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        let lines = summary(&c);
        let line = lines.iter().find(|l| l.starts_with("intent plugin")).expect("in the summary");
        assert_eq!(
            line,
            "intent plugin: enforce, engine llm (@fast), catalogue support (3 intents, 2 rules), on error closed"
        );
        let i = c.intent.expect("switched on");
        assert_eq!(
            (i.mode, i.engine, i.on_error, i.expose),
            (PluginMode::Enforce, EngineChoice::Llm, OnError::Closed, false)
        );
        assert_eq!((i.catalogue.name.as_str(), i.catalogue.intents.len()), ("support", 3));
        assert!(i.catalogue.problems().is_empty(), "the example is a usable catalogue");
        assert_eq!((i.model.as_deref(), i.jev_model, i.timeout_ms), (Some("@fast"), None, None));

        // JSON when the name says so; Jev takes the only decision model there is.
        let json = file("intents.json", r#"{"name": "c", "intents": [{"name": "billing", "description": "Bills."}]}"#);
        let mut pairs = minimal();
        pairs.extend([
            ("PGW_PROVIDER_TYPESAFE_KEY", "ts-test"),
            ("PGW_MODEL_JEV", "typesafe/jev-1.13.0"),
            ("PGW_INTENT_MODE", "shadow"),
            ("PGW_INTENT_FILE", &json),
            ("PGW_INTENT_ENGINE", "cascade"),
            ("PGW_INTENT_MODEL", "openai/gpt-5.6-luna"),
            ("PGW_INTENT_ON_ERROR", "open"),
            ("PGW_INTENT_TIMEOUT_MS", "2500"),
            ("PGW_INTENT_EXPOSE", "true"),
        ]);
        let i = read_pairs(&pairs).unwrap().intent.unwrap();
        assert_eq!(
            (i.mode, i.engine, i.on_error, i.expose),
            (PluginMode::Shadow, EngineChoice::Cascade, OnError::Open, true)
        );
        assert_eq!((i.jev_model.as_deref(), i.timeout_ms), (Some("typesafe/jev-1.13.0"), Some(2500)));
    }

    #[test]
    fn the_intent_plugin_says_what_is_missing() {
        let with = |extra: &[(&'static str, &str)]| {
            let mut pairs: Vec<(&str, &str)> = minimal();
            pairs.extend(extra.iter().copied());
            errors(&pairs)
        };
        let e = with(&[("PGW_INTENT_MODE", "enforce")]);
        assert!(e.iter().any(|m| m.starts_with("PGW_INTENT_FILE is not set")), "{e:?}");
        assert!(e.iter().any(|m| m.starts_with("PGW_INTENT_MODEL is not set")), "{e:?}");

        let e = with(&[("PGW_INTENT_MODE", "on"), ("PGW_INTENT_FILE", "/nonexistent/intents.yaml")]);
        assert!(e.iter().any(|m| m.contains("\"on\" is not one of off, shadow, enforce")), "{e:?}");

        let e = with(&[("PGW_INTENT_MODE", "shadow"), ("PGW_INTENT_FILE", "/nonexistent/intents.yaml")]);
        assert!(e.iter().any(|m| m.starts_with("PGW_INTENT_FILE: reading /nonexistent/intents.yaml")), "{e:?}");

        let e = with(&[
            ("PGW_INTENT_MODE", "shadow"),
            ("PGW_INTENT_FILE", INTENTS),
            ("PGW_INTENT_MODEL", "openai/gpt-nonexistent"),
        ]);
        assert!(e.iter().any(|m| m.contains("is not a chat model or @pool")), "{e:?}");

        // Jev needs a decision model; a chat model is not one.
        let e = with(&[("PGW_INTENT_MODE", "shadow"), ("PGW_INTENT_FILE", INTENTS), ("PGW_INTENT_ENGINE", "jev")]);
        assert!(e.iter().any(|m| m.starts_with("PGW_INTENT_JEV_MODEL is not set")), "{e:?}");
        let e = with(&[
            ("PGW_INTENT_MODE", "shadow"),
            ("PGW_INTENT_FILE", INTENTS),
            ("PGW_INTENT_ENGINE", "jev"),
            ("PGW_INTENT_JEV_MODEL", "openai/gpt-5.6-luna"),
        ]);
        assert!(e.iter().any(|m| m.contains("is not a decision model")), "{e:?}");

        // What the catalogue holds is checked as in the console; booleans are true or false.
        let bad = file("bad.yaml", "name: c\nintents:\n  - name: unknown\n    description: Taken.\n");
        let e = with(&[("PGW_INTENT_MODE", "shadow"), ("PGW_INTENT_FILE", &bad), ("PGW_INTENT_MODEL", "@fast")]);
        assert!(e.iter().any(|m| m.contains("PGW_INTENT_FILE: the intent \"unknown\"")), "{e:?}");
        let yes = file(
            "yes.yaml",
            "name: c\nintents: [{ name: a, description: A. }]\nrules: [{ name: r, then: { action: flag, alert: yes } }]\n",
        );
        let e = with(&[("PGW_INTENT_MODE", "shadow"), ("PGW_INTENT_FILE", &yes), ("PGW_INTENT_MODEL", "@fast")]);
        assert!(e.iter().any(|m| m.contains("is not an intent catalogue")), "{e:?}");

        // Off is off, and says so when the rest is set.
        let mut pairs = minimal();
        pairs.push(("PGW_INTENT_FILE", INTENTS));
        let c = read_pairs(&pairs).unwrap();
        assert!(c.intent.is_none());
        assert!(c.warnings.iter().any(|w| w.contains("the plugin is off")), "{:?}", c.warnings);
    }

    #[test]
    fn a_local_provider_needs_no_key_but_a_hosted_one_does() {
        let e =
            errors(&[("PGW_PROVIDER_OLLAMACLOUD_PRESET", "ollama-cloud"), ("PGW_PROVIDER_OLLAMA_PRESET", "ollama")]);
        assert!(e.iter().any(|m| m.contains("provider \"ollamacloud\" needs a key")), "{e:?}");
        assert!(!e.iter().any(|m| m.contains("provider \"ollama\" needs")), "{e:?}");
    }
}
