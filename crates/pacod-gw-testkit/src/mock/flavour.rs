//! Provider emulations: `/emulate/<provider>/…` answers as that provider does.
//!
//! The plain mock speaks OpenAI's own dialect. Under an emulation prefix it takes on what sets
//! one provider apart: the field that carries reasoning, whether a model thinks by default, the
//! shape and place of usage, the error envelope, and the requests the provider refuses. The
//! shapes follow the recorded fixtures in `crates/pacod-gw-testkit/fixtures/<provider>/` where
//! they exist and the provider's documentation otherwise; each knob says which.
//!
//! The prefix is stripped before routing, and the rest of the path is matched by its suffix
//! (`…/chat/completions`, `…/models`), so a provider set up with its real base path works:
//! `{mock}/emulate/deepseek` (DeepSeek has no `/v1`), `{mock}/emulate/glm/api/paas/v4`,
//! `{mock}/emulate/gemini-openai/v1beta/openai`. Native Gemini under `/emulate/gemini/v1beta/…`
//! checks thought signatures like Gemini 3.

use serde_json::{Value, json};

use super::server::ApiError;

/// The provider an emulation prefix names. `OpenAi` is the plain mock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavour {
    OpenAi,
    DeepSeek,
    Glm,
    Kimi,
    /// Gemini's OpenAI-compatible endpoint.
    GeminiOpenai,
    /// Gemini's native API, with Gemini 3's thought-signature checks.
    Gemini,
    /// Gemini on Vertex AI: the native API below a project and location (or the project-less
    /// express path), with a Bearer token the mock's token endpoint issued.
    Vertex,
    /// Ollama's OpenAI-compatible endpoint.
    Ollama,
    /// xAI's Chat Completions (docs.x.ai; nothing recorded).
    Xai,
}

impl Flavour {
    /// Splits `/emulate/<name>/rest` into the flavour and `/rest`. Any other path is the plain
    /// mock. An unknown name is the plain mock too, with the path unchanged.
    pub fn strip(path: &str) -> (Flavour, &str) {
        let Some(rest) = path.strip_prefix("/emulate/") else { return (Flavour::OpenAi, path) };
        let (name, tail) = rest.split_once('/').map_or((rest, ""), |(n, t)| (n, t));
        let flavour = match name {
            "deepseek" => Flavour::DeepSeek,
            "glm" => Flavour::Glm,
            "kimi" => Flavour::Kimi,
            "gemini-openai" => Flavour::GeminiOpenai,
            "gemini" => Flavour::Gemini,
            "vertex" => Flavour::Vertex,
            "ollama" => Flavour::Ollama,
            "xai" => Flavour::Xai,
            _ => return (Flavour::OpenAi, path),
        };
        // `tail` lost its leading slash in the split; the original keeps it.
        (flavour, &path[path.len() - tail.len() - 1..])
    }

    /// The field of a message or stream delta that carries reasoning text. Ollama's compatible
    /// endpoint uses `reasoning` (source: ollama/openai/openai.go); the others `reasoning_content`.
    pub fn reasoning_field(self) -> &'static str {
        match self {
            Flavour::Ollama => "reasoning",
            _ => "reasoning_content",
        }
    }

    /// Whether the model reasons for this request. DeepSeek and GLM think unless told not to,
    /// Kimi K3 always does; Ollama only when an effort other than `none` is asked for.
    pub fn thinks(self, request: &Value, scripted: bool) -> bool {
        let disabled = request.pointer("/thinking/type").and_then(Value::as_str) == Some("disabled");
        let effort = request.get("reasoning_effort").and_then(Value::as_str);
        match self {
            Flavour::DeepSeek => !disabled && effort != Some("none"),
            Flavour::Glm => !disabled,
            Flavour::Kimi => !disabled,
            Flavour::Ollama => scripted || effort.is_some_and(|e| e != "none"),
            Flavour::OpenAi | Flavour::GeminiOpenai | Flavour::Gemini | Flavour::Vertex | Flavour::Xai => scripted,
        }
    }

    /// Where a stream reports usage.
    pub fn stream_usage(self) -> StreamUsage {
        match self {
            // On the chunk with the finish reason, asked or not (recorded: deepseek/chat-stream*).
            Flavour::DeepSeek => StreamUsage::OnFinishAlways,
            // The same at GLM (recorded: glm/chat-stream*).
            Flavour::Glm => StreamUsage::OnFinishAlways,
            // In `choices[0].usage` on the finish chunk, and in a chunk of its own when asked
            // (platform.kimi.ai, streaming guide).
            Flavour::Kimi => StreamUsage::InChoiceAndWhenAsked,
            _ => StreamUsage::OwnChunkWhenAsked,
        }
    }

    /// `usage` of a whole answer, from the neutral counts.
    pub fn usage_body(self, u: &super::reply::Usage) -> Value {
        let cached = u.cache_read;
        let completion = u.output + u.reasoning;
        match self {
            Flavour::DeepSeek => {
                let prompt = u.input + cached;
                json!({
                    "prompt_tokens": prompt,
                    "completion_tokens": completion,
                    "total_tokens": prompt + completion,
                    "prompt_tokens_details": { "cached_tokens": cached },
                    "completion_tokens_details": { "reasoning_tokens": u.reasoning },
                    "prompt_cache_hit_tokens": cached,
                    "prompt_cache_miss_tokens": u.input,
                })
            }
            Flavour::Glm => {
                let prompt = u.input + cached;
                json!({
                    "completion_tokens": completion,
                    "completion_tokens_details": { "reasoning_tokens": u.reasoning },
                    "prompt_tokens": prompt,
                    "prompt_tokens_details": { "cached_tokens": cached },
                    "total_tokens": prompt + completion,
                })
            }
            // Reasoning is in `total_tokens` only, not in `completion_tokens` (recorded:
            // gemini-openai/chat).
            Flavour::GeminiOpenai => {
                let prompt = u.input + cached;
                let mut body = json!({
                    "completion_tokens": u.output,
                    "prompt_tokens": prompt,
                    "total_tokens": prompt + u.output + u.reasoning,
                });
                if cached > 0 {
                    body["prompt_tokens_details"] = json!({ "cached_tokens": cached });
                }
                body
            }
            Flavour::Kimi => {
                let prompt = u.input + cached + u.cache_write;
                json!({
                    "prompt_tokens": prompt,
                    "completion_tokens": completion,
                    "total_tokens": prompt + completion,
                    "cached_tokens": cached,
                    "prompt_tokens_details": { "cached_tokens": cached, "cache_write_tokens": u.cache_write },
                })
            }
            Flavour::Ollama => {
                let prompt = u.input + cached;
                let mut body = json!({ "prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion });
                if cached > 0 {
                    body["prompt_tokens_details"] = json!({ "cached_tokens": cached });
                }
                body
            }
            // Reasoning tokens apart and outside `completion_tokens`, inside the total
            // (LiteLLM's xai transform).
            Flavour::Xai => {
                let prompt = u.input + cached;
                json!({
                    "prompt_tokens": prompt,
                    "completion_tokens": u.output,
                    "total_tokens": prompt + u.output + u.reasoning,
                    "prompt_tokens_details": { "cached_tokens": cached },
                    "completion_tokens_details": { "reasoning_tokens": u.reasoning },
                })
            }
            Flavour::OpenAi | Flavour::Gemini | Flavour::Vertex => super::openai::openai_usage(u),
        }
    }

    /// The short usage Kimi writes into `choices[0].usage`: no cache fields.
    pub fn choice_usage(u: &super::reply::Usage) -> Value {
        let prompt = u.input + u.cache_read + u.cache_write;
        let completion = u.output + u.reasoning;
        json!({ "prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion })
    }

    /// The model list of `GET …/models`.
    pub fn models(self) -> Value {
        let ids: &[&str] = match self {
            Flavour::DeepSeek => &["deepseek-flash", "deepseek-v4-pro"],
            Flavour::Glm => &["glm-5.3", "glm-4.7"],
            Flavour::Kimi => &["kimi-k3", "kimi-k2.6"],
            // Gemini's compatible endpoint names models with the native prefix.
            Flavour::GeminiOpenai | Flavour::Gemini | Flavour::Vertex => {
                &["models/gemini-3.8-flash", "models/gemini-3.5-flash-lite"]
            }
            Flavour::Ollama => &["qwen3:8b", "gpt-oss:20b"],
            Flavour::Xai => &["grok-4", "grok-3-mini"],
            Flavour::OpenAi => &["gpt-4o", "gpt-4o-mini", "text-embedding-3-small", "tts-1"],
        };
        let owner = match self {
            Flavour::Ollama => "library",
            Flavour::OpenAi => "mock",
            _ => "system",
        };
        let data: Vec<Value> = ids
            .iter()
            .map(|id| {
                let mut m = json!({ "id": id, "object": "model", "created": super::reply::CREATED, "owned_by": owner });
                // DeepSeek lists what its models can do (recorded: deepseek/models).
                if self == Flavour::DeepSeek {
                    let images = id.contains("flash");
                    m["context_window"] = json!(1_048_576);
                    m["max_output_tokens"] = json!(393_216);
                    m["input_modalities"] = if images { json!(["text", "image"]) } else { json!(["text"]) };
                    m["effort"] = json!({ "supported_levels": ["low", "high", "max"], "default_level": "high" });
                }
                m
            })
            .collect();
        json!({ "object": "list", "data": data })
    }

    /// The error body in this provider's envelope. `code` is a scripted business code.
    pub fn error_body(self, status: u16, message: &str, code: Option<&str>) -> Option<Value> {
        Some(match self {
            Flavour::Glm => {
                // String codes (docs.z.ai, error codes). 1302 is the retryable rate limit.
                let default = match status {
                    401 => "1000",
                    404 => "1211",
                    429 => "1302",
                    s if s >= 500 => "500",
                    _ => "1214",
                };
                json!({ "error": { "code": code.unwrap_or(default), "message": message } })
            }
            Flavour::Kimi => {
                let default = match status {
                    401 => "invalid_authentication_error",
                    403 => "permission_denied_error",
                    404 => "resource_not_found_error",
                    429 => "rate_limit_reached_error",
                    s if s >= 500 => "engine_overloaded_error",
                    _ => "invalid_request_error",
                };
                json!({ "error": { "type": code.unwrap_or(default), "message": message } })
            }
            // Wrapped in an array on chat completions, as observed (google.rpc.Status inside).
            Flavour::GeminiOpenai => json!([super::gemini::error_body(status, message)]),
            Flavour::Ollama => {
                let kind = match status {
                    400 => "invalid_request_error",
                    404 => "not_found_error",
                    _ => "api_error",
                };
                json!({ "error": { "message": message, "type": kind, "param": null, "code": null } })
            }
            Flavour::Gemini | Flavour::Vertex => super::gemini::error_body(status, message),
            // `{"code": "…", "error": "…"}` (secondary sources; not recorded).
            Flavour::Xai => {
                let default = match status {
                    401 | 403 => "unauthenticated",
                    404 => "not-found",
                    429 => "resource-exhausted",
                    s if s >= 500 => "internal",
                    _ => "invalid-argument",
                };
                json!({ "code": code.unwrap_or(default), "error": message })
            }
            Flavour::OpenAi | Flavour::DeepSeek => return None,
        })
    }

    /// What this provider refuses in a chat-completions request, checked before answering.
    pub fn check(self, request: &Value, thinking: bool) -> Result<(), ApiError> {
        let tools = request.get("tools").and_then(Value::as_array).is_some_and(|t| !t.is_empty());
        let forced = match request.get("tool_choice") {
            Some(Value::String(s)) => s == "required",
            Some(Value::Object(_)) => true,
            _ => false,
        };
        let messages = request.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
        match self {
            Flavour::DeepSeek => {
                if thinking && forced {
                    return Err(ApiError::bad_request(
                        "deepseek: `required` and named tool choices are not supported in thinking mode",
                    ));
                }
                // With tools, the reasoning of every earlier assistant turn must come back. The
                // documentation says so; deepseek-flash took a turn without it when recorded
                // (deepseek/tools-2-bare), so the emulation is the stricter of the two.
                if thinking && tools {
                    for (i, m) in messages.iter().enumerate() {
                        if m.get("role").and_then(Value::as_str) == Some("assistant")
                            && m.get("reasoning_content").and_then(Value::as_str).is_none()
                        {
                            return Err(ApiError::bad_request(format!(
                                "deepseek: the reasoning_content of the assistant message at index {i} must be passed back"
                            )));
                        }
                    }
                }
            }
            Flavour::Kimi => {
                // Fixed samplers: any value but the model's own is refused.
                for (name, fixed) in
                    [("temperature", 1.0), ("top_p", 0.95), ("presence_penalty", 0.0), ("frequency_penalty", 0.0)]
                {
                    if let Some(v) = request.get(name).and_then(Value::as_f64)
                        && (v - fixed).abs() > f64::EPSILON
                    {
                        return Err(ApiError::bad_request(format!(
                            "kimi: invalid {name}: only {fixed} is allowed for this model"
                        )));
                    }
                }
                if request.get("n").and_then(Value::as_u64).is_some_and(|n| n != 1) {
                    return Err(ApiError::bad_request("kimi: invalid n: only 1 is allowed for this model"));
                }
                if thinking && matches!(request.get("tool_choice"), Some(Value::Object(_))) {
                    return Err(ApiError::bad_request(
                        "kimi: tool_choice 'specified' is incompatible with thinking enabled",
                    ));
                }
            }
            Flavour::GeminiOpenai => {
                // Gemini 3: the first tool call of each step of the current turn carries its
                // thought signature back.
                let start = messages.iter().rposition(|m| m.get("role").and_then(Value::as_str) == Some("user"));
                for (i, m) in messages.iter().enumerate().skip(start.map_or(0, |s| s + 1)) {
                    if let Some(first) = m.get("tool_calls").and_then(Value::as_array).and_then(|c| c.first())
                        && first.pointer("/extra_content/google/thought_signature").and_then(Value::as_str).is_none()
                    {
                        return Err(ApiError::bad_request(format!(
                            "Function call is missing a thought_signature in functionCall parts. Message {i}."
                        )));
                    }
                }
                if messages.last().and_then(|m| m.get("role")).and_then(Value::as_str) == Some("assistant") {
                    return Err(ApiError::bad_request("gemini: a request may not end with a model turn"));
                }
            }
            Flavour::Xai => {
                // Reasoning models refuse stop and the penalties; grok-4 an effort as well.
                let model = request.get("model").and_then(Value::as_str).unwrap_or_default();
                let reasons = !model.contains("non-reasoning")
                    && (model.starts_with("grok-4") || model.starts_with("grok-code") || model.contains("mini"));
                let mut refused = vec!["stop", "presence_penalty", "frequency_penalty"];
                if !model.contains("mini") {
                    refused.push("reasoning_effort");
                }
                if reasons && let Some(name) = refused.iter().find(|f| request.get(**f).is_some()) {
                    return Err(ApiError::bad_request(format!("Argument not supported on this model: {name}")));
                }
            }
            Flavour::OpenAi | Flavour::Glm | Flavour::Ollama | Flavour::Gemini | Flavour::Vertex => {}
        }
        Ok(())
    }

    /// Gemini native: the first function call of each model step in the current turn must carry
    /// its signature, and a request may not end with a model turn.
    pub fn check_gemini(contents: &[Value]) -> Result<(), ApiError> {
        let is_user_text = |c: &Value| {
            matches!(c.get("role").and_then(Value::as_str), None | Some("user"))
                && c["parts"].as_array().is_some_and(|p| p.iter().any(|p| p.get("text").is_some()))
        };
        let start = contents.iter().rposition(is_user_text).map_or(0, |s| s + 1);
        for (i, c) in contents.iter().enumerate().skip(start) {
            if c.get("role").and_then(Value::as_str) != Some("model") {
                continue;
            }
            let first_call = c["parts"].as_array().and_then(|p| p.iter().find(|p| p.get("functionCall").is_some()));
            if let Some(part) = first_call
                && part.get("thoughtSignature").and_then(Value::as_str).is_none()
            {
                return Err(ApiError::bad_request(format!(
                    "Function call is missing a thought_signature in functionCall parts. This is required for tools to work correctly, and missing thought_signature may lead to degraded model performance. Content {i}."
                )));
            }
        }
        if contents.last().and_then(|c| c.get("role")).and_then(Value::as_str) == Some("model") {
            return Err(ApiError::bad_request("A request may not end with a model turn."));
        }
        Ok(())
    }
}

/// Where a chat-completions stream reports usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamUsage {
    /// A chunk of its own with empty `choices`, when `stream_options.include_usage` is set.
    OwnChunkWhenAsked,
    /// On the chunk that carries the finish reason, always.
    OnFinishAlways,
    /// In `choices[0].usage` of the finish chunk always, and a chunk of its own when asked.
    InChoiceAndWhenAsked,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value Google documents for a history whose thought signatures were lost; any
    /// signature passes the mock's check.
    const SKIP_SIGNATURE: &str = "skip_thought_signature_validator";

    #[test]
    fn prefixes_are_stripped() {
        assert_eq!(Flavour::strip("/emulate/deepseek/chat/completions"), (Flavour::DeepSeek, "/chat/completions"));
        assert_eq!(
            Flavour::strip("/emulate/glm/api/paas/v4/chat/completions"),
            (Flavour::Glm, "/api/paas/v4/chat/completions")
        );
        assert_eq!(Flavour::strip("/emulate/gemini/v1beta/models/m:generateContent").0, Flavour::Gemini);
        assert_eq!(Flavour::strip("/v1/chat/completions"), (Flavour::OpenAi, "/v1/chat/completions"));
        assert_eq!(Flavour::strip("/emulate/nobody/x"), (Flavour::OpenAi, "/emulate/nobody/x"));
    }

    #[test]
    fn deepseek_wants_reasoning_back_with_tools() {
        let tools = json!([{ "type": "function", "function": { "name": "f", "parameters": {} } }]);
        let bare = json!({ "tools": tools, "messages": [
            { "role": "user", "content": "x" },
            { "role": "assistant", "content": null, "tool_calls": [] },
            { "role": "tool", "tool_call_id": "a", "content": "y" } ] });
        assert!(Flavour::DeepSeek.check(&bare, true).is_err());
        assert!(Flavour::DeepSeek.check(&bare, false).is_ok(), "only in thinking mode");
        let mut kept = bare.clone();
        kept["messages"][1]["reasoning_content"] = json!("because");
        assert!(Flavour::DeepSeek.check(&kept, true).is_ok());
    }

    #[test]
    fn gemini_wants_signatures_in_the_current_turn() {
        let call = |sig: Option<&str>| {
            let mut part = json!({ "functionCall": { "name": "f", "args": {} } });
            if let Some(s) = sig {
                part["thoughtSignature"] = json!(s);
            }
            json!({ "role": "model", "parts": [part] })
        };
        let user = json!({ "role": "user", "parts": [{ "text": "q" }] });
        let result = json!({ "role": "user", "parts": [{ "functionResponse": { "name": "f", "response": {} } }] });
        assert!(Flavour::check_gemini(&[user.clone(), call(None), result.clone()]).is_err());
        assert!(Flavour::check_gemini(&[user.clone(), call(Some("c2ln")), result.clone()]).is_ok());
        assert!(Flavour::check_gemini(&[user.clone(), call(Some(SKIP_SIGNATURE)), result.clone()]).is_ok());
        // An earlier turn is not checked.
        assert!(Flavour::check_gemini(&[user.clone(), call(None), result, user.clone()]).is_ok());
        assert!(Flavour::check_gemini(&[user, call(Some("x"))]).is_err(), "ends with a model turn");
    }

    #[test]
    fn kimi_samplers_are_fixed() {
        assert!(Flavour::Kimi.check(&json!({ "temperature": 0.2 }), true).is_err());
        assert!(Flavour::Kimi.check(&json!({ "temperature": 1.0, "n": 1 }), true).is_ok());
        assert!(Flavour::Kimi.check(&json!({ "n": 2 }), true).is_err());
    }
}
