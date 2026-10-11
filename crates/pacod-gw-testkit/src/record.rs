//! Recording provider answers as fixtures (concept section 8.6).
//!
//! `gw-smoke record` calls each provider directly, without the gateway, with a fixed set of
//! small requests: a plain call, streams with and without usage, reasoning switched on and off,
//! a two-turn tool loop (once with the reasoning the provider returned and once without it), a
//! cached second call, and the errors of a wrong model and a wrong key. Each exchange becomes one
//! JSON file under `fixtures/<provider>/`. The parsers and the mock are written against these
//! files, so that what the documentation leaves open is settled by what the provider sent.
//!
//! The files are committed, so they are scrubbed before they are written:
//!
//! - response headers are kept only from an allow-list (content type, `retry-after`; rate-limit
//!   headers by name, with their values replaced);
//! - ids are replaced by stable placeholders (`fx-1`, `fx-2`, …), consistently across a
//!   provider's whole run, so that a tool call's id in one answer still matches the id the next
//!   request sends back;
//! - Google project numbers in error details are replaced;
//! - a file that would contain any of the provider keys is not written, and the run fails.
//!
//! Bytes other than the replaced strings are kept as the provider sent them, including the
//! framing of streams.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};

/// A provider that can be recorded, with the key it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// DeepSeek's OpenAI-compatible API.
    DeepSeek,
    /// GLM (z.ai), OpenAI-compatible.
    Glm,
    /// Gemini's OpenAI-compatible endpoint.
    GeminiOpenai,
    /// Gemini's native API.
    Gemini,
    /// Gemini on Vertex AI, with a service account's token.
    Vertex,
}

impl Target {
    pub const ALL: [Target; 5] = [Target::DeepSeek, Target::Glm, Target::GeminiOpenai, Target::Gemini, Target::Vertex];

    /// The directory name under `fixtures/`.
    pub fn name(self) -> &'static str {
        match self {
            Target::DeepSeek => "deepseek",
            Target::Glm => "glm",
            Target::GeminiOpenai => "gemini-openai",
            Target::Gemini => "gemini",
            Target::Vertex => "vertex",
        }
    }

    /// The environment variable that holds the key. Vertex's is the path of a service-account
    /// key file, with `VERTEX_PROJECT` and `VERTEX_LOCATION` beside it.
    pub fn key_var(self) -> &'static str {
        match self {
            Target::DeepSeek => "DEEPSEEK_API_KEY",
            Target::Glm => "ZAI_API_KEY",
            Target::GeminiOpenai | Target::Gemini => "GEMINI_API_KEY",
            Target::Vertex => "VERTEX_CREDENTIALS_FILE",
        }
    }

    pub fn parse(name: &str) -> Option<Target> {
        Target::ALL.into_iter().find(|t| t.name() == name)
    }
}

/// The models a run calls. Each has a small, current default.
#[derive(Debug, Clone)]
pub struct Models {
    pub deepseek: String,
    /// A GLM model with the `thinking` switch.
    pub glm: String,
    /// A GLM model that takes `reasoning_effort`.
    pub glm_effort: String,
    pub gemini: String,
    /// A cheaper Gemini model with the `minimal` thinking level.
    pub gemini_lite: String,
    /// Gemini's embedding model, on AI Studio and on Vertex.
    pub gemini_embedding: String,
    /// Vertex's text embedding model, called with `:predict`.
    pub vertex_embedding: String,
}

impl Default for Models {
    fn default() -> Self {
        Self {
            deepseek: "deepseek-flash".into(),
            glm: "glm-4.7".into(),
            glm_effort: "glm-5.3".into(),
            gemini: "gemini-3.8-flash".into(),
            gemini_lite: "gemini-3.5-flash-lite".into(),
            gemini_embedding: "gemini-embedding-001".into(),
            vertex_embedding: "text-embedding-005".into(),
        }
    }
}

/// The response headers a fixture keeps. Rate-limit headers are kept by name only.
const KEPT_HEADERS: &[&str] = &["content-type", "retry-after"];
const NAMED_HEADER_PREFIXES: &[&str] = &["x-ratelimit-"];

/// Members whose string values identify one response, call or account; replaced by placeholders.
const ID_KEYS: &[&str] = &["id", "responseId", "system_fingerprint", "tool_call_id", "request_id", "requestId"];

/// The tool every tool case offers.
fn weather_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "The current weather in a city.",
            "parameters": {
                "type": "object",
                "properties": { "city": { "type": "string", "description": "The city name" } },
                "required": ["city"]
            }
        }
    })
}

const TOOL_PROMPT: &str =
    "What is the weather in Paris right now? Use the get_weather tool, then answer in one sentence.";
const TOOL_RESULT: &str = "{\"city\":\"Paris\",\"temperature_c\":18,\"sky\":\"light rain\"}";

/// A system prompt long enough for every provider's prompt cache (Gemini needs 4,096 tokens),
/// and new for every run so that the first call cannot hit an old cache entry.
fn cacheable_system(run: &str) -> String {
    format!(
        "Recording run {run}. {}",
        "The quick brown fox jumps over the lazy dog while the cat watches. ".repeat(420)
    )
}

/// Where a Vertex project is reached.
#[derive(Debug, Clone)]
pub struct VertexSite {
    pub project: String,
    pub location: String,
    /// The service account's address: replaced wherever it appears.
    pub client_email: String,
    /// Secret strings of the account (its key id): nothing written may contain them.
    pub secrets: Vec<String>,
}

impl VertexSite {
    /// The host: `aiplatform.googleapis.com` for the global location, else the regional one.
    fn host(&self) -> String {
        if self.location == "global" {
            "https://aiplatform.googleapis.com".into()
        } else {
            format!("https://{}-aiplatform.googleapis.com", self.location)
        }
    }

    /// The base URL a Vertex provider is set up with.
    fn base_url(&self) -> String {
        format!("{}/v1/projects/{}/locations/{}/publishers/google", self.host(), self.project, self.location)
    }
}

/// One recorded exchange, as written to disk.
struct Exchange {
    case: String,
    method: &'static str,
    /// The path below the provider's base URL, with the query; a key in the query is never
    /// kept. With `root`, the path below the host instead.
    path: String,
    root: bool,
    request: Option<Value>,
    status: u16,
    headers: BTreeMap<String, String>,
    /// The response body as text, exactly as received apart from the replaced strings.
    body: String,
}

/// Writes fixtures for one or more providers.
pub struct Recorder {
    http: reqwest::Client,
    out: PathBuf,
    models: Models,
    /// Every key in the environment: nothing written may contain one.
    secrets: Vec<String>,
    /// Original string → placeholder, per provider run.
    replacements: BTreeMap<String, String>,
    run: String,
    /// The Vertex project to record against; required for [`Target::Vertex`].
    pub vertex: Option<VertexSite>,
    /// Only these cases are sent and written; empty means all. Cases that need an earlier
    /// answer (the second turn of a tool loop) are skipped with it.
    pub only: Vec<String>,
    /// What each case produced, for the report.
    pub lines: Vec<String>,
}

impl Recorder {
    pub fn new(out: PathBuf, models: Models, secrets: Vec<String>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(180)).build()?;
        let run = uuid::Uuid::now_v7().simple().to_string()[20..].to_owned();
        Ok(Self {
            http,
            out,
            models,
            secrets,
            replacements: BTreeMap::new(),
            run,
            vertex: None,
            only: Vec::new(),
            lines: Vec::new(),
        })
    }

    /// The HTTP client, for minting a Vertex token before [`Recorder::record`].
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Records every case of `target` with `key` (for Vertex: an access token). A case that
    /// fails to send is reported and the run goes on; a fixture that would leak a key stops the
    /// run.
    pub async fn record(&mut self, target: Target, key: &str) -> anyhow::Result<()> {
        self.replacements.clear();
        let dir = self.out.join(target.name());
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        match target {
            Target::Gemini | Target::Vertex => {
                if target == Target::Vertex {
                    let Some(site) = self.vertex.clone() else { bail!("vertex: no project and location given") };
                    // A token is a secret like a key; the account's address and project are
                    // replaced wherever they appear.
                    self.secrets.push(key.to_owned());
                    self.secrets.extend(site.secrets.iter().cloned());
                }
                self.gemini_native(target, key, &dir).await
            }
            _ => self.compatible(target, key, &dir).await,
        }
    }

    /// The base URL of `target`.
    fn base_url(&self, target: Target) -> String {
        match target {
            Target::DeepSeek => "https://api.deepseek.com".into(),
            Target::Glm => "https://api.z.ai/api/paas/v4".into(),
            Target::GeminiOpenai => "https://generativelanguage.googleapis.com/v1beta/openai".into(),
            Target::Gemini => "https://generativelanguage.googleapis.com/v1beta".into(),
            Target::Vertex => self.vertex.as_ref().map(VertexSite::base_url).unwrap_or_default(),
        }
    }

    // --- OpenAI-compatible providers -------------------------------------------------------

    async fn compatible(&mut self, target: Target, key: &str, dir: &Path) -> anyhow::Result<()> {
        let model = match target {
            Target::DeepSeek => self.models.deepseek.clone(),
            Target::Glm => self.models.glm.clone(),
            _ => self.models.gemini.clone(),
        };
        let hello = json!([{ "role": "user", "content": "Reply with the single word: pong" }]);
        let chat = |extra: Value| {
            let mut body = json!({ "model": model, "messages": hello, "max_tokens": 1024 });
            merge(&mut body, extra);
            body
        };

        self.get(target, key, dir, "models", "/models").await?;
        // What the gateway's model probe asks.
        self.get(target, key, dir, "model-get", &format!("/models/{model}")).await?;
        self.post(target, key, dir, "chat", "/chat/completions", chat(json!({}))).await?;
        self.post(
            target,
            key,
            dir,
            "chat-stream",
            "/chat/completions",
            chat(json!({ "stream": true, "stream_options": { "include_usage": true } })),
        )
        .await?;
        self.post(target, key, dir, "chat-stream-plain", "/chat/completions", chat(json!({ "stream": true }))).await?;

        // Reasoning, in the form each provider documents.
        let (off, on): (Value, Value) = match target {
            Target::DeepSeek => (json!({ "thinking": { "type": "disabled" } }), json!({ "reasoning_effort": "low" })),
            Target::Glm => {
                (json!({ "thinking": { "type": "disabled" } }), json!({ "thinking": { "type": "enabled" } }))
            }
            _ => (json!({ "reasoning_effort": "none" }), json!({ "reasoning_effort": "low" })),
        };
        self.post(target, key, dir, "reasoning-off", "/chat/completions", chat(off)).await?;
        self.post(target, key, dir, "reasoning-on", "/chat/completions", chat(on.clone())).await?;
        let mut streamed = chat(on);
        merge(&mut streamed, json!({ "stream": true, "stream_options": { "include_usage": true } }));
        self.post(target, key, dir, "reasoning-stream", "/chat/completions", streamed).await?;
        match target {
            Target::Glm => {
                let mut body = chat(json!({ "reasoning_effort": "low" }));
                body["model"] = json!(self.models.glm_effort);
                self.post(target, key, dir, "reasoning-effort", "/chat/completions", body).await?;
            }
            Target::GeminiOpenai => {
                // Where the compatible endpoint puts thought text (not documented).
                let thoughts = json!({ "extra_body": { "google": { "thinking_config": { "thinking_level": "low", "include_thoughts": true } } } });
                self.post(target, key, dir, "thoughts", "/chat/completions", chat(thoughts.clone())).await?;
                let mut body = chat(thoughts);
                merge(&mut body, json!({ "stream": true, "stream_options": { "include_usage": true } }));
                self.post(target, key, dir, "thoughts-stream", "/chat/completions", body).await?;
                let mut body = chat(json!({ "reasoning_effort": "minimal" }));
                body["model"] = json!(self.models.gemini);
                self.post(target, key, dir, "reasoning-minimal", "/chat/completions", body).await?;
                let mut body = chat(json!({ "reasoning_effort": "minimal" }));
                body["model"] = json!(self.models.gemini_lite);
                self.post(target, key, dir, "reasoning-minimal-lite", "/chat/completions", body).await?;
                let mut body = chat(json!({ "reasoning_effort": "none" }));
                body["model"] = json!("gemini-2.5-flash");
                self.post(target, key, dir, "reasoning-off-2-5", "/chat/completions", body).await?;
                // Embeddings on the compatible endpoint: is usage reported?
                let embeddings = json!({ "model": self.models.gemini_embedding, "input": ["The quick brown fox.", "A lazy dog."], "dimensions": 8 });
                self.post(target, key, dir, "embeddings", "/embeddings", embeddings.clone()).await?;
                let mut base64 = embeddings;
                base64["encoding_format"] = json!("base64");
                self.post(target, key, dir, "embeddings-base64", "/embeddings", base64).await?;
            }
            _ => {}
        }

        // A tool loop: the call, the answer with the assistant turn sent back as it came, and
        // the same without what the provider added (reasoning, signatures).
        let user = json!({ "role": "user", "content": TOOL_PROMPT });
        let first = json!({ "model": model, "max_tokens": 2048, "tools": [weather_tool()], "messages": [user] });
        let answer = self.post(target, key, dir, "tools-1", "/chat/completions", first).await?;
        let assistant = answer.as_ref().and_then(|v| v.pointer("/choices/0/message").cloned());
        match assistant {
            Some(assistant) if assistant.get("tool_calls").is_some() => {
                let call_id = assistant.pointer("/tool_calls/0/id").and_then(Value::as_str).unwrap_or("").to_owned();
                let tool = json!({ "role": "tool", "tool_call_id": call_id, "content": TOOL_RESULT });
                let second = |assistant: Value| json!({ "model": model, "max_tokens": 2048, "tools": [weather_tool()], "messages": [user, assistant, tool] });
                self.post(target, key, dir, "tools-2", "/chat/completions", second(assistant.clone())).await?;
                let mut stream = second(assistant.clone());
                merge(&mut stream, json!({ "stream": true, "stream_options": { "include_usage": true } }));
                self.post(target, key, dir, "tools-2-stream", "/chat/completions", stream).await?;
                let mut bare = assistant;
                if let Some(m) = bare.as_object_mut() {
                    m.retain(|k, _| matches!(k.as_str(), "role" | "content" | "tool_calls"));
                }
                if let Some(calls) = bare.get_mut("tool_calls").and_then(Value::as_array_mut) {
                    for c in calls {
                        if let Some(c) = c.as_object_mut() {
                            c.retain(|k, _| matches!(k.as_str(), "id" | "type" | "function"));
                        }
                    }
                }
                // A turn another provider served brings no reasoning: is an empty one accepted?
                let mut empty = bare.clone();
                empty["reasoning_content"] = json!("");
                self.post(target, key, dir, "tools-2-empty-reasoning", "/chat/completions", second(empty)).await?;
                self.post(target, key, dir, "tools-2-bare", "/chat/completions", second(bare)).await?;
            }
            _ => self
                .lines
                .push(format!("{}: tools-1 brought no tool call; the tool loop was not recorded", target.name())),
        }
        let mut stream = json!({ "model": model, "max_tokens": 2048, "tools": [weather_tool()], "messages": [{ "role": "user", "content": TOOL_PROMPT }] });
        merge(&mut stream, json!({ "stream": true, "stream_options": { "include_usage": true } }));
        self.post(target, key, dir, "tools-1-stream", "/chat/completions", stream).await?;
        self.post(
            target,
            key,
            dir,
            "tool-choice-required",
            "/chat/completions",
            json!({ "model": model, "max_tokens": 512, "tools": [weather_tool()], "tool_choice": "required", "messages": [{ "role": "user", "content": TOOL_PROMPT }] }),
        )
        .await?;

        // What each provider makes of the parameters it is said to restrict.
        let schema = json!({ "type": "json_schema", "json_schema": { "name": "answer", "strict": true, "schema": {
            "type": "object", "properties": { "word": { "type": "string" } }, "required": ["word"], "additionalProperties": false } } });
        self.post(target, key, dir, "json-schema", "/chat/completions", chat(json!({ "response_format": schema })))
            .await?;
        self.post(
            target,
            key,
            dir,
            "json-object",
            "/chat/completions",
            json!({ "model": model, "max_tokens": 512, "response_format": { "type": "json_object" },
                    "messages": [{ "role": "user", "content": "Answer in JSON with the key \"word\" set to pong." }] }),
        )
        .await?;
        self.post(target, key, dir, "stop-two", "/chat/completions", chat(json!({ "stop": ["po", "ng"] }))).await?;
        let prefill = json!({ "model": model, "max_tokens": 256, "messages": [
            { "role": "user", "content": "Count from one to five." }, { "role": "assistant", "content": "One, two," } ] });
        self.post(target, key, dir, "prefill", "/chat/completions", prefill).await?;

        // The prompt cache: the second identical call reads what the first wrote.
        let cached = json!({ "model": model, "max_tokens": 64, "messages": [
            { "role": "system", "content": cacheable_system(&self.run) },
            { "role": "user", "content": "Reply with the single word: pong" } ] });
        self.post(target, key, dir, "cache-1", "/chat/completions", cached.clone()).await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        self.post(target, key, dir, "cache-2", "/chat/completions", cached).await?;

        // Errors.
        let mut wrong = chat(json!({}));
        wrong["model"] = json!("no-such-model-for-recording");
        self.post(target, key, dir, "error-model", "/chat/completions", wrong).await?;
        self.post(target, "invalid-key-for-recording", dir, "error-key", "/chat/completions", chat(json!({}))).await?;
        Ok(())
    }

    // --- Gemini's native API, on AI Studio and on Vertex AI --------------------------------

    async fn gemini_native(&mut self, target: Target, key: &str, dir: &Path) -> anyhow::Result<()> {
        let vertex = target == Target::Vertex;
        let model = self.models.gemini.clone();
        let generate = format!("/models/{model}:generateContent");
        let stream = format!("/models/{model}:streamGenerateContent");
        let hello = json!([{ "role": "user", "parts": [{ "text": "Reply with the single word: pong" }] }]);
        let body = |extra: Value| {
            let mut b = json!({ "contents": hello });
            merge(&mut b, extra);
            b
        };

        if vertex {
            // Vertex lists Google's models without a project, in v1beta1.
            self.get_root(target, key, dir, "models", "/v1beta1/publishers/google/models?pageSize=5").await?;
            self.get_root(target, key, dir, "model-get-root", &format!("/v1/publishers/google/models/{model}")).await?;
        } else {
            self.get(target, key, dir, "models", "/models?pageSize=5").await?;
        }
        self.get(target, key, dir, "model-get", &format!("/models/{model}")).await?;
        self.post(target, key, dir, "generate", &generate, body(json!({}))).await?;
        self.post(target, key, dir, "stream-sse", &format!("{stream}?alt=sse"), body(json!({}))).await?;
        self.post(target, key, dir, "stream-array", &stream, body(json!({}))).await?;
        let thoughts =
            json!({ "generationConfig": { "thinkingConfig": { "thinkingLevel": "low", "includeThoughts": true } } });
        self.post(target, key, dir, "thoughts", &generate, body(thoughts.clone())).await?;
        self.post(target, key, dir, "thoughts-stream", &format!("{stream}?alt=sse"), body(thoughts)).await?;
        // Gemini 2.5 takes a token budget instead of a level.
        let budget =
            json!({ "generationConfig": { "thinkingConfig": { "thinkingBudget": 1024, "includeThoughts": true } } });
        self.post(target, key, dir, "budget-2-5", "/models/gemini-2.5-flash:generateContent", body(budget)).await?;
        let level = json!({ "generationConfig": { "thinkingConfig": { "thinkingLevel": "low" } } });
        self.post(target, key, dir, "level-2-5", "/models/gemini-2.5-flash:generateContent", body(level)).await?;

        let tools = json!([{ "functionDeclarations": [{
            "name": "get_weather", "description": "The current weather in a city.",
            "parameters": { "type": "object", "properties": { "city": { "type": "string" } }, "required": ["city"] } }] }]);
        let user = json!({ "role": "user", "parts": [{ "text": TOOL_PROMPT }] });
        let first = json!({ "contents": [user], "tools": tools });
        let answer = self.post(target, key, dir, "tools-1", &generate, first.clone()).await?;
        self.post(target, key, dir, "tools-1-stream", &format!("{stream}?alt=sse"), first).await?;
        let model_turn = answer.as_ref().and_then(|v| v.pointer("/candidates/0/content").cloned());
        let call = model_turn
            .as_ref()
            .and_then(|c| c["parts"].as_array()?.iter().find_map(|p| p.get("functionCall").cloned()));
        let result_of = |call: &Value| -> anyhow::Result<Value> {
            let mut response =
                json!({ "name": call["name"], "response": { "result": serde_json::from_str::<Value>(TOOL_RESULT)? } });
            if let Some(id) = call.get("id") {
                response["id"] = id.clone();
            }
            Ok(json!({ "role": "user", "parts": [{ "functionResponse": response }] }))
        };
        match (model_turn, call) {
            (Some(model_turn), Some(call)) => {
                let result = result_of(&call)?;
                let second = |turn: Value| json!({ "contents": [user, turn, result], "tools": tools });
                self.post(target, key, dir, "tools-2", &generate, second(model_turn.clone())).await?;
                let mut bare = model_turn;
                for p in bare["parts"].as_array_mut().into_iter().flatten() {
                    if let Some(p) = p.as_object_mut() {
                        p.remove("thoughtSignature");
                    }
                }
                self.post(target, key, dir, "tools-2-bare", &generate, second(bare.clone())).await?;
                // Google's documented value for a history whose signatures were lost.
                for p in bare["parts"].as_array_mut().into_iter().flatten() {
                    if p.get("functionCall").is_some() {
                        p["thoughtSignature"] = json!("skip_thought_signature_validator");
                        break;
                    }
                }
                self.post(target, key, dir, "tools-2-skip", &generate, second(bare)).await?;
            }
            _ => self
                .lines
                .push(format!("{}: tools-1 brought no function call; the tool loop was not recorded", target.name())),
        }
        if vertex {
            // Does Vertex take a turn whose signature AI Studio issued? The model turn comes
            // from the AI Studio recording.
            let studio = self.out.join("gemini").join("tools-1.json");
            let turn = std::fs::read_to_string(&studio)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|f| f.pointer("/response/body/candidates/0/content").cloned());
            let call =
                turn.as_ref().and_then(|c| c["parts"].as_array()?.iter().find_map(|p| p.get("functionCall").cloned()));
            match (turn, call) {
                (Some(turn), Some(call)) => {
                    let body = json!({ "contents": [user, turn, result_of(&call)?], "tools": tools });
                    self.post(target, key, dir, "tools-2-foreign-signature", &generate, body).await?;
                }
                _ => {
                    self.lines.push("vertex: no AI Studio tool call in gemini/tools-1.json; record gemini first".into())
                }
            }
        }

        let schema = json!({ "generationConfig": { "responseMimeType": "application/json", "responseJsonSchema": {
            "type": "object", "properties": { "word": { "type": "string" } }, "required": ["word"] } } });
        self.post(target, key, dir, "json-schema", &generate, body(schema)).await?;
        let count = format!("/models/{model}:countTokens");
        self.post(target, key, dir, "count-tokens", &count, json!({ "contents": hello })).await?;
        // The form that also counts a system instruction and tools.
        let full =
            json!({ "systemInstruction": { "parts": [{ "text": "Be brief." }] }, "contents": hello, "tools": tools });
        let mut nested = full.clone();
        nested["model"] = json!(format!("models/{model}"));
        self.post(target, key, dir, "count-tokens-request", &count, json!({ "generateContentRequest": nested }))
            .await?;
        if vertex {
            // Vertex documents the flat form with the same members.
            self.post(target, key, dir, "count-tokens-flat", &count, full).await?;
            self.post(target, key, dir, "labels", &generate, body(json!({ "labels": { "team": "recording" } })))
                .await?;
        }

        // Embeddings.
        let embedding = self.models.gemini_embedding.clone();
        let content = |text: &str| json!({ "parts": [{ "text": text }] });
        self.post(
            target,
            key,
            dir,
            "embed",
            &format!("/models/{embedding}:embedContent"),
            json!({ "content": content("The quick brown fox."), "taskType": "RETRIEVAL_DOCUMENT", "outputDimensionality": 128 }),
        )
        .await?;
        let request = |text: &str, dims: u32| json!({ "model": format!("models/{embedding}"), "content": content(text), "outputDimensionality": dims });
        let batch = format!("/models/{embedding}:batchEmbedContents");
        self.post(
            target,
            key,
            dir,
            "batch-embed",
            &batch,
            json!({ "requests": [request("The quick brown fox.", 128), request("A lazy dog.", 128)] }),
        )
        .await?;
        if vertex {
            let predict = format!("/models/{}:predict", self.models.vertex_embedding);
            let instances = json!({ "instances": [{ "content": "The quick brown fox." }, { "content": "A lazy dog." }],
                                    "parameters": { "outputDimensionality": 128 } });
            self.post(target, key, dir, "embed-predict", &predict, instances).await?;
        } else {
            // Where the batch limit is: 101 requests of a few dimensions.
            let many: Vec<Value> = (0..101).map(|i| request(&format!("Item {i}."), 8)).collect();
            self.post(target, key, dir, "batch-embed-101", &batch, json!({ "requests": many })).await?;
            self.get(target, key, dir, "model-get-embedding", &format!("/models/{embedding}")).await?;
        }

        let cached =
            json!({ "systemInstruction": { "parts": [{ "text": cacheable_system(&self.run) }] }, "contents": hello });
        self.post(target, key, dir, "cache-1", &generate, cached.clone()).await?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        self.post(target, key, dir, "cache-2", &generate, cached).await?;
        self.post(
            target,
            key,
            dir,
            "error-model",
            "/models/no-such-model-for-recording:generateContent",
            body(json!({})),
        )
        .await?;
        let case = if vertex { "error-auth" } else { "error-key" };
        self.post(target, "invalid-key-for-recording", dir, case, &generate, body(json!({}))).await?;
        Ok(())
    }

    // --- sending and writing ----------------------------------------------------------------

    async fn get(&mut self, target: Target, key: &str, dir: &Path, case: &str, path: &str) -> anyhow::Result<()> {
        let req = self.http.get(format!("{}{path}", self.base_url(target)));
        self.send(target, key, dir, case, "GET", (path, false), None, req).await.map(|_| ())
    }

    /// A GET below the host rather than the base URL (Vertex's project-less paths).
    async fn get_root(&mut self, target: Target, key: &str, dir: &Path, case: &str, path: &str) -> anyhow::Result<()> {
        let host = self.vertex.as_ref().map(VertexSite::host).unwrap_or_default();
        let req = self.http.get(format!("{host}{path}"));
        self.send(target, key, dir, case, "GET", (path, true), None, req).await.map(|_| ())
    }

    /// Sends one request and writes its fixture. Returns the parsed answer when it is JSON.
    async fn post(
        &mut self,
        target: Target,
        key: &str,
        dir: &Path,
        case: &str,
        path: &str,
        body: Value,
    ) -> anyhow::Result<Option<Value>> {
        let req = self.http.post(format!("{}{path}", self.base_url(target))).header("content-type", "application/json");
        self.send(target, key, dir, case, "POST", (path, false), Some(body), req).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn send(
        &mut self,
        target: Target,
        key: &str,
        dir: &Path,
        case: &str,
        method: &'static str,
        (path, root): (&str, bool),
        body: Option<Value>,
        req: reqwest::RequestBuilder,
    ) -> anyhow::Result<Option<Value>> {
        if !self.only.is_empty() && !self.only.iter().any(|c| c == case) {
            return Ok(None);
        }
        let req = match target {
            Target::Gemini => req.header("x-goog-api-key", key),
            // Vertex: the service account's access token.
            _ => req.bearer_auth(key),
        };
        let req = match &body {
            Some(b) => req.body(b.to_string()),
            None => req,
        };
        let response = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                self.lines.push(format!("{}: {case}: not sent: {e}", target.name()));
                return Ok(None);
            }
        };
        let status = response.status().as_u16();
        let mut headers = BTreeMap::new();
        for (name, value) in response.headers() {
            let name = name.as_str().to_ascii_lowercase();
            if KEPT_HEADERS.contains(&name.as_str()) {
                headers.insert(name, value.to_str().unwrap_or("").to_owned());
            } else if NAMED_HEADER_PREFIXES.iter().any(|p| name.starts_with(p)) {
                headers.insert(name, "fx".into());
            }
        }
        let text = response.text().await.unwrap_or_default();
        let parsed: Option<Value> = serde_json::from_str(&text).ok();
        self.lines.push(format!("{}: {case}: {status}, {} bytes", target.name(), text.len()));
        let exchange = Exchange {
            case: case.to_owned(),
            method,
            path: path.to_owned(),
            root,
            request: body,
            status,
            headers,
            body: text,
        };
        self.write(target, dir, exchange)?;
        Ok(parsed)
    }

    fn write(&mut self, target: Target, dir: &Path, ex: Exchange) -> anyhow::Result<()> {
        // Collect what identifies the call, in both the request (ids sent back) and the answer.
        // Model lists hold nothing of the kind.
        if !ex.case.starts_with("models") && !ex.case.starts_with("model-get") {
            if let Some(r) = &ex.request {
                collect(r, &mut self.replacements);
            }
            for value in json_values(&ex.body) {
                collect(&value, &mut self.replacements);
            }
        }
        let mut request = ex.request.as_ref().map(|r| self.scrub(&r.to_string()));
        if let Some(r) = &request
            && serde_json::from_str::<Value>(r).is_err()
        {
            request = None;
        }
        let response_body = self.scrub(&ex.body);
        let mut fixture = Map::new();
        fixture.insert("source".into(), json!("recorded"));
        fixture.insert("recorded".into(), json!(chrono_today()));
        fixture.insert("provider".into(), json!(target.name()));
        fixture.insert("case".into(), json!(ex.case));
        let mut req = json!({ "method": ex.method, "path": ex.path });
        if ex.root {
            req["root"] = json!(true);
        }
        if let Some(r) = request {
            req["body"] = serde_json::from_str(&r)?;
        }
        fixture.insert("request".into(), req);
        let mut resp = json!({ "status": ex.status, "headers": ex.headers });
        match serde_json::from_str::<Value>(&response_body) {
            // A whole JSON body is stored as JSON; anything else (a stream) as its text.
            Ok(v) if !is_stream_text(&ex.headers) => resp["body"] = v,
            _ => resp["text"] = json!(response_body),
        }
        fixture.insert("response".into(), resp);
        let text = serde_json::to_string_pretty(&Value::Object(fixture))? + "\n";
        if self.secrets.iter().any(|s| s.len() >= 8 && text.contains(s.as_str())) {
            bail!("{}: {}: the answer contains a provider key; nothing was written", target.name(), ex.case);
        }
        let file = dir.join(format!("{}.json", ex.case));
        std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
        Ok(())
    }

    fn scrub(&self, text: &str) -> String {
        let mut out = text.to_owned();
        // The Vertex project and service account, wherever they appear (paths, error texts).
        if let Some(site) = &self.vertex {
            out = out.replace(&site.client_email, "fx-service-account").replace(&site.project, "fx-project");
        }
        for (from, to) in &self.replacements {
            out = out.replace(&format!("\"{from}\""), &format!("\"{to}\""));
            // The same string inside an escaped JSON string (a request body stored as text).
            out = out.replace(&format!("\\\"{from}\\\""), &format!("\\\"{to}\\\""));
        }
        scrub_projects(&out)
    }
}

/// Every JSON value in a body: the body itself, the elements of a JSON array stream, or the
/// `data:` payloads of an event stream.
fn json_values(body: &str) -> Vec<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        return match v {
            Value::Array(items) => items,
            v => vec![v],
        };
    }
    body.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .collect()
}

/// Records a placeholder for every id-like string in `v`. A model's id is no secret and is
/// what the parsers look for: it stays.
fn collect(v: &Value, out: &mut BTreeMap<String, String>) {
    match v {
        Value::Object(map) => {
            let is_model = map.get("object").and_then(Value::as_str) == Some("model");
            for (k, child) in map {
                if is_model && k == "id" {
                    continue;
                }
                if ID_KEYS.contains(&k.as_str())
                    && let Some(s) = child.as_str()
                    && !s.is_empty()
                    && !out.contains_key(s)
                {
                    let n = out.len() + 1;
                    out.insert(s.to_owned(), format!("fx-{n}"));
                }
                collect(child, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| collect(i, out)),
        _ => {}
    }
}

/// `projects/123456789` → `projects/0`, wherever it appears.
fn scrub_projects(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("projects/") {
        let (head, tail) = rest.split_at(at + "projects/".len());
        out.push_str(head);
        let digits = tail.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 {
            out.push('0');
        }
        rest = &tail[digits..];
    }
    out.push_str(rest);
    out
}

fn is_stream_text(headers: &BTreeMap<String, String>) -> bool {
    headers.get("content-type").is_some_and(|c| c.starts_with("text/event-stream"))
}

/// Shallow-merges the members of `extra` into `body`.
fn merge(body: &mut Value, extra: Value) {
    if let (Some(b), Value::Object(e)) = (body.as_object_mut(), extra) {
        for (k, v) in e {
            b.insert(k, v);
        }
    }
}

/// Today's date (UTC) as `YYYY-MM-DD`, without a date library.
fn chrono_today() -> String {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_replaced_consistently_and_projects_are_hidden() {
        let mut map = BTreeMap::new();
        let answer =
            json!({ "id": "chatcmpl-abc", "choices": [{ "message": { "tool_calls": [{ "id": "call_9" }] } }] });
        collect(&answer, &mut map);
        let request = json!({ "messages": [{ "role": "tool", "tool_call_id": "call_9" }] });
        collect(&request, &mut map);
        assert_eq!(map.len(), 2, "the id sent back is the same placeholder: {map:?}");
        let r = Recorder {
            http: reqwest::Client::new(),
            out: PathBuf::new(),
            models: Models::default(),
            secrets: vec![],
            replacements: map,
            run: "x".into(),
            vertex: None,
            only: vec![],
            lines: vec![],
        };
        let scrubbed = r.scrub(&format!("{answer} {request} consumer projects/12345678 and projects/x"));
        assert!(!scrubbed.contains("call_9") && !scrubbed.contains("chatcmpl-abc"), "{scrubbed}");
        assert!(scrubbed.contains("projects/0 and projects/x"), "{scrubbed}");
        let a = scrubbed.matches("\"fx-").count();
        assert_eq!(a, 3, "{scrubbed}");
    }

    #[test]
    fn model_ids_stay() {
        let mut map = BTreeMap::new();
        collect(&json!({ "data": [{ "id": "deepseek-flash", "object": "model" }], "id": "chatcmpl-1" }), &mut map);
        assert_eq!(map.keys().collect::<Vec<_>>(), ["chatcmpl-1"]);
    }

    #[test]
    fn stream_payloads_are_found() {
        let sse = "data: {\"id\":\"a\"}\n\n: keep-alive\n\ndata: [DONE]\n\n";
        assert_eq!(json_values(sse), vec![json!({ "id": "a" })]);
        assert_eq!(json_values("[{\"responseId\":\"r\"},{\"x\":1}]").len(), 2);
    }

    #[test]
    fn dates_are_civil() {
        let d = chrono_today();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"), "{d}");
    }
}
