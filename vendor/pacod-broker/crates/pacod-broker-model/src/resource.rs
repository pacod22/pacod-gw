//! Resources (docs/concept.md §2.2): one endpoint someone can call, with everything a consumer
//! needs to choose it. As in pacod-gw's model repository, a resource is one provider endpoint for
//! one thing: the same model served by two providers is two resources.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::availability::Availability;
use crate::ids::is_slug;
use crate::limits;
use crate::pricing::Pricing;
use crate::text::Text;
use crate::validate::{self, Problems};

/// The kinds the broker knows. Others are written `other:<slug>`.
pub const KINDS: [&str; 7] = ["model", "mcp_server", "agent", "tool", "compute", "embedding", "dataset"];

/// The interface protocols the broker knows. Others are written `other:<slug>`.
pub const PROTOCOLS: [&str; 11] = [
    "openai-chat",
    "openai-responses",
    "anthropic-messages",
    "gemini",
    "pacod-gw",
    "mcp-streamable-http",
    "mcp-sse",
    "a2a",
    "openapi",
    "grpc",
    "http",
];

/// What a provider announces for one resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceSpec {
    /// The provider's own key for the resource, a slug unique among its resources. The
    /// resource's id is derived from it, so it stays the same across announcements.
    pub key: String,
    /// `model`, `mcp_server`, `agent`, `tool`, `compute`, `embedding`, `dataset`, or
    /// `other:<slug>`.
    pub kind: String,
    /// A slug naming what is served: the model id, the server's name.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(flatten)]
    pub text: Text,
    #[serde(default)]
    pub interfaces: Vec<Interface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelInfo>,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_on: Option<RunsOn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Pricing>,
    #[serde(default)]
    pub availability: Availability,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub access: Access,
}

/// How to call the resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Interface {
    pub protocol: String,
    pub url: String,
    #[serde(default)]
    pub auth: Auth,
    /// A machine-readable description: an OpenAPI document, an A2A agent card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    #[serde(default)]
    pub scheme: AuthScheme,
    /// Where to get credentials: a sign-up page, a token endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obtain_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    #[default]
    None,
    Bearer,
    ApiKey,
    Oauth2,
    /// A pacod-id token: the caller's own, or a service account's.
    PacodId,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelInfo {
    /// `claude`, `gpt`, `llama`, `qwen`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// The id the upstream provider uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub modalities: Modalities,
    /// `fp16`, `fp8`, `int4`, `q4_k_m`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantisation: Option<String>,
    /// Parameters, in billions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters_b: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Modalities {
    /// `text`, `image`, `audio`, `video`, `file`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tools: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vision: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reasoning: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub structured_output: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub streaming: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prompt_cache: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub embeddings: bool,
    /// For MCP servers: the tools they offer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_tools: Vec<McpTool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_protocol_version: Option<String>,
    /// For compute: the accelerator, its memory and how many.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_gb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Anything else, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// The inventory items a resource runs on, by their ids in the provider's inventory.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunsOn {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hardware: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub software: Vec<String>,
}

/// What happens to the data sent to the resource.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// `none`, `transient`, `30d`, … as the provider states it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_retention: Option<String>,
    /// Whether inputs may be used to train models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub training_use: Option<bool>,
    /// Where the data is processed: regions or ISO 3166 codes (`eu`, `bg`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub residency: Vec<String>,
    /// The highest data classification the resource accepts: `public`, `internal`,
    /// `confidential`, `restricted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<String>,
}

/// Who may see the resource.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Access {
    #[serde(default)]
    pub visibility: Visibility,
    /// Clients with a lower reputation do not see the resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_client_reputation: Option<f64>,
    /// Clients still too new to have a reputation (confidence below 0.3) see it anyway.
    #[serde(default = "yes")]
    pub allow_new_clients: bool,
}

fn yes() -> bool {
    true
}

/// `public` (also anonymous callers, when the broker allows), `platform` (anyone signed in) or
/// `organisation:<id>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum Visibility {
    Public,
    #[default]
    Platform,
    Organisation(String),
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Public => f.write_str("public"),
            Self::Platform => f.write_str("platform"),
            Self::Organisation(o) => write!(f, "organisation:{o}"),
        }
    }
}

impl FromStr for Visibility {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "public" => Ok(Self::Public),
            "platform" => Ok(Self::Platform),
            _ => match s.strip_prefix("organisation:") {
                Some(o) if !o.is_empty() && o.len() <= 128 => Ok(Self::Organisation(o.to_string())),
                _ => Err(format!("{s:?}: public, platform or organisation:<id>")),
            },
        }
    }
}

impl Serialize for Visibility {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Visibility {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Visibility {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Visibility".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type": "string", "pattern": "^(public|platform|organisation:.+)$", "examples": ["public", "platform", "organisation:org_123"]})
    }
}

/// Where a record came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Announced by a provider under a lease: soft state.
    #[default]
    Lease,
    /// From the catalogue file or the admin API: kept across restarts.
    Static,
    /// Imported from another description (reserved).
    Import,
}

/// A resource as the broker holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceRecord {
    pub id: String,
    pub provider_id: String,
    #[serde(flatten)]
    pub spec: ResourceSpec,
    /// The broker the record was announced on.
    pub origin: String,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    pub registered_at: Timestamp,
    pub updated_at: Timestamp,
}

impl ResourceSpec {
    /// Checks the resource on its own. Checks against the provider's inventory and the
    /// previous version are done where those are known.
    pub fn validate(&self, p: &mut Problems, path: &str, max_description: usize) {
        p.check(is_slug(&self.key, limits::SLUG_CHARS), format!("{path}.key"), "a slug (a-z, 0-9, - _ . : /)");
        p.check(
            is_known_or_other(&self.kind, &KINDS),
            format!("{path}.kind"),
            format_args!("one of {} or other:<slug>", KINDS.join(", ")),
        );
        p.check(is_slug(&self.name, limits::SLUG_CHARS), format!("{path}.name"), "a slug");
        if let Some(v) = &self.version {
            validate::text_len(p, &format!("{path}.version"), v, 64);
        }
        p.check(self.tags.len() <= limits::TAGS, format!("{path}.tags"), "too many tags");
        for (i, t) in self.tags.iter().enumerate() {
            p.check(is_slug(t, limits::SLUG_CHARS), format!("{path}.tags[{i}]"), "a slug");
        }
        self.text.validate(p, path, max_description);
        p.check(self.interfaces.len() <= limits::INTERFACES, format!("{path}.interfaces"), "too many interfaces");
        for (i, f) in self.interfaces.iter().enumerate() {
            let at = format!("{path}.interfaces[{i}]");
            p.check(
                is_known_or_other(&f.protocol, &PROTOCOLS),
                format!("{at}.protocol"),
                "a known protocol or other:<slug>",
            );
            check_endpoint(p, &format!("{at}.url"), &f.url, &f.protocol);
            if let Some(u) = &f.spec_url {
                validate::url(p, &format!("{at}.spec_url"), u);
            }
            if let Some(u) = &f.auth.obtain_url {
                validate::url(p, &format!("{at}.auth.obtain_url"), u);
            }
        }
        if let Some(m) = &self.model {
            if let Some(q) = m.parameters_b {
                p.check(q.is_finite() && q > 0.0, format!("{path}.model.parameters_b"), "a positive number");
            }
            for (i, x) in m.modalities.input.iter().chain(&m.modalities.output).enumerate() {
                p.check(is_slug(x, limits::SLUG_CHARS), format!("{path}.model.modalities[{i}]"), "a slug");
            }
        }
        let c = &self.capabilities;
        p.check(c.mcp_tools.len() <= limits::TOOLS, format!("{path}.capabilities.mcp_tools"), "too many tools");
        for (i, t) in c.mcp_tools.iter().enumerate() {
            validate::text_len(p, &format!("{path}.capabilities.mcp_tools[{i}].name"), &t.name, limits::SLUG_CHARS);
            validate::text_len(
                p,
                &format!("{path}.capabilities.mcp_tools[{i}].description"),
                &t.description,
                limits::NOTE_CHARS * 4,
            );
        }
        p.check(c.extra.len() <= limits::EXTRA, format!("{path}.capabilities.extra"), "too many entries");
        if serde_json::to_vec(&c.extra).map_or(0, |v| v.len()) > limits::EXTRA_BYTES {
            p.add(format!("{path}.capabilities.extra"), "too large");
        }
        for k in c.extra.keys() {
            p.check(is_slug(k, limits::SLUG_CHARS), format!("{path}.capabilities.extra.{k}"), "keys are slugs");
        }
        if let Some(pr) = &self.pricing {
            pr.validate(p, &format!("{path}.pricing"));
            crate::pricing::validate_notes(pr, p, &format!("{path}.pricing"));
        }
        self.availability.validate(p, &format!("{path}.availability"));
        for (i, r) in self.policy.residency.iter().enumerate() {
            p.check(is_slug(r, limits::SLUG_CHARS), format!("{path}.policy.residency[{i}]"), "a slug");
        }
        if let Some(m) = self.access.min_client_reputation {
            p.check((0.0..=1.0).contains(&m), format!("{path}.access.min_client_reputation"), "between 0 and 1");
        }
    }

    /// Checks that `runs_on` names items the inventory has.
    pub fn validate_runs_on(&self, inventory: Option<&crate::provider::Inventory>, p: &mut Problems, path: &str) {
        let Some(r) = &self.runs_on else { return };
        for (i, id) in r.hardware.iter().enumerate() {
            p.check(
                inventory.and_then(|inv| inv.hardware(id)).is_some(),
                format!("{path}.runs_on.hardware[{i}]"),
                format_args!("no hardware {id:?} in the inventory"),
            );
        }
        for (i, id) in r.software.iter().enumerate() {
            p.check(
                inventory.and_then(|inv| inv.software(id)).is_some(),
                format!("{path}.runs_on.software[{i}]"),
                format_args!("no software {id:?} in the inventory"),
            );
        }
    }

    pub fn title(&self) -> &str {
        self.text.title.as_deref().unwrap_or(&self.name)
    }

    /// The bytes this resource holds, for the broker's memory accounting.
    pub fn weight(&self) -> usize {
        // A close estimate: the serialised size, without the cost of serialising the text twice.
        self.text.weight()
            + serde_json::to_vec(&self.capabilities).map_or(0, |v| v.len())
            + 512 * (1 + self.interfaces.len())
    }
}

fn is_known_or_other(s: &str, known: &[&str]) -> bool {
    known.contains(&s) || s.strip_prefix("other:").is_some_and(|rest| is_slug(rest, limits::SLUG_CHARS))
}

fn check_endpoint(p: &mut Problems, path: &str, url: &str, protocol: &str) {
    // gRPC endpoints are often written without a scheme; everything else is a URL.
    if protocol == "grpc" && !url.contains("://") {
        p.check(
            !url.is_empty() && url.len() <= limits::URL_CHARS && !url.contains(char::is_whitespace),
            path,
            "host:port or a URL",
        );
    } else {
        validate::url(p, path, url);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_full_resource_reads_and_validates() {
        let r: ResourceSpec = serde_json::from_value(json!({
            "key": "llama-70b-eu",
            "kind": "model",
            "name": "llama-3.3-70b-instruct",
            "title": "Llama 3.3 70B on H100s in Sofia",
            "summary": "Llama 3.3 70B Instruct, fp8, served by vLLM on eight H100s.",
            "description": "## What it is\nAn OpenAI-compatible endpoint.\n",
            "examples": [{"title": "curl", "language": "bash", "code": "curl …"}],
            "tags": ["llm", "open-weights"],
            "interfaces": [{"protocol": "openai-chat", "url": "https://gpu.example/v1", "auth": {"scheme": "bearer"}}],
            "model": {"family": "llama", "context_window": 131072, "quantisation": "fp8", "parameters_b": 70},
            "capabilities": {"tools": true, "streaming": true},
            "runs_on": {"hardware": ["h100"], "software": ["vllm"]},
            "pricing": {"currency": "EUR", "periods": [{"valid_from": "2026-01-01T00:00:00Z", "rates": {"input": 0.4, "output": 0.6}}]},
            "availability": {"regions": ["bg"], "capacity": {"max_concurrency": 64}},
            "policy": {"data_retention": "none", "training_use": false, "residency": ["eu"]},
            "access": {"visibility": "public", "min_client_reputation": 0.4}
        }))
        .unwrap();
        let mut p = Problems::new();
        r.validate(&mut p, "resources[0]", 16 * 1024);
        assert!(p.is_empty(), "{p}");
        assert_eq!(r.title(), "Llama 3.3 70B on H100s in Sofia");
        assert!(r.access.allow_new_clients);

        let inv: crate::provider::Inventory =
            serde_json::from_value(json!({"hardware": [{"id": "h100", "type": "gpu"}]})).unwrap();
        let mut p = Problems::new();
        r.validate_runs_on(Some(&inv), &mut p, "resources[0]");
        assert_eq!(p.list(), ["resources[0].runs_on.software[0]: no software \"vllm\" in the inventory"]);
    }

    #[test]
    fn kinds_protocols_and_visibility() {
        assert!(is_known_or_other("other:weather", &KINDS));
        assert!(!is_known_or_other("weather", &KINDS));
        assert_eq!("organisation:o1".parse::<Visibility>().unwrap(), Visibility::Organisation("o1".into()));
        assert!("organisation:".parse::<Visibility>().is_err());
        assert_eq!(Visibility::Public.to_string(), "public");
    }

    #[test]
    fn unknown_fields_are_refused() {
        let r =
            serde_json::from_value::<ResourceSpec>(json!({"key": "a", "kind": "model", "name": "a", "colour": "red"}));
        assert!(r.is_err());
    }
}
