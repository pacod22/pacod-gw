//! Vendors by one id in every source. OpenRouter names a model's author (`x-ai/grok-4`), LiteLLM
//! the API that serves it (`litellm_provider: "xai"`); both become the vendor id here, which
//! the gateway maps its providers to.

/// A vendor whose models pacod-aggregator tracks, or that a source names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorInfo {
    pub id: &'static str,
    pub display_name: &'static str,
    /// The currency its own price list is in.
    pub currency: &'static str,
}

const fn v(id: &'static str, display_name: &'static str, currency: &'static str) -> VendorInfo {
    VendorInfo { id, display_name, currency }
}

/// The vendors with a vendor id of their own. A source may name others; they keep the source's
/// name as their id.
pub const VENDORS: &[VendorInfo] = &[
    v("openai", "OpenAI", "USD"),
    v("anthropic", "Anthropic", "USD"),
    v("google", "Google (Gemini API)", "USD"),
    v("google-vertex", "Google (Vertex AI)", "USD"),
    v("zai", "Z.ai (GLM)", "USD"),
    v("zhipu", "Zhipu AI (bigmodel.cn)", "CNY"),
    v("deepseek", "DeepSeek", "USD"),
    v("xai", "xAI", "USD"),
    v("moonshot", "Moonshot AI (Kimi)", "USD"),
    v("moonshot-cn", "Moonshot AI (Kimi, mainland)", "CNY"),
    v("mistral", "Mistral AI", "USD"),
    v("cohere", "Cohere", "USD"),
    v("voyage", "Voyage AI", "USD"),
    v("jina", "Jina AI", "USD"),
    v("groq", "Groq", "USD"),
    v("perplexity", "Perplexity", "USD"),
    v("qwen", "Alibaba Cloud (Qwen)", "USD"),
    v("minimax", "MiniMax", "USD"),
    v("meta", "Meta", "USD"),
];

/// The entry of a known vendor.
pub fn info(id: &str) -> Option<&'static VendorInfo> {
    VENDORS.iter().find(|v| v.id == id)
}

/// The vendor of an OpenRouter model, from the author part of its id (`x-ai/grok-4`). An author
/// not listed here keeps its own name.
pub fn openrouter_vendor(author: &str) -> String {
    let id = match author {
        "x-ai" => "xai",
        "z-ai" => "zai",
        "moonshotai" => "moonshot",
        "mistralai" => "mistral",
        "meta-llama" => "meta",
        "minimax" => "minimax",
        other => other,
    };
    id.to_owned()
}

/// The vendor of a LiteLLM entry, from its `litellm_provider`, for the vendors' own APIs only.
/// Resellers and clouds (`azure`, `bedrock`, `openrouter`, `together_ai`, …) answer `None`: their
/// prices are theirs, not the vendor's.
pub fn litellm_vendor(provider: &str) -> Option<&'static str> {
    Some(match provider {
        "openai" | "text-completion-openai" => "openai",
        "anthropic" => "anthropic",
        "gemini" => "google",
        p if p == "vertex_ai" || p.starts_with("vertex_ai-") => "google-vertex",
        "zai" => "zai",
        "deepseek" => "deepseek",
        "xai" => "xai",
        "moonshot" => "moonshot",
        "mistral" | "codestral" => "mistral",
        "cohere" | "cohere_chat" => "cohere",
        "voyage" => "voyage",
        "jina_ai" => "jina",
        "groq" => "groq",
        "perplexity" => "perplexity",
        "minimax" => "minimax",
        // DashScope serves other vendors' models too (`dashscope/deepseek-…`, `dashscope/glm-…`)
        // at its own prices: a host, like the clouds.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_name_vendors_one_way() {
        assert_eq!(openrouter_vendor("x-ai"), "xai");
        assert_eq!(openrouter_vendor("anthropic"), "anthropic");
        assert_eq!(openrouter_vendor("nousresearch"), "nousresearch");
        assert_eq!(litellm_vendor("gemini"), Some("google"));
        assert_eq!(litellm_vendor("vertex_ai-language-models"), Some("google-vertex"));
        assert_eq!(litellm_vendor("azure"), None);
        assert_eq!(litellm_vendor("bedrock_converse"), None);
        assert!(info("moonshot-cn").is_some_and(|v| v.currency == "CNY"));
    }
}
