//! Providers and their credentials (concept sections 6.1 and 11.4).

use chrono::{DateTime, Utc};
use pacod_gw_dialects::canonical::{
    DEFAULT_CACHE_WRITE_TOKENS_AT, DEFAULT_CACHED_TOKENS_AT, DEFAULT_EXTRA_PARAMS, DEFAULT_REASONING_TOKENS_AT, owned,
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

/// Which of a provider's APIs managed mode speaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagedApi {
    /// The OpenAI-compatible API (or Anthropic's Messages API for the Anthropic family).
    #[default]
    OpenaiCompatible,
    /// Gemini's own API (`generateContent`), for the Gemini family: Google Search grounding,
    /// code execution, context caches, safety settings and thought summaries.
    Native,
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
    /// Which API managed mode speaks; `native` only for the Gemini family. Proxy mode relays
    /// both of Gemini's APIs whatever is set here.
    #[serde(default)]
    pub managed_api: ManagedApi,
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

/// Whether an OpenAI-compatible endpoint wants the reasoning of earlier assistant turns sent
/// back with them (in the field `reasoning_field` names).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EchoReasoning {
    /// Never sent back.
    #[default]
    Never,
    /// When the request has tools, every earlier assistant turn carries its reasoning, empty
    /// where it has none. DeepSeek refuses such a request otherwise.
    WithTools,
    /// Assistant turns carry their reasoning where they have some (Kimi K3 needs it; GLM keeps
    /// it with `thinking.clear_thinking: false`).
    Always,
}

/// How a provider's error codes are read. A `429` whose code means the credential's quota or
/// balance is used up is not retried (the call may still move on to another model); any other
/// `429` is a passing rate limit and is retried.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ErrorRules {
    /// Where the code is in an error body, as dotted paths tried in order. A number there is
    /// read as its decimal text; a leading `0.` addresses the first element of an array body.
    pub code_at: Vec<String>,
    /// Codes that mean the credential's quota or balance is used up.
    pub quota: Vec<String>,
}

impl Default for ErrorRules {
    fn default() -> Self {
        Self {
            code_at: vec!["error.code".into(), "error.type".into()],
            // OpenAI's own code for an exhausted quota.
            quota: vec!["insufficient_quota".into()],
        }
    }
}

impl ErrorRules {
    /// The provider's code in an error body, if it names one.
    pub fn code_of(&self, body: &serde_json::Value) -> Option<String> {
        self.code_at.iter().find_map(|path| {
            let v = path.split('.').try_fold(body, |v, key| match key.parse::<usize>() {
                Ok(i) if v.is_array() => v.get(i),
                _ => v.get(key),
            })?;
            match v {
                serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            }
        })
    }

    /// Whether an error body says the credential's quota or balance is used up.
    pub fn is_quota(&self, body: &[u8]) -> bool {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else { return false };
        self.code_of(&v).is_some_and(|c| self.quota.contains(&c))
    }
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
    /// Where `usage` reports tokens written to the prompt cache (Kimi): dotted paths, tried in
    /// order. They are part of `prompt_tokens`.
    pub cache_write_tokens_at: Vec<String>,
    /// Members an OpenAI-dialect caller may send that managed mode passes on to this provider.
    pub extra_params: Vec<String>,
    /// Whether earlier reasoning is sent back to the provider.
    pub echo_reasoning: EchoReasoning,
    /// Members managed mode adds to every request where the caller did not set them, merged
    /// one level deep: `{"thinking": {"clear_thinking": false}}` keeps GLM's reasoning across
    /// turns, `{"thinking": {"keep": "all"}}` Kimi's.
    pub request_fields: serde_json::Map<String, serde_json::Value>,
    /// How the provider's error codes are read.
    pub error_rules: ErrorRules,
    /// Tool calls carry Gemini 3's thought signatures in `extra_content.google.thought_signature`,
    /// which must come back with the call (Gemini's compatible endpoint).
    pub thought_signatures: bool,
    /// Reasoning tokens are left out of `completion_tokens` and counted only in `total_tokens`;
    /// the difference is billed as output (Gemini's compatible endpoint, recorded 2026-10).
    pub reasoning_outside_completion: bool,
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
            cache_write_tokens_at: owned(DEFAULT_CACHE_WRITE_TOKENS_AT),
            extra_params: owned(DEFAULT_EXTRA_PARAMS),
            echo_reasoning: EchoReasoning::Never,
            request_fields: serde_json::Map::new(),
            error_rules: ErrorRules::default(),
            thought_signatures: false,
            reasoning_outside_completion: false,
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
    /// Set where the provider takes its credential other than the family does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthScheme>,
    pub managed_api: ManagedApi,
    pub quirks: Quirks,
    /// What to know when setting the provider up, and what is still to be confirmed.
    pub notes: &'static str,
}

/// The built-in provider presets. Base URLs and quirks follow the providers' documentation as
/// of 2026-10 and the recorded fixtures where there are some (`crates/pacod-gw-testkit/fixtures`).
pub fn provider_presets() -> Vec<ProviderPreset> {
    let q = Quirks::default;
    let codes = |code_at: &str, quota: &[&str]| ErrorRules { code_at: vec![code_at.into()], quota: owned(quota) };
    // GLM's business codes for a used-up balance or package (docs.z.ai, error codes); 1302 and
    // 1305 are passing rate limits.
    let glm_quota = [
        "1113", "1308", "1309", "1310", "1311", "1313", "1314", "1315", "1316", "1317", "1318", "1319", "1320", "1321",
    ];
    let glm = Quirks {
        // Models before GLM-5.2 take the switch; later ones get `reasoning_param` from their
        // capability profile.
        reasoning_param: ReasoningParam::ThinkingToggle,
        echo_reasoning: EchoReasoning::Always,
        // Usage comes on the last chunk unasked; `stream_options` is not in GLM's schema.
        stream_usage: false,
        error_rules: codes("error.code", &glm_quota),
        ..q()
    };
    let kimi = Quirks {
        echo_reasoning: EchoReasoning::Always,
        error_rules: codes("error.type", &["exceeded_current_quota_error"]),
        ..q()
    };
    vec![
        ProviderPreset {
            name: "openai",
            display_name: "OpenAI",
            family: Family::Openai,
            base_url: "https://api.openai.com/v1",
            auth: None,
            managed_api: ManagedApi::OpenaiCompatible,
            quirks: q(),
            notes: "",
        },
        ProviderPreset {
            name: "anthropic",
            display_name: "Anthropic",
            family: Family::Anthropic,
            base_url: "https://api.anthropic.com",
            auth: None,
            managed_api: ManagedApi::OpenaiCompatible,
            quirks: Quirks { cache_control: true, thinking_object: true, ..q() },
            notes: "",
        },
        ProviderPreset {
            name: "gemini",
            display_name: "Google Gemini",
            family: Family::Gemini,
            base_url: "https://generativelanguage.googleapis.com/v1beta",
            auth: None,
            managed_api: ManagedApi::Native,
            quirks: q(),
            notes: "Managed mode speaks Gemini's own API (x-goog-api-key): Google Search, code execution and URL context as provider tools, context caches and safety settings through provider_options.gemini. Proxy mode relays the native API and the compatible endpoint. Gemini 3 needs each tool call's thought signature back; the gateway carries it through every interface.",
        },
        ProviderPreset {
            name: "gemini-openai",
            display_name: "Google Gemini (OpenAI-compatible)",
            family: Family::Gemini,
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
            auth: None,
            managed_api: ManagedApi::OpenaiCompatible,
            quirks: Quirks {
                normalise_history: true,
                thought_signatures: true,
                reasoning_outside_completion: true,
                // Google's thinking options travel in a member of that name.
                extra_params: owned(DEFAULT_EXTRA_PARAMS).into_iter().chain(["extra_body".to_owned()]).collect(),
                ..q()
            },
            notes: "Managed mode uses Gemini's OpenAI-compatible endpoint (Bearer key); proxy mode relays both APIs. Fewer features than the native API: no Google Search or code execution in managed mode.",
        },
        ProviderPreset {
            name: "glm",
            display_name: "GLM (z.ai)",
            family: Family::Openai,
            base_url: "https://api.z.ai/api/paas/v4",
            auth: None,
            managed_api: ManagedApi::OpenaiCompatible,
            quirks: glm.clone(),
            notes: "Pay-as-you-go endpoint, prices in USD. The Coding Plan (https://api.z.ai/api/coding/paas/v4) may only be used from the tools z.ai lists. GLM-5.2 and later take reasoning_effort; earlier models the thinking switch: choose the capability preset per model.",
        },
        ProviderPreset {
            name: "glm-cn",
            display_name: "GLM (bigmodel.cn)",
            family: Family::Openai,
            base_url: "https://open.bigmodel.cn/api/paas/v4",
            auth: None,
            managed_api: ManagedApi::OpenaiCompatible,
            quirks: glm,
            notes: "Zhipu's mainland endpoint, prices in CNY with input-length tiers. Needs a CNY exchange rate in the settings for reports in another currency.",
        },
        ProviderPreset {
            name: "deepseek",
            display_name: "DeepSeek",
            family: Family::Openai,
            base_url: "https://api.deepseek.com",
            auth: None,
            managed_api: ManagedApi::OpenaiCompatible,
            quirks: Quirks {
                // Thinking is on by default; reasoning_effort `none` switches it off.
                echo_reasoning: EchoReasoning::WithTools,
                ..q()
            },
            notes: "Models think by default. With tools, earlier reasoning must go back to the provider; the gateway does this on translated calls. Off-peak hours are billed at half price.",
        },
        ProviderPreset {
            name: "kimi",
            display_name: "Kimi (Moonshot)",
            family: Family::Openai,
            base_url: "https://api.moonshot.ai/v1",
            auth: None,
            managed_api: ManagedApi::OpenaiCompatible,
            quirks: kimi.clone(),
            notes: "International endpoint; keys are bound to it. Sampling parameters are fixed per model and must be left out.",
        },
        ProviderPreset {
            name: "kimi-cn",
            display_name: "Kimi (Moonshot, mainland)",
            family: Family::Openai,
            base_url: "https://api.moonshot.cn/v1",
            auth: None,
            managed_api: ManagedApi::OpenaiCompatible,
            quirks: kimi,
            notes: "Mainland endpoint, prices in CNY; keys are bound to it.",
        },
        ProviderPreset {
            name: "ollama",
            display_name: "Ollama",
            family: Family::Ollama,
            base_url: "http://localhost:11434/v1",
            auth: None,
            managed_api: ManagedApi::OpenaiCompatible,
            quirks: Quirks { reasoning_field: "reasoning".into(), ..q() },
            notes: "Managed mode uses the OpenAI-compatible endpoint under /v1; proxy mode relays the native /api paths. Must be reachable only from the gateway: allow its host in PGW_EGRESS_ALLOW_PRIVATE.",
        },
        ProviderPreset {
            name: "ollama-cloud",
            display_name: "Ollama Cloud",
            family: Family::Ollama,
            base_url: "https://ollama.com/v1",
            auth: Some(AuthScheme::Bearer),
            managed_api: ManagedApi::OpenaiCompatible,
            quirks: Quirks { reasoning_field: "reasoning".into(), ..q() },
            notes: "Ollama's hosted models, with an API key from ollama.com. Model names as in https://ollama.com/api/tags.",
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

    /// How proxy mode sends the credential on `path`: Gemini's OpenAI-compatible endpoint
    /// (`/v1beta/openai/…`) takes a Bearer token, its native API `x-goog-api-key`.
    pub fn proxy_auth_scheme(&self, path: &str) -> AuthScheme {
        self.auth.unwrap_or(match self.family {
            Family::Gemini if path.starts_with("/v1beta/openai/") => AuthScheme::Bearer,
            f => f.default_auth(),
        })
    }

    /// How managed mode sends the credential. Gemini's OpenAI-compatible endpoint takes a
    /// Bearer token, its native API `x-goog-api-key`.
    pub fn managed_auth_scheme(&self) -> AuthScheme {
        self.auth.unwrap_or(match (self.family, self.managed_api) {
            (Family::Gemini, ManagedApi::Native) => AuthScheme::XGoogApiKey,
            (Family::Gemini, ManagedApi::OpenaiCompatible) => AuthScheme::Bearer,
            (f, _) => f.default_auth(),
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
            managed_api: ManagedApi::default(),
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
        let native = Provider { managed_api: ManagedApi::Native, ..g.clone() };
        assert_eq!(native.managed_auth_scheme(), AuthScheme::XGoogApiKey);
        assert_eq!(
            provider("https://api.deepseek.com", Family::Openai).managed_url("chat/completions"),
            "https://api.deepseek.com/chat/completions"
        );
        // Presets parse as providers and have distinct names.
        let presets = provider_presets();
        let mut names: Vec<_> = presets.iter().map(|p| p.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), presets.len());
        assert_eq!(presets.len(), 11);
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
    fn error_codes_are_read_where_the_provider_puts_them() {
        let glm = ErrorRules { code_at: vec!["error.code".into()], quota: vec!["1113".into()] };
        assert!(glm.is_quota(br#"{"error":{"code":"1113","message":"balance"}}"#));
        assert!(!glm.is_quota(br#"{"error":{"code":"1302","message":"slow down"}}"#));
        assert!(!glm.is_quota(b"not json"));
        let numeric = ErrorRules { code_at: vec!["code".into()], quota: vec!["1113".into()] };
        assert!(numeric.is_quota(br#"{"code":1113,"message":"x"}"#), "GLM's schema also has a numeric code");
        let wrapped = ErrorRules { code_at: vec!["0.error.status".into()], quota: vec!["RESOURCE_EXHAUSTED".into()] };
        assert!(wrapped.is_quota(br#"[{"error":{"code":429,"status":"RESOURCE_EXHAUSTED"}}]"#));
        assert!(
            ErrorRules::default().is_quota(br#"{"error":{"type":"insufficient_quota","code":"insufficient_quota"}}"#)
        );
    }

    #[test]
    fn default_auth_per_family() {
        assert_eq!(provider("https://x", Family::Anthropic).auth_scheme(), AuthScheme::XApiKey);
        assert_eq!(provider("http://x", Family::Ollama).auth_scheme(), AuthScheme::None);
    }
}
