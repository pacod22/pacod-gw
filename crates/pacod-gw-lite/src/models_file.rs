//! The models file (`PGW_MODELS_FILE`, docs/lite.md section 3): models, their supported parameters
//! and their prices from a file, in the format the aggregator's catalogue and a gateway's upload
//! share. `.env` still wins for what it sets, and a guess fills what neither states.
//!
//! A catalogue defines the models of the vendors a provider is configured for. An OpenRouter or
//! LiteLLM list holds thousands of models nobody chose, so it only completes the models `.env`
//! names: the same rule as the full gateway's sync.

use std::collections::{BTreeMap, BTreeSet};

use pacod_agg_catalog as cat;
use pacod_gw_core::catalog::convert;
use pacod_gw_core::model::repo::{Modalities, capability_presets, is_model_name, suggested_preset};
use pacod_gw_core::model::{Capabilities, Family, ModelKind, PriceTier, PriceWindow, Rates};
use serde::Serialize;

use crate::config::{ModelCfg, PriceCfg, ProviderCfg, guess, is_currency};

/// What a price holds beyond the rates `_PRICE` can state.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PriceMore {
    pub variants: BTreeMap<String, Rates>,
    pub tiers: Vec<PriceTier>,
    pub windows: Vec<PriceWindow>,
}

/// What the file said of a model, for `check`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FromFile {
    /// What was taken from it.
    pub taken: Vec<&'static str>,
    /// What it states, and `.env` overrules.
    pub overruled: Vec<&'static str>,
}

/// What a reading of the file came to, for `check`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelsFile {
    pub path: String,
    /// The format it was found to be in.
    pub format: &'static str,
    /// Models it defined.
    pub defined: usize,
    /// Models `.env` names that it completed.
    pub completed: usize,
    /// Vendors of its models that no provider is configured for: their models are left out.
    pub without_provider: Vec<String>,
    /// Models it says are retired, left out.
    pub retired: usize,
}

/// The preset for a model of a kind the guess did not arrive at.
fn preset_of(kind: ModelKind, family: Family, upstream: &str) -> &'static str {
    match kind {
        ModelKind::Chat => suggested_preset(family, upstream, None, None),
        kind => kind.preset(),
    }
}

fn capabilities_of(preset: &str) -> Capabilities {
    capability_presets().into_iter().find(|p| p.name == preset).map(|p| p.capabilities).unwrap_or_default()
}

/// A count of tokens as the gateway keeps it. A value that does not fit is no real one.
fn tokens(value: Option<u64>) -> Option<u32> {
    value.and_then(|v| u32::try_from(v).ok()).filter(|v| *v > 0)
}

/// Lays what `entry` states over `m`, keeping what `.env` set.
fn complete(m: &mut ModelCfg, entry: &cat::Model, family: Family, default_currency: &str, warnings: &mut Vec<String>) {
    let mut from = FromFile::default();
    if let Some(kind) = convert::kind(entry).filter(|k| *k != m.kind) {
        if m.kind_set {
            from.overruled.push("kind");
        } else {
            m.kind = kind;
            if !m.capabilities_set {
                m.capabilities = preset_of(kind, family, &m.upstream);
            }
            from.taken.push("kind");
        }
    }

    let base = capabilities_of(m.capabilities);
    let stated = convert::capabilities(entry, base.clone());
    if stated != base {
        if m.capabilities_set {
            from.overruled.push("capabilities");
        } else {
            m.stated = Some(Box::new(stated));
            from.taken.push("capabilities");
        }
    }

    for (name, slot, value) in [
        ("context", &mut m.context, tokens(entry.properties.context_window)),
        ("max output", &mut m.max_output, tokens(entry.properties.max_output_tokens)),
    ] {
        if value.is_some() {
            if slot.is_some() {
                from.overruled.push(name);
            } else {
                *slot = value;
                from.taken.push(name);
            }
        }
    }

    if let Some(mo) = entry.properties.modalities.as_ref().filter(|mo| !mo.input.is_empty() || !mo.output.is_empty()) {
        m.modalities = Some(Modalities { input: mo.input.clone(), output: mo.output.clone() });
        from.taken.push("modalities");
    }

    if let Some(price) = entry.price.as_ref().filter(|p| !p.rates.is_empty()) {
        let wrong = price.rates.implausible();
        if m.price != PriceCfg::None {
            from.overruled.push("price");
        } else if let Some(rate) = wrong {
            warnings.push(format!("the models file: the {rate} rate of {} cannot be a price; it has none", m.name));
        } else if !is_currency(&price.currency) {
            warnings.push(format!(
                "the models file: \"{}\", the currency of {}, is not a currency code like USD; it has no price",
                price.currency, m.name
            ));
        } else {
            let currency = if price.currency.is_empty() { default_currency } else { &price.currency };
            m.price = PriceCfg::Rates { rates: Box::new(convert::rates(&price.rates)), currency: currency.to_owned() };
            m.price_more = Some(PriceMore {
                variants: price.variants.iter().map(|(k, r)| (k.clone(), convert::rates(r))).collect(),
                tiers: convert::tiers(price),
                windows: convert::windows(price),
            });
            from.taken.push("price");
        }
    }
    m.from_file = Some(from);
}

/// The vendor a provider's models are filed under.
fn vendor_of(p: &ProviderCfg) -> &str {
    p.preset.catalog_vendor.unwrap_or(&p.name)
}

/// The label of a model the file defines. Labels name objects: stable from one start to the next,
/// and different from one another and from those of `.env`.
fn label_of(name: &str, models: &[ModelCfg]) -> String {
    let plain: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect();
    let base = format!("FILE_{plain}");
    let taken = |label: &str| models.iter().any(|m| m.label == label);
    if !taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}_{n}")).find(|l| !taken(l)).expect("a free label")
}

/// Reads the file, completes `models` and adds what it defines. A file that cannot be read or does
/// not parse is an error: a gateway that started without the prices it was given would log the
/// wrong costs until someone noticed.
pub fn apply(
    path: &str,
    providers: &[ProviderCfg],
    models: &mut Vec<ModelCfg>,
    default_currency: &str,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Option<ModelsFile> {
    let body = match std::fs::read(path) {
        Ok(body) => body,
        Err(e) => {
            errors.push(format!("PGW_MODELS_FILE: reading {path}: {e}"));
            return None;
        }
    };
    let format = serde_json::from_slice(&body).ok().as_ref().and_then(cat::detect);
    let catalog = match cat::parse(&body, None) {
        Ok(catalog) => catalog,
        Err(e) => {
            errors.push(format!("PGW_MODELS_FILE: {path}: {e}"));
            return None;
        }
    };
    let index = catalog.index();
    let mut report = ModelsFile {
        path: path.to_owned(),
        format: format.map_or("unknown", cat::Format::name),
        defined: 0,
        completed: 0,
        without_provider: Vec::new(),
        retired: 0,
    };

    for m in models.iter_mut() {
        let Some(p) = providers.iter().find(|p| p.name == m.provider) else { continue };
        match index.find(vendor_of(p), &m.upstream) {
            cat::Found::One(entry) if entry.provider.as_deref().is_none_or(|named| named == p.name) => {
                complete(m, entry, p.preset.family, default_currency, warnings);
                report.completed += 1;
            }
            cat::Found::Ambiguous(ids) => warnings.push(format!(
                "the models file: several models match {} ({}); none is taken",
                m.name,
                ids.join(", ")
            )),
            _ => {}
        }
    }

    if format != Some(cat::Format::Catalog) {
        return Some(report);
    }

    let mut without_provider = BTreeSet::new();
    for entry in &catalog.models {
        if entry.status == Some(cat::ModelStatus::Retired) {
            report.retired += 1;
            continue;
        }
        let named = entry.provider.as_deref();
        let vendor = entry.vendor.as_deref().unwrap_or_default();
        let serving: Vec<&ProviderCfg> = providers
            .iter()
            .filter(|p| named.map_or(vendor_of(p) == vendor || p.name == vendor, |named| named == p.name))
            .collect();
        if serving.is_empty() {
            without_provider.insert(named.unwrap_or(vendor).to_owned());
            continue;
        }
        for p in serving {
            let known = |m: &ModelCfg| {
                m.provider == p.name && (m.upstream == entry.api_id || entry.aliases.contains(&m.upstream))
            };
            if models.iter().any(known) {
                continue;
            }
            let name = entry.name.clone().unwrap_or_else(|| format!("{}/{}", p.name, entry.api_id));
            if !is_model_name(&name) {
                warnings.push(format!("the models file: \"{name}\" is not a usable model name; it is left out"));
                continue;
            }
            // A name the file gives holds for one provider, and `.env` may hold it already.
            if models.iter().any(|m| m.name == name) {
                warnings.push(format!("the models file: the model name \"{name}\" is in use; this entry is left out"));
                continue;
            }
            let (kind, preset) = guess(p, &entry.api_id);
            let stated_kind = convert::kind(entry).unwrap_or(kind);
            let mut m = ModelCfg {
                label: label_of(&name, models),
                name,
                provider: p.name.clone(),
                upstream: entry.api_id.clone(),
                kind: stated_kind,
                kind_set: false,
                capabilities: if stated_kind == kind {
                    preset
                } else {
                    preset_of(stated_kind, p.preset.family, &entry.api_id)
                },
                capabilities_set: false,
                stated: None,
                modalities: None,
                context: None,
                max_output: None,
                price: PriceCfg::None,
                price_more: None,
                from_file: None,
            };
            complete(&mut m, entry, p.preset.family, default_currency, warnings);
            models.push(m);
            report.defined += 1;
        }
    }
    report.without_provider = without_provider.into_iter().collect();
    Some(report)
}

/// A file that lasts as long as a test.
#[cfg(test)]
pub(crate) struct TempFile(std::path::PathBuf);

#[cfg(test)]
impl TempFile {
    pub(crate) fn new(body: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pgw-lite-models-{}-{n}.json", std::process::id()));
        std::fs::write(&path, body).unwrap();
        Self(path)
    }

    pub(crate) fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

#[cfg(test)]
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use pacod_gw_core::model::ParamSupport;

    use crate::config::{Config, read};
    use crate::env::Vars;

    const KEY: &str = "pgw_portal_0123456789abcdefghij";

    fn file(body: &str) -> super::TempFile {
        super::TempFile::new(body)
    }

    fn with(path: &str, extra: &[(&str, &str)]) -> Result<Config, Vec<String>> {
        let mut pairs =
            vec![("PGW_PROVIDER_OPENAI_KEY", "sk-a"), ("PGW_CLIENT_PORTAL_KEY", KEY), ("PGW_MODELS_FILE", path)];
        pairs.extend_from_slice(extra);
        read(&Vars::from_pairs(pairs.iter().copied())).map_err(|e| e.0)
    }

    const CATALOGUE: &str = r#"{
        "schema": "pacod.catalog/v1",
        "models": [
            { "vendor": "openai", "api_id": "gpt-x", "kind": "chat",
              "properties": { "context_window": 400000, "max_output_tokens": 16000, "vision": false,
                              "modalities": { "input": ["text"], "output": ["text"] } },
              "parameters": { "temperature": { "support": "default_only", "default": 1 },
                              "top_p": { "support": "rejected" } },
              "price": { "currency": "EUR", "rates": { "input": 2.0, "output": 8.0 },
                         "variants": { "batch": { "input": 1.0 } },
                         "tiers": [{ "above_input_tokens": 200000, "input": 4.0 }] } },
            { "vendor": "openai", "api_id": "gpt-named", "name": "writer" },
            { "vendor": "openai", "api_id": "gpt-old", "status": "retired" },
            { "vendor": "cohere", "api_id": "command-z" },
            { "provider": "openai", "api_id": "tuned-1" }
        ]
    }"#;

    fn model<'a>(cfg: &'a Config, name: &str) -> &'a super::ModelCfg {
        cfg.models.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("no {name} in {:?}", cfg.models))
    }

    #[test]
    fn a_catalogue_defines_the_models_of_configured_providers() {
        let f = file(CATALOGUE);
        let cfg = with(f.path(), &[]).unwrap();
        let mut names: Vec<&str> = cfg.models.iter().map(|m| m.name.as_str()).collect();
        names.sort_unstable();
        // Under the provider's name, or its own; a vendor's id or a named provider both map.
        assert_eq!(names, ["openai/gpt-x", "openai/tuned-1", "writer"]);
        assert_eq!(model(&cfg, "writer").upstream, "gpt-named");

        let report = cfg.models_file.as_ref().unwrap();
        assert_eq!((report.format, report.defined, report.completed, report.retired), ("catalog", 3, 0, 1));
        assert_eq!(report.without_provider, ["cohere"], "check lists what was left out");
    }

    #[test]
    fn parameters_and_properties_replace_the_guess() {
        let f = file(CATALOGUE);
        let cfg = with(f.path(), &[]).unwrap();
        let m = model(&cfg, "openai/gpt-x");
        let stated = m.stated.as_ref().expect("the file states what the model takes");
        assert_eq!((stated.temperature, stated.top_p), (ParamSupport::DefaultOnly, ParamSupport::Rejected));
        assert!(!stated.vision);
        assert_eq!((m.context, m.max_output), (Some(400_000), Some(16_000)));
        assert_eq!(m.modalities.as_ref().unwrap().input, ["text"]);
    }

    #[test]
    fn a_price_keeps_what_the_variable_cannot_say() {
        let f = file(CATALOGUE);
        let cfg = with(f.path(), &[]).unwrap();
        let m = model(&cfg, "openai/gpt-x");
        let super::PriceCfg::Rates { rates, currency } = &m.price else { panic!("no price: {:?}", m.price) };
        assert_eq!((rates.input, currency.as_str()), (Some(2.0), "EUR"));
        let more = m.price_more.as_ref().unwrap();
        assert_eq!(more.variants["batch"].input, Some(1.0));
        assert_eq!((more.tiers[0].above_input_tokens, more.tiers[0].rates.input), (200_000, Some(4.0)));
    }

    #[test]
    fn what_the_variables_set_wins() {
        let f = file(CATALOGUE);
        let path = f.path();
        let cfg = with(
            path,
            &[
                ("PGW_MODEL_X", "openai/gpt-x"),
                ("PGW_MODEL_X_PRICE", "input=9,output=9"),
                ("PGW_MODEL_X_CAPABILITIES", "openai-chat"),
                ("PGW_MODEL_X_CONTEXT", "1000"),
            ],
        )
        .unwrap();
        let m = model(&cfg, "openai/gpt-x");
        assert_eq!(m.label, "X", "the file completes the model; it does not add a second one");
        assert!(matches!(&m.price, super::PriceCfg::Rates { rates, .. } if rates.input == Some(9.0)));
        assert!(m.price_more.is_none(), "the variable's price stands alone");
        assert!(m.stated.is_none(), "a capability preset in .env stands");
        assert_eq!(m.capabilities, "openai-chat");
        assert_eq!((m.context, m.max_output), (Some(1000), Some(16_000)), "what .env leaves open comes from the file");
        let from = m.from_file.as_ref().unwrap();
        assert!(["price", "capabilities", "context"].iter().all(|v| from.overruled.contains(v)), "{from:?}");
        assert!(from.taken.contains(&"max output"), "{from:?}");
        assert_eq!(cfg.models_file.as_ref().unwrap().completed, 1);
    }

    #[test]
    fn a_list_completes_the_named_models_and_defines_none() {
        let litellm = r#"{ "gpt-x": { "litellm_provider": "openai", "mode": "chat", "max_input_tokens": 300000,
                                      "input_cost_per_token": 0.000002, "output_cost_per_token": 0.000008 },
                           "gpt-other": { "litellm_provider": "openai", "mode": "chat",
                                          "input_cost_per_token": 0.000001, "output_cost_per_token": 0.000002 } }"#;
        let f = file(litellm);
        let cfg = with(f.path(), &[("PGW_MODEL_X", "openai/gpt-x")]).unwrap();
        assert_eq!(cfg.models.len(), 1, "a list of thousands defines nothing");
        let report = cfg.models_file.as_ref().unwrap();
        assert_eq!((report.format, report.defined, report.completed), ("litellm", 0, 1));
        let m = model(&cfg, "openai/gpt-x");
        assert!(matches!(&m.price, super::PriceCfg::Rates { rates, .. } if rates.input == Some(2.0)), "{:?}", m.price);
        assert_eq!(m.context, Some(300_000));
    }

    #[test]
    fn pools_and_clients_can_use_what_the_file_defines() {
        let f = file(CATALOGUE);
        let cfg = with(
            f.path(),
            &[("PGW_POOL_FAST", "openai/gpt-x,writer"), ("PGW_CLIENT_PORTAL_MODELS", "@fast,openai/gpt-x")],
        )
        .unwrap();
        assert_eq!(cfg.pools[0].models, ["openai/gpt-x", "writer"]);
    }

    #[test]
    fn the_same_file_gives_the_same_labels() {
        let f = file(CATALOGUE);
        let labels = |cfg: Config| cfg.models.into_iter().map(|m| m.label).collect::<Vec<_>>();
        let path = f.path();
        let first = labels(with(path, &[]).unwrap());
        assert_eq!(first, labels(with(path, &[]).unwrap()));
        assert!(first.contains(&"FILE_OPENAI_GPT_X".to_owned()), "{first:?}");
    }

    #[test]
    fn a_rate_that_cannot_be_a_price_is_left_out_and_said() {
        let f = file(
            r#"{ "schema": "pacod.catalog/v1", "models": [
                { "vendor": "openai", "api_id": "gpt-y", "price": { "rates": { "input": 999999 } } } ] }"#,
        );
        let cfg = with(f.path(), &[]).unwrap();
        assert_eq!(model(&cfg, "openai/gpt-y").price, super::PriceCfg::None);
        assert!(cfg.warnings.iter().any(|w| w.contains("cannot be a price")), "{:?}", cfg.warnings);
    }

    #[test]
    fn a_name_in_use_stays_with_env() {
        let f = file(
            r#"{ "schema": "pacod.catalog/v1", "models": [
                { "vendor": "openai", "api_id": "gpt-y", "name": "writer" } ] }"#,
        );
        let cfg = with(f.path(), &[("PGW_MODEL_W", "writer"), ("PGW_MODEL_W_PROVIDER", "openai")]);
        let cfg = cfg.unwrap();
        assert_eq!(model(&cfg, "writer").label, "W");
        assert!(cfg.warnings.iter().any(|w| w.contains("is in use")), "{:?}", cfg.warnings);
    }

    #[test]
    fn a_file_that_cannot_be_used_stops_the_start_with_the_place() {
        let missing = with("/nonexistent/models.json", &[]).unwrap_err();
        assert!(missing.iter().any(|e| e.contains("PGW_MODELS_FILE: reading /nonexistent/models.json")), "{missing:?}");

        let f = file("{ \"schema\": \"pacod.catalog/v1\",\n  \"models\": [ oops ] }");
        let path = f.path().to_owned();
        let broken = with(&path, &[]).unwrap_err();
        assert!(broken.iter().any(|e| e.contains(&path) && e.contains("line 2")), "{broken:?}");

        let f = file(r#"{ "schema": "pacod.catalog/v2", "models": [] }"#);
        assert!(with(f.path(), &[]).is_err(), "a major version it does not know is refused");
    }

    #[test]
    fn the_example_file_defines_its_models() {
        let cfg = with(concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/lite/models.example.json"), &[]).unwrap();
        assert!(cfg.models_file.as_ref().unwrap().defined >= 2, "{:?}", cfg.models_file);
    }
}
