//! Provider profiles and inventories (docs/concept.md §2.1).
//!
//! The profile says who the provider is in words people read. The inventory lists, optionally,
//! the hardware and software the provider runs on: GPUs and memory, drivers, serving engines and
//! libraries. Consumers can filter on it ("an H100 with at least 80 GB", "vLLM 0.7 or later"),
//! and a resource can point at the items it runs on. Everything in it is declared by the
//! provider; the broker marks it so and never claims to have checked it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::is_slug;
use crate::limits;
use crate::resource::Visibility;
use crate::text::Text;
use crate::validate::{self, Problems};

/// What a provider says about itself when it announces.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    /// A slug naming the provider. In `open` mode it also identifies the provider.
    pub name: String,
    #[serde(flatten)]
    pub text: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website: Option<String>,
    /// How to reach a person: an email address or a URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_url: Option<String>,
    /// Regions or ISO 3166 country codes the provider operates in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub certifications: Vec<Certification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Inventory>,
}

/// A certification the provider declares (ISO 27001, SOC 2, …).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Certification {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<jiff::Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_url: Option<String>,
    /// Always `declared` today; `attested` is reserved for checked claims.
    #[serde(default)]
    pub assurance: Assurance,
}

/// Whether anyone but the provider has checked a claim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Assurance {
    #[default]
    Declared,
    Attested,
}

/// The hardware and software the provider runs on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    /// Who may see the inventory. It never widens the provider's resources' visibility.
    #[serde(default)]
    pub visibility: Visibility,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hardware: Vec<HardwareItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub software: Vec<SoftwareItem>,
}

/// One kind of hardware, `count` of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HardwareItem {
    /// A slug unique within the inventory, for resources to point at.
    pub id: String,
    #[serde(rename = "type")]
    pub kind: HardwareType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// The model as the vendor names it: `H100-SXM`, `MI300X`, `EPYC 9654`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default = "one")]
    pub count: u32,
    /// Memory per unit, in GB: a GPU's memory, or the size of a memory or storage item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_gb: Option<f64>,
    /// `nvlink`, `infiniband`, `pcie`, `ethernet-400g`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interconnect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub assurance: Assurance,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HardwareType {
    Gpu,
    Cpu,
    Tpu,
    Npu,
    Accelerator,
    Memory,
    Storage,
    Network,
}

/// One piece of software.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SoftwareItem {
    /// A slug unique within the inventory, for resources to point at.
    pub id: String,
    pub kind: SoftwareKind,
    /// The name, lower case: `cuda`, `vllm`, `pytorch`, `onnxruntime`, `ubuntu`.
    pub name: String,
    /// A version; semantic versions (`0.7.3`, `12.6`) can be filtered with ranges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub assurance: Assurance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SoftwareKind {
    Os,
    Driver,
    Runtime,
    Framework,
    Library,
    ServingEngine,
    ModelFormat,
    Other,
}

impl ProviderProfile {
    pub fn validate(&self, p: &mut Problems, max_description: usize) {
        p.check(is_slug(&self.name, limits::SLUG_CHARS), "provider.name", "a slug (a-z, 0-9, - _ . : /)");
        self.text.validate(p, "provider", max_description);
        for (path, url) in [
            ("provider.website", &self.website),
            ("provider.terms_url", &self.terms_url),
            ("provider.privacy_url", &self.privacy_url),
        ] {
            if let Some(u) = url {
                validate::url(p, path, u);
            }
        }
        if let Some(c) = &self.contact {
            validate::text_len(p, "provider.contact", c, limits::URL_CHARS);
        }
        p.check(self.locations.len() <= limits::TAGS, "provider.locations", "too many");
        for (i, l) in self.locations.iter().enumerate() {
            p.check(is_slug(l, limits::SLUG_CHARS), format!("provider.locations[{i}]"), "a slug");
        }
        p.check(self.certifications.len() <= limits::TAGS, "provider.certifications", "too many");
        for (i, c) in self.certifications.iter().enumerate() {
            validate::text_len(p, &format!("provider.certifications[{i}].name"), &c.name, limits::TITLE_CHARS);
            p.check(
                c.assurance == Assurance::Declared,
                format!("provider.certifications[{i}].assurance"),
                "only the broker attests",
            );
            if let Some(u) = &c.evidence_url {
                validate::url(p, &format!("provider.certifications[{i}].evidence_url"), u);
            }
        }
        if let Some(inv) = &self.inventory {
            inv.validate(p);
        }
    }
}

impl Inventory {
    pub fn validate(&self, p: &mut Problems) {
        p.check(self.hardware.len() <= limits::INVENTORY_ITEMS, "provider.inventory.hardware", "too many items");
        p.check(self.software.len() <= limits::INVENTORY_ITEMS, "provider.inventory.software", "too many items");
        let mut ids = std::collections::HashSet::new();
        for (i, h) in self.hardware.iter().enumerate() {
            let at = format!("provider.inventory.hardware[{i}]");
            p.check(is_slug(&h.id, limits::SLUG_CHARS), format!("{at}.id"), "a slug");
            p.check(ids.insert(h.id.as_str()), format!("{at}.id"), "not unique in the inventory");
            p.check(h.count >= 1, format!("{at}.count"), "at least 1");
            if let Some(m) = h.memory_gb {
                p.check(m.is_finite() && m > 0.0, format!("{at}.memory_gb"), "a positive number");
            }
            for (f, v) in
                [("vendor", &h.vendor), ("model", &h.model), ("interconnect", &h.interconnect), ("region", &h.region)]
            {
                if let Some(v) = v {
                    validate::text_len(p, &format!("{at}.{f}"), v, limits::TITLE_CHARS);
                }
            }
            if let Some(n) = &h.notes {
                validate::text_len(p, &format!("{at}.notes"), n, limits::NOTE_CHARS);
            }
            p.check(h.assurance == Assurance::Declared, format!("{at}.assurance"), "only the broker attests");
        }
        for (i, s) in self.software.iter().enumerate() {
            let at = format!("provider.inventory.software[{i}]");
            p.check(is_slug(&s.id, limits::SLUG_CHARS), format!("{at}.id"), "a slug");
            p.check(ids.insert(s.id.as_str()), format!("{at}.id"), "not unique in the inventory");
            p.check(is_slug(&s.name, limits::SLUG_CHARS), format!("{at}.name"), "a slug, lower case");
            if let Some(v) = &s.version {
                validate::text_len(p, &format!("{at}.version"), v, 64);
            }
            if let Some(n) = &s.notes {
                validate::text_len(p, &format!("{at}.notes"), n, limits::NOTE_CHARS);
            }
            p.check(s.assurance == Assurance::Declared, format!("{at}.assurance"), "only the broker attests");
        }
    }

    pub fn hardware(&self, id: &str) -> Option<&HardwareItem> {
        self.hardware.iter().find(|h| h.id == id)
    }

    pub fn software(&self, id: &str) -> Option<&SoftwareItem> {
        self.software.iter().find(|s| s.id == id)
    }
}

impl HardwareItem {
    /// The model in the form the index keys it: lower case, spaces as `-`.
    pub fn model_key(&self) -> Option<String> {
        self.model.as_ref().map(|m| m.trim().to_ascii_lowercase().replace(' ', "-"))
    }
}

impl SoftwareItem {
    /// The version as a semantic version, padding `12.6` to `12.6.0` and `12` to `12.0.0`.
    pub fn semver(&self) -> Option<semver::Version> {
        let v = self.version.as_deref()?.trim().trim_start_matches('v');
        semver::Version::parse(v).ok().or_else(|| {
            let parts: Vec<&str> = v.split('.').collect();
            match parts.len() {
                1 => semver::Version::parse(&format!("{v}.0.0")).ok(),
                2 => semver::Version::parse(&format!("{v}.0")).ok(),
                _ => None,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sw(v: &str) -> SoftwareItem {
        SoftwareItem {
            id: "x".into(),
            kind: SoftwareKind::Library,
            name: "x".into(),
            version: Some(v.into()),
            notes: None,
            assurance: Assurance::Declared,
        }
    }

    #[test]
    fn versions_are_padded() {
        assert_eq!(sw("12.6").semver(), Some(semver::Version::new(12, 6, 0)));
        assert_eq!(sw("v0.7.3").semver(), Some(semver::Version::new(0, 7, 3)));
        assert_eq!(sw("12").semver(), Some(semver::Version::new(12, 0, 0)));
        assert_eq!(sw("latest").semver(), None);
    }

    #[test]
    fn inventory_ids_are_unique_and_assurance_is_declared() {
        let inv: Inventory = serde_json::from_value(serde_json::json!({
            "hardware": [{"id": "gpu", "type": "gpu", "model": "H100", "count": 8, "memory_gb": 80, "assurance": "attested"}],
            "software": [{"id": "gpu", "kind": "serving_engine", "name": "vllm", "version": "0.7.3"}]
        }))
        .unwrap();
        let mut p = Problems::new();
        inv.validate(&mut p);
        let all = p.to_string();
        assert!(all.contains("software[0].id: not unique"), "{all}");
        assert!(all.contains("hardware[0].assurance: only the broker attests"), "{all}");
    }
}
