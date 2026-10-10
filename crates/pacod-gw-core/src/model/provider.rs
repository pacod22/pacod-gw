//! Providers and their credentials (concept sections 6.1 and 11.4).

use chrono::{DateTime, Utc};
use pacod_gw_dialects::canonical::{
    DEFAULT_CACHED_TOKENS_AT, DEFAULT_EXTRA_PARAMS, DEFAULT_REASONING_TOKENS_AT, owned,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Doc, Status};
use crate::crypto::envelope::Sealed;

/// The dialect a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    /// OpenAI and everything that offers an OpenAI-compatible API.
    Openai,
    Anthropic,
    /// Google's native Gemini API. Proxy mode only in the first release.
    Gemini,
    /// Ollama's native API. Proxy mode only; managed mode uses its OpenAI-compatible endpoint.
    Ollama,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Openai => "openai",
            Family::Anthropic => "anthropic",
            Family::Gemini => "gemini",
            Family::Ollama => "ollama",
        }
    }

    /// How this family's API expects the credential.
    pub fn default_auth(self) -> AuthScheme {
        match self {
            Family::Openai => AuthScheme::Bearer,
            Family::Anthropic => AuthScheme::XApiKey,
            Family::Gemini => AuthScheme::XGoogApiKey,
            Family::Ollama => AuthScheme::None,
        }
    }

    /// Path prefixes proxy mode relays for this family when the provider sets none.
    pub fn default_proxy_paths(self) -> &'static [&'static str] {
        match self {
            Family::Openai => &["/"],
            Family::Anthropic => &["/v1/"],
            Family::Gemini => &["/v1beta/", "/v1/"],
            Family::Ollama => &["/api/", "/v1/"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `x-api-key: <key>`
    XApiKey,
    /// `x-goog-api-key: <key>`
    XGoogApiKey,
    /// No credential, for example a local model server.
    None,
}

/// An LLM provider endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Provider {
    /// Slug. Appears in proxy URLs: `/proxy/{name}/…`.
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    pub family: Family,
    /// Where managed mode sends requests, for example `https://api.openai.com/v1`.
    pub base_url: String,
    /// Where proxy mode sends the path after `/proxy/{name}`. Defaults to the origin of `base_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_base_url: Option<String>,
    /// Defaults to the family's scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthScheme>,
    /// Path prefixes proxy mode relays. Empty uses the family's defaults.
    #[serde(default)]
    pub proxy_paths: Vec<String>,
    #[serde(default = "default_timeout")]
    pub default_timeout_seconds: u32,
    /// How this endpoint departs from the plain OpenAI-compatible behaviour.
    #[serde(default)]
    pub quirks: Quirks,
    #[serde(default)]
    pub status: Status,
}

fn default_timeout() -> u32 {
    180
}

/// The request field that steers a model's reasoning on an OpenAI-compatible endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningParam {
    /// `reasoning_effort: "low" | "medium" | …`
    #[default]
    ReasoningEffort,
    /// `thinking: {"type": "enabled" | "disabled"}`: on or off, without levels.
    ThinkingToggle,
    /// The endpoint has no such field; reasoning settings are not sent.
    None,
}

/// How an OpenAI-compatible endpoint departs from the plain behaviour (concept section 8.2).
/// This is data, so that a new provider needs configuration and no release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Quirks {
    /// Streams report usage when asked with `stream_options.include_usage`.
    pub stream_usage: bool,
    /// `cache_control` markers on content blocks are accepted. Otherwise managed mode removes them.
    pub cache_control: bool,
    /// An Anthropic-style top-level `thinking` object is accepted as it is. Otherwise managed
    /// mode maps it to `reasoning` and removes it.
    pub thinking_object: bool,
    pub reasoning_param: ReasoningParam,
    /// The provider rejects consecutive turns of one role and a leading assistant turn.
    /// Managed mode merges and drops them before forwarding.
    pub normalise_history: bool,
    /// The field of a message or stream delta that carries reasoning text.
    pub reasoning_field: String,
    /// Top-level request fields the provider rejects. Managed mode removes them.
    pub drop_fields: Vec<String>,
    /// Where `usage` reports cached input tokens: dotted paths, tried in order.
    pub cached_tokens_at: Vec<String>,
    /// Where `usage` reports reasoning tokens: dotted paths, tried in order.
    pub reasoning_tokens_at: Vec<String>,
    /// Members an OpenAI-dialect caller may send that managed mode passes on to this provider.
    pub extra_params: Vec<String>,
}

impl Default for Quirks {
    fn default() -> Self {
        Self {
            stream_usage: true,
            cache_control: false,
            thinking_object: false,
            reasoning_param: ReasoningParam::ReasoningEffort,
            normalise_history: false,
            reasoning_field: "reasoning_content".into(),
            drop_fields: Vec::new(),
            cached_tokens_at: owned(DEFAULT_CACHED_TOKENS_AT),
            reasoning_tokens_at: owned(DEFAULT_REASONING_TOKENS_AT),
            extra_params: owned(DEFAULT_EXTRA_PARAMS),
        }
    }
}

/// A starting point for one of the providers the gateway knows (concept section 8.1).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProviderPreset {
    pub name: &'static str,
    pub display_name: &'static str,
    pub family: Family,
    pub base_url: &'static str,
    pub quirks: Quirks,
    /// What is still to be confirmed against the provider.
    pub notes: &'static str,
}

/// The built-in provider presets. Base URLs and quirks are starting values.
pub fn provider_presets() -> Vec<ProviderPreset> {
    let q = Quirks::default;
    vec![
        ProviderPreset {
            name: "openai",
            display_name: "OpenAI",
            family: Family::Openai,
            base_url: "https://api.openai.com/v1",
            quirks: q(),
            notes: "",
        },
        ProviderPreset {
            name: "anthropic",
            display_name: "Anthropic",
            family: Family::Anthropic,
            base_url: "https://api.anthropic.com",
            quirks: Quirks { cache_control: true, thinking_object: true, ..q() },
            notes: "",
        },
        ProviderPreset {
            name: "gemini",
            display_name: "Google Gemini",
            family: Family::Gemini,
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
            quirks: Quirks { normalise_history: true, ..q() },
            notes: "Managed mode uses the OpenAI-compatible endpoint; proxy mode relays the native API. Reasoning and cached-token reporting on the compatible endpoint are not verified.",
        },
        ProviderPreset {
            name: "glm",
            display_name: "GLM (Zhipu / z.ai)",
            family: Family::Openai,
            base_url: "https://api.z.ai/api/paas/v4",
            quirks: Quirks { reasoning_param: ReasoningParam::ThinkingToggle, ..q() },
            notes: "The thinking switch and the reasoning field are not verified against the live API.",
        },
        ProviderPreset {
            name: "deepseek",
            display_name: "DeepSeek",
            family: Family::Openai,
            base_url: "https://api.deepseek.com",
            quirks: Quirks { cached_tokens_at: vec!["prompt_cache_hit_tokens".into()], ..q() },
            notes: "Cache-hit token fields are read from `prompt_cache_hit_tokens`; not verified against the live API.",
        },
        ProviderPreset {
            name: "kimi",
            display_name: "Kimi (Moonshot)",
            family: Family::Openai,
            base_url: "https://api.moonshot.ai/v1",
            quirks: q(),
            notes: "Reasoning and cache fields are not verified against the live API.",
        },
        ProviderPreset {
            name: "ollama",
            display_name: "Ollama",
            family: Family::Ollama,
            base_url: "http://localhost:11434/v1",
            quirks: Quirks { reasoning_param: ReasoningParam::None, ..q() },
            notes: "Managed mode uses the OpenAI-compatible endpoint under /v1; proxy mode relays the native /api paths. Must be reachable only from the gateway.",
        },
    ]
}

impl Doc for Provider {
    const KIND: &'static str = "provider";
    const PREFIX: &'static str = "prv";
}

impl Provider {
    pub fn auth_scheme(&self) -> AuthScheme {
        self.auth.unwrap_or(self.family.default_auth())
    }

    /// How managed mode sends the credential. Gemini's OpenAI-compatible endpoint takes a
    /// Bearer token, unlike its native API.
    pub fn managed_auth_scheme(&self) -> AuthScheme {
        self.auth.unwrap_or(match self.family {
            Family::Gemini => AuthScheme::Bearer,
            f => f.default_auth(),
        })
    }

    /// The URL of an endpoint under `base_url`, for managed mode.
    pub fn managed_url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path.trim_start_matches('/'))
    }

    /// The URL proxy mode prepends to the relayed path, without a trailing slash.
    pub fn proxy_origin(&self) -> Result<String, url::ParseError> {
        if let Some(p) = &self.proxy_base_url {
            return Ok(p.trim_end_matches('/').to_owned());
        }
        let u = url::Url::parse(&self.base_url)?;
        Ok(u.origin().ascii_serialization())
    }

    /// Whether proxy mode may relay `path` (which starts with `/`).
    pub fn allows_proxy_path(&self, path: &str) -> bool {
        if self.proxy_paths.is_empty() {
            self.family.default_proxy_paths().iter().any(|p| path.starts_with(p))
        } else {
            self.proxy_paths.iter().any(|p| path.starts_with(p.as_str()))
        }
    }
}

/// Who a credential is for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "id", rename_all = "snake_case")]
pub enum CredentialScope {
    /// The default for every workspace.
    Global,
    /// An organisation's own key.
    Organisation(String),
    /// A workspace's own key.
    Workspace(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    /// Used for new requests.
    #[default]
    Current,
    /// Replaced by a newer credential; kept until `retire_at`.
    Retiring,
    Disabled,
}

/// A provider key. The secret is stored encrypted and never returned by the API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Credential {
    pub provider_id: String,
    pub scope: CredentialScope,
    #[serde(default)]
    pub label: String,
    pub sealed: Sealed,
    /// A label that identifies the secret without revealing it.
    pub fingerprint: String,
    #[serde(default)]
    pub state: CredentialState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retire_at: Option<DateTime<Utc>>,
}

impl Doc for Credential {
    const KIND: &'static str = "credential";
    const PREFIX: &'static str = "cred";
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(base: &str, family: Family) -> Provider {
        Provider {
            name: "p".into(),
            display_name: String::new(),
            family,
            base_url: base.into(),
            proxy_base_url: None,
            auth: None,
            proxy_paths: vec![],
            default_timeout_seconds: 180,
            quirks: Quirks::default(),
            status: Status::Active,
        }
    }

    #[test]
    fn managed_urls_and_auth() {
        let g = provider("https://generativelanguage.googleapis.com/v1beta/openai/", Family::Gemini);
        assert_eq!(
            g.managed_url("/chat/completions"),
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
        );
        assert_eq!(g.managed_auth_scheme(), AuthScheme::Bearer);
        assert_eq!(g.auth_scheme(), AuthScheme::XGoogApiKey);
        assert_eq!(
            provider("https://api.deepseek.com", Family::Openai).managed_url("chat/completions"),
            "https://api.deepseek.com/chat/completions"
        );
        // Presets parse as providers and have distinct names.
        let presets = provider_presets();
        let mut names: Vec<_> = presets.iter().map(|p| p.name).collect();
        names.dedup();
        assert_eq!(names.len(), 7);
        assert!(presets.iter().all(|p| url::Url::parse(p.base_url).is_ok()));
    }

    #[test]
    fn proxy_origin_is_the_origin_of_the_base_url() {
        assert_eq!(
            provider("https://api.openai.com/v1", Family::Openai).proxy_origin().unwrap(),
            "https://api.openai.com"
        );
        assert_eq!(
            provider("https://api.z.ai/api/paas/v4", Family::Openai).proxy_origin().unwrap(),
            "https://api.z.ai"
        );
        assert_eq!(provider("http://ollama:11434/v1", Family::Ollama).proxy_origin().unwrap(), "http://ollama:11434");
        let mut p = provider("https://api.openai.com/v1", Family::Openai);
        p.proxy_base_url = Some("https://eu.example/".into());
        assert_eq!(p.proxy_origin().unwrap(), "https://eu.example");
    }

    #[test]
    fn proxy_path_allow_list() {
        let a = provider("https://api.anthropic.com", Family::Anthropic);
        assert!(a.allows_proxy_path("/v1/messages"));
        assert!(!a.allows_proxy_path("/admin"));
        let mut o = provider("https://api.openai.com/v1", Family::Openai);
        assert!(o.allows_proxy_path("/v1/chat/completions"));
        o.proxy_paths = vec!["/v1/chat/".into()];
        assert!(o.allows_proxy_path("/v1/chat/completions"));
        assert!(!o.allows_proxy_path("/v1/files"));
    }

    #[test]
    fn default_auth_per_family() {
        assert_eq!(provider("https://x", Family::Anthropic).auth_scheme(), AuthScheme::XApiKey);
        assert_eq!(provider("http://x", Family::Ollama).auth_scheme(), AuthScheme::None);
    }
}
