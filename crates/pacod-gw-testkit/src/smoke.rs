//! The smoke test: a short flow against a running gateway, through its public interfaces
//! only. It sets up an organisation of its own, makes a handful of calls in both modes, and
//! checks what came back and what was recorded.
//!
//! Two profiles. `mock` points the gateway at the scripted mock provider and checks exact
//! behaviour. `live` uses real provider keys from the environment, makes a few tiny calls per
//! provider, and checks that nothing the gateway returns contains a key.
//!
//! Provider keys are read from the environment and sent only to the gateway's management
//! API. They are never printed: every text this module reports goes through [`Smoke::redact`].

use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use reqwest::Method;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};

/// Where the gateway is, and how to log in.
#[derive(Debug, Clone)]
pub struct Options {
    /// The management API, for example `http://localhost:18081`.
    pub admin: String,
    /// The data plane, for example `http://localhost:18080`.
    pub data: String,
    pub email: String,
    pub password: String,
    /// Signing in through pacod-id (`--auth oidc`) instead of the core's own login.
    pub oidc: Option<crate::oidc::IdClient>,
    /// The mock provider as the gateway reaches it, for example `http://gw-mock:9000`.
    pub mock_url: String,
    /// The mock provider as this tool reaches it, to script failures. Empty: the steps that
    /// need it are skipped.
    pub mock_control: String,
    /// Wait for the calls to appear in the request log. Needs the analytics service.
    pub check_events: bool,
}

/// The provider keys of the live profile, read from the environment. Never printed.
#[derive(Debug, Clone, Default)]
pub struct LiveKeys {
    pub openai: Option<String>,
    pub anthropic: Option<String>,
    pub gemini: Option<String>,
    pub deepseek: Option<String>,
    pub glm: Option<String>,
    /// TypeSafe's key, for one Jev classification through the intent plugin.
    pub typesafe: Option<String>,
}

impl LiveKeys {
    /// Every key that is set, for redaction and log scans.
    pub fn all(&self) -> Vec<String> {
        [&self.openai, &self.anthropic, &self.gemini, &self.deepseek, &self.glm, &self.typesafe]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }
}

/// The models the live profile calls. `None` picks a small current one.
#[derive(Debug, Clone, Default)]
pub struct LiveModels {
    pub openai: Option<String>,
    pub anthropic: Option<String>,
    pub gemini: Option<String>,
    pub deepseek: Option<String>,
    pub glm: Option<String>,
}

/// One line of the report.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Passed(String),
    Failed(String),
    Skipped(String),
}

/// A response of the gateway.
pub struct Reply {
    pub status: u16,
    pub headers: HeaderMap,
    pub text: String,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn json(&self) -> anyhow::Result<Value> {
        serde_json::from_str(&self.text).with_context(|| format!("not JSON: {}", short(&self.text)))
    }
}

fn short(s: &str) -> String {
    let mut out: String = s.chars().take(240).collect();
    if s.chars().count() > 240 {
        out.push('…');
    }
    out
}

/// The id of this run's provider whose name starts with `slug-`.
async fn find_provider(smoke: &Smoke, slug: &str) -> Option<String> {
    let prefix = format!("{slug}-{}", smoke.suffix);
    // By name: an installation that has seen many runs holds more providers than one page.
    let (_, v) = smoke.api("GET", &format!("/providers?q={prefix}&limit=50"), None).await.ok()?;
    v["items"]
        .as_array()?
        .iter()
        .find(|p| p["name"] == prefix.as_str())
        .and_then(|p| p["id"].as_str())
        .map(str::to_owned)
}

const TOOL_PROMPT: &str =
    "What is the weather in Paris right now? Use the get_weather tool, then answer in one sentence.";
const TOOL_RESULT: &str = "{\"city\":\"Paris\",\"temperature_c\":18,\"sky\":\"light rain\"}";

fn weather_tool_anthropic() -> Value {
    json!({ "name": "get_weather", "description": "The current weather in a city.",
            "input_schema": { "type": "object", "properties": { "city": { "type": "string" } }, "required": ["city"] } })
}

fn weather_tool_openai() -> Value {
    json!({ "type": "function", "function": { "name": "get_weather", "description": "The current weather in a city.",
            "parameters": { "type": "object", "properties": { "city": { "type": "string" } }, "required": ["city"] } } })
}

/// The content blocks of an Anthropic message stream, put back together.
fn assemble_anthropic(text: &str) -> anyhow::Result<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    let mut json_parts: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data: ") else { continue };
        let Ok(ev) = serde_json::from_str::<Value>(data) else { continue };
        match ev["type"].as_str() {
            Some("content_block_start") => {
                blocks.push(ev["content_block"].clone());
                json_parts.push(String::new());
            }
            Some("content_block_delta") => {
                let i = ev["index"].as_u64().unwrap_or(0) as usize;
                let (Some(b), Some(j)) = (blocks.get_mut(i), json_parts.get_mut(i)) else { continue };
                let d = &ev["delta"];
                match d["type"].as_str() {
                    Some("text_delta") => {
                        b["text"] =
                            json!(format!("{}{}", b["text"].as_str().unwrap_or(""), d["text"].as_str().unwrap_or("")))
                    }
                    Some("thinking_delta") => {
                        b["thinking"] = json!(format!(
                            "{}{}",
                            b["thinking"].as_str().unwrap_or(""),
                            d["thinking"].as_str().unwrap_or("")
                        ))
                    }
                    Some("signature_delta") => b["signature"] = d["signature"].clone(),
                    Some("input_json_delta") => j.push_str(d["partial_json"].as_str().unwrap_or("")),
                    _ => {}
                }
            }
            Some("error") => bail!("the stream ended with an error: {}", short(data)),
            _ => {}
        }
    }
    for (b, j) in blocks.iter_mut().zip(&json_parts) {
        if b["type"] == "tool_use" && !j.is_empty() {
            b["input"] = serde_json::from_str(j).context("tool input is not JSON")?;
        }
    }
    Ok(Value::Array(blocks))
}

pub struct Smoke {
    opt: Options,
    http: reqwest::Client,
    token: String,
    /// With `--auth oidc`: the session at pacod-id, whose access token `token` stands for.
    session: Option<tokio::sync::Mutex<crate::oidc::Session>>,
    /// Values that must never appear in output or in anything the gateway returns.
    secrets: Vec<String>,
    /// Makes the names of this run's objects unique.
    suffix: String,
    pub checks: Vec<Check>,
}

/// What [`Smoke::load_scene`] set up.
pub struct LoadScene {
    /// A gateway API key.
    pub key: String,
    /// The provider's name, as it appears in proxy URLs.
    pub provider_name: String,
    /// The model's id in the repository, for managed mode.
    pub model: String,
    /// The provider's own id for the model, for proxy mode.
    pub upstream: String,
}

/// What a run set up.
struct Scene {
    org: String,
    ws: String,
    key: String,
    key_id: String,
}

impl Smoke {
    pub async fn connect(opt: Options) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(90)).build()?;
        let suffix = uuid::Uuid::now_v7().simple().to_string()[20..].to_owned();
        if let Some(client) = opt.oidc.clone() {
            let session = crate::oidc::Session::sign_in(client, &opt.email, &opt.password).await?;
            let s = tokio::sync::Mutex::new(session);
            return Ok(Self {
                opt,
                http,
                token: String::new(),
                session: Some(s),
                secrets: Vec::new(),
                suffix,
                checks: Vec::new(),
            });
        }
        let r = http
            .post(format!("{}/api/v1/session", opt.admin))
            .json(&json!({ "email": opt.email, "password": opt.password }))
            .send()
            .await
            .with_context(|| format!("the management API at {} does not answer", opt.admin))?;
        if !r.status().is_success() {
            bail!("login as {} failed with {}", opt.email, r.status());
        }
        let token = r.json::<Value>().await?["token"].as_str().context("the login answer has no token")?.to_owned();
        Ok(Self { opt, http, token, session: None, secrets: Vec::new(), suffix, checks: Vec::new() })
    }

    /// The bearer for the management API: the core's session token, or pacod-id's access
    /// token, refreshed when due.
    async fn bearer(&self) -> anyhow::Result<String> {
        match &self.session {
            Some(s) => s.lock().await.access_token().await,
            None => Ok(self.token.clone()),
        }
    }

    /// Replaces every secret this run knows with a marker.
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for s in &self.secrets {
            if !s.is_empty() {
                out = out.replace(s.as_str(), "[redacted]");
            }
        }
        out
    }

    fn leaks(&self, text: &str) -> bool {
        self.secrets.iter().any(|s| s.len() >= 8 && text.contains(s.as_str()))
    }

    fn record(&mut self, name: &str, result: anyhow::Result<String>) -> bool {
        let outcome = match result {
            Ok(detail) => Outcome::Passed(self.redact(&detail)),
            Err(e) => Outcome::Failed(self.redact(&format!("{e:#}"))),
        };
        let ok = matches!(outcome, Outcome::Passed(_));
        self.checks.push(Check { name: name.to_owned(), outcome });
        ok
    }

    fn skip(&mut self, name: &str, why: &str) {
        self.checks.push(Check { name: name.to_owned(), outcome: Outcome::Skipped(why.to_owned()) });
    }

    pub fn failed(&self) -> usize {
        self.checks.iter().filter(|c| matches!(c.outcome, Outcome::Failed(_))).count()
    }

    pub fn print(&self) {
        for c in &self.checks {
            let (mark, detail) = match &c.outcome {
                Outcome::Passed(d) => ("ok  ", d),
                Outcome::Failed(d) => ("FAIL", d),
                Outcome::Skipped(d) => ("skip", d),
            };
            println!("{mark}  {:<44} {detail}", c.name);
        }
        let passed = self.checks.iter().filter(|c| matches!(c.outcome, Outcome::Passed(_))).count();
        let skipped = self.checks.iter().filter(|c| matches!(c.outcome, Outcome::Skipped(_))).count();
        println!("{passed} passed, {} failed, {skipped} skipped", self.failed());
    }

    // --- management API ---

    /// Calls the management API. After a change it waits until every running instance has
    /// applied it: with several gateways, the change reaches the others within a second or two,
    /// and the calls through the data plane that follow may land on any of them.
    async fn api(&self, method: &str, path: &str, body: Option<Value>) -> anyhow::Result<(u16, Value)> {
        let out = self.request(method, path, body).await?;
        if method != "GET" && (200..300).contains(&out.0) {
            self.wait_applied().await;
        }
        Ok(out)
    }

    /// Waits until every running instance has applied the configuration version the
    /// management API is at. Gives up quietly after 15 s, or when the core does not say.
    async fn wait_applied(&self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let Ok((200, live)) = self.request("GET", "/monitoring/live", None).await else { return };
            let (Some(version), Some(instances)) = (live["config_version"].as_i64(), live["instances"].as_array())
            else {
                return;
            };
            if instances.iter().all(|i| i["applied_version"].as_i64().is_some_and(|v| v >= version))
                || Instant::now() > deadline
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// How many running instances serve the data plane; 1 when the core does not say.
    async fn data_plane_instances(&self) -> usize {
        let Ok((200, live)) = self.request("GET", "/monitoring/live", None).await else { return 1 };
        live["instances"].as_array().map_or(1, |list| list.iter().filter(|i| i["role"] != "admin").count().max(1))
    }

    /// Calls the management API without waiting for the other instances.
    async fn request(&self, method: &str, path: &str, body: Option<Value>) -> anyhow::Result<(u16, Value)> {
        let token = self.bearer().await?;
        let mut req =
            self.http.request(method.parse()?, format!("{}/api/v1{path}", self.opt.admin)).bearer_auth(&token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await?;
        let status = r.status().as_u16();
        let text = r.text().await?;
        if self.leaks(&text) {
            bail!("the answer to {method} {path} contains a provider key");
        }
        let value =
            if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) };
        Ok((status, value))
    }

    async fn create(&self, path: &str, body: Value) -> anyhow::Result<Value> {
        let (status, v) = self.api("POST", path, Some(body)).await?;
        if status != 201 {
            bail!("POST {path} answered {status}: {}", short(&v.to_string()));
        }
        Ok(v)
    }

    fn id(v: &Value) -> anyhow::Result<String> {
        v["id"].as_str().map(str::to_owned).ok_or_else(|| anyhow!("no id in {}", short(&v.to_string())))
    }

    async fn scene(&self) -> anyhow::Result<Scene> {
        let org = Self::id(&self.create("/orgs", json!({ "name": format!("smoke-{}", self.suffix) })).await?)?;
        let ws = Self::id(
            &self
                .create(
                    &format!("/orgs/{org}/workspaces"),
                    // Audit mode on request: a call, a Realtime session or a batch is recorded in
                    // full only when it asks for it.
                    json!({ "name": "main", "tag_keys": ["purpose"],
                            "settings": { "audit": { "mode": "on_request", "realtime": true, "objects": true } } }),
                )
                .await?,
        )?;
        let key = self.create(&format!("/orgs/{org}/workspaces/{ws}/keys"), json!({ "name": "smoke" })).await?;
        Ok(Scene {
            key: key["secret"].as_str().context("the new key has no secret")?.to_owned(),
            key_id: Self::id(&key)?,
            org,
            ws,
        })
    }

    /// Registers a provider with a shared credential. Returns its id and its name.
    async fn provider(&self, family: &str, base_url: &str, secret: &str) -> anyhow::Result<(String, String)> {
        self.provider_named(family, family, base_url, secret, json!({})).await
    }

    /// As [`Self::provider`], with a slug prefix of its own (several providers of one family).
    async fn provider_named(
        &self,
        slug: &str,
        family: &str,
        base_url: &str,
        secret: &str,
        extra: Value,
    ) -> anyhow::Result<(String, String)> {
        let name = format!("{slug}-{}", self.suffix);
        let mut body = json!({ "name": name, "family": family, "base_url": base_url });
        for (k, v) in extra.as_object().into_iter().flatten() {
            body[k] = v.clone();
        }
        let id = Self::id(&self.create("/providers", body).await?)?;
        let cred =
            self.create(&format!("/providers/{id}/credentials"), json!({ "label": "smoke", "secret": secret })).await?;
        if cred.to_string().contains(secret) {
            bail!("the credential's secret came back from the API");
        }
        Ok((id, name))
    }

    /// Registers a model with a price, activates it (the gateway probes the provider) and
    /// grants it to the organisation.
    async fn model(
        &self,
        sc: &Scene,
        provider: &str,
        name: &str,
        upstream: &str,
        preset: &str,
    ) -> anyhow::Result<String> {
        let body =
            json!({ "name": name, "provider_id": provider, "upstream_id": upstream, "capability_preset": preset });
        let id = Self::id(&self.create("/models", body).await?)?;
        let (st, v) = self
            .api(
                "PUT",
                &format!("/models/{id}/price"),
                Some(json!({ "currency": "USD", "input": 2.0, "output": 10.0 })),
            )
            .await?;
        if st != 201 {
            bail!("setting the price answered {st}: {}", short(&v.to_string()));
        }
        let (st, v) = self.api("POST", &format!("/models/{id}/transition"), Some(json!({ "to": "active" }))).await?;
        if st != 200 {
            bail!("activating {name} answered {st}: {}", short(&v.to_string()));
        }
        self.create(&format!("/orgs/{}/grants", sc.org), json!({ "model_id": id })).await?;
        Ok(id)
    }

    /// As [`Self::model`], for a provider whose model probe may not work yet: when activation
    /// fails, the probe's answer is returned as a note and the model is activated without it.
    async fn model_unprobed(
        &self,
        sc: &Scene,
        provider: &str,
        name: &str,
        upstream: &str,
        preset: &str,
    ) -> anyhow::Result<(String, Option<String>)> {
        let body =
            json!({ "name": name, "provider_id": provider, "upstream_id": upstream, "capability_preset": preset });
        let id = Self::id(&self.create("/models", body).await?)?;
        let (st, v) = self
            .api(
                "PUT",
                &format!("/models/{id}/price"),
                Some(json!({ "currency": "USD", "input": 2.0, "output": 10.0 })),
            )
            .await?;
        if st != 201 {
            bail!("setting the price answered {st}: {}", short(&v.to_string()));
        }
        let path = format!("/models/{id}/transition");
        let (st, v) = self.api("POST", &path, Some(json!({ "to": "active" }))).await?;
        let note = if st == 200 {
            None
        } else {
            let (st2, v2) = self.api("POST", &path, Some(json!({ "to": "active", "skip_probe": true }))).await?;
            if st2 != 200 {
                bail!("activating {name} answered {st2}: {}", short(&v2.to_string()));
            }
            Some(format!("probe refused ({st}: {}); activated without it", short(&v.to_string())))
        };
        self.create(&format!("/orgs/{}/grants", sc.org), json!({ "model_id": id })).await?;
        Ok((id, note))
    }

    async fn template(&self, sc: &Scene, name: &str, spec: Value) -> anyhow::Result<()> {
        self.create(&format!("/orgs/{}/workspaces/{}/templates", sc.org, sc.ws), json!({ "name": name, "spec": spec }))
            .await?;
        Ok(())
    }

    // --- data plane ---

    async fn call(&self, key: &str, path: &str, headers: &[(&str, &str)], body: Value) -> anyhow::Result<Reply> {
        let mut req = self.http.post(format!("{}{path}", self.opt.data)).header("content-type", "application/json");
        // Anthropic's SDK sends the key in x-api-key; the gateway accepts it there too.
        req = if headers.iter().any(|(k, _)| *k == "anthropic-version") {
            req.header("x-api-key", key)
        } else {
            req.bearer_auth(key)
        };
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        self.reply(path, req.body(body.to_string())).await
    }

    /// A call about a file or a batch at Anthropic's interface: any method, any body.
    async fn object_call(
        &self,
        key: &str,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<(&str, Vec<u8>)>,
    ) -> anyhow::Result<Reply> {
        let mut req = self.http.request(method, format!("{}{path}", self.opt.data)).header("x-api-key", key);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        if let Some((content_type, bytes)) = body {
            req = req.header("content-type", content_type).body(bytes);
        }
        self.reply(path, req).await
    }

    async fn reply(&self, path: &str, req: reqwest::RequestBuilder) -> anyhow::Result<Reply> {
        let r = req.send().await?;
        let reply = Reply { status: r.status().as_u16(), headers: r.headers().clone(), text: r.text().await? };
        if self.leaks(&reply.text) || reply.headers.values().any(|v| v.to_str().is_ok_and(|v| self.leaks(v))) {
            bail!("the answer of {path} contains a provider key");
        }
        Ok(reply)
    }

    fn expect_status(r: &Reply, what: &str) -> anyhow::Result<Value> {
        if r.status != 200 {
            bail!("{what}: status {} ({}): {}", r.status, r.header("x-gw-error-type").unwrap_or("-"), short(&r.text));
        }
        r.json()
    }

    /// Files and message batches through the gateway (concept section 8.3), at the Anthropic
    /// provider named `provider`, with `model` for the message and the batch's one item. It
    /// stays small for live runs: one tiny file, and a batch that is cancelled at once.
    async fn objects(&mut self, sc: &Scene, provider: &str, model: &str) {
        let headers = [("anthropic-version", "2023-06-01"), ("x-gw-provider", provider)];
        let owned = |listed: &Value, id: &str| {
            listed["items"].as_array().and_then(|items| items.iter().find(|o| o["id"] == id)).cloned()
        };
        let result: anyhow::Result<String> = async {
            let boundary = format!("smoke-{}", self.suffix);
            let body = format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"smoke.txt\"\r\n\
                 Content-Type: text/plain\r\n\r\nThe smoke test's file, run {}.\r\n--{boundary}--\r\n",
                self.suffix
            );
            let content_type = format!("multipart/form-data; boundary={boundary}");
            let up = self
                .object_call(&sc.key, Method::POST, "/v1/files", &headers, Some((&content_type, body.into_bytes())))
                .await?;
            let id = Self::expect_status(&up, "upload")?["id"].as_str().context("the upload has no id")?.to_owned();
            let listed = self.object_call(&sc.key, Method::GET, "/v1/files", &headers, None).await?;
            let listed = Self::expect_status(&listed, "list")?;
            if !listed["data"].as_array().is_some_and(|d| d.iter().any(|f| f["id"] == id.as_str())) {
                bail!("the list does not show the file: {}", short(&listed.to_string()));
            }
            let message = json!({ "model": model, "max_tokens": 16, "messages": [{ "role": "user", "content": [
                { "type": "document", "source": { "type": "file", "file_id": id } },
                { "type": "text", "text": "Reply with the single word: pong" }
            ] }] });
            let used = self.call(&sc.key, "/v1/messages", &headers[..1], message).await?;
            Self::expect_ok(&used)?;
            let deleted = self.object_call(&sc.key, Method::DELETE, &format!("/v1/files/{id}"), &headers, None).await?;
            Self::expect_status(&deleted, "delete")?;
            let (_, listed) = self.api("GET", &format!("/orgs/{}/workspaces/{}/files", sc.org, sc.ws), None).await?;
            let row = owned(&listed, &id).context("the management API does not list the file")?;
            if row["status"] != "deleted" || row["key_id"] != sc.key_id.as_str() {
                bail!("the management API lists it as {} of key {}", row["status"], row["key_id"]);
            }
            Ok("uploaded, listed, named in a message, deleted; listed as deleted under its key".into())
        }
        .await;
        self.record("files: upload, list, use in a message, delete", result);

        let result: anyhow::Result<String> = async {
            let body = json!({ "requests": [{ "custom_id": "smoke-1", "params": {
                "model": model, "max_tokens": 16,
                "messages": [{ "role": "user", "content": "Reply with the single word: pong" }] } }] });
            let created = self
                .object_call(
                    &sc.key,
                    Method::POST,
                    "/v1/messages/batches",
                    &headers,
                    Some(("application/json", body.to_string().into_bytes())),
                )
                .await?;
            let id = Self::expect_status(&created, "create")?["id"].as_str().context("the batch has no id")?.to_owned();
            let path = format!("/v1/messages/batches/{id}");
            Self::expect_status(&self.object_call(&sc.key, Method::GET, &path, &headers, None).await?, "read")?;
            let cancel = self.object_call(&sc.key, Method::POST, &format!("{path}/cancel"), &headers, None).await?;
            let status =
                Self::expect_status(&cancel, "cancel")?["processing_status"].as_str().unwrap_or("?").to_owned();
            let (_, listed) = self.api("GET", &format!("/orgs/{}/workspaces/{}/batches", sc.org, sc.ws), None).await?;
            let row = owned(&listed, &id).context("the management API does not list the batch")?;
            if row["key_id"] != sc.key_id.as_str() {
                bail!("the management API lists it under key {}", row["key_id"]);
            }
            Ok(format!("one item, created and cancelled ({status}); listed under its key"))
        }
        .await;
        self.record("batches: create, read, cancel", result);
    }

    fn expect_ok(r: &Reply) -> anyhow::Result<()> {
        if r.status != 200 {
            bail!(
                "status {} ({}): {}",
                r.status,
                r.header("x-gw-error-type").unwrap_or("no gateway error type"),
                short(&r.text)
            );
        }
        for h in ["x-gw-request-id", "x-gw-attempts", "x-gw-delay-ms", "x-gw-model-served"] {
            if r.header(h).is_none() {
                bail!("the response has no {h} header");
            }
        }
        Ok(())
    }

    fn delay(r: &Reply) -> String {
        format!("gateway delay {} ms", r.header("x-gw-delay-ms").unwrap_or("?"))
    }

    /// Waits until the request log has `expected` calls of the workspace, then checks them.
    async fn events(&mut self, sc: &Scene, expected: usize) {
        let name = "request log has every call with its stages";
        if !self.opt.check_events {
            self.skip(name, "not asked for (--events)");
            return;
        }
        let started = Instant::now();
        let result: anyhow::Result<String> = async {
            loop {
                let (st, v) = self.api("GET", &format!("/requests?workspace_id={}&limit=100", sc.ws), None).await?;
                if st != 200 {
                    bail!("the request log answered {st}: {}", short(&v.to_string()));
                }
                let rows = v["requests"].as_array().cloned().unwrap_or_default();
                if rows.len() >= expected {
                    for r in &rows {
                        for field in ["gateway_delay_ms", "pre_ms", "post_ms", "total_ms", "upstream_first_byte_ms"] {
                            if !r[field].is_number() {
                                bail!("an event has no {field}");
                            }
                        }
                    }
                    let ok = rows.iter().filter(|r| r["status"] == "ok").count();
                    let priced = rows.iter().filter(|r| r["cost"].as_f64().is_some_and(|c| c > 0.0)).count();
                    let exact = rows.iter().filter(|r| r["usage_source"] == "provider").count();
                    if priced == 0 {
                        bail!("no event carries a cost");
                    }
                    // The native calls name their agent, user, task and complexity.
                    let attributed = rows
                        .iter()
                        .filter(|r| {
                            r["agent_id"] == "smoke-agent"
                                && r["user_id"] == "smoke-staff"
                                && r["task"] == "greet"
                                && r["complexity"] == "low"
                        })
                        .count();
                    if attributed < 2 {
                        bail!("{attributed} of the 2 native calls carry their agent, user, task and complexity");
                    }
                    return Ok(format!(
                        "{} events, {ok} ok, {exact} with provider usage, {priced} with a cost, \
                         {attributed} attributed to an agent",
                        rows.len()
                    ));
                }
                if started.elapsed() > Duration::from_secs(30) {
                    bail!("only {} of {expected} events after 30 s", rows.len());
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        .await;
        self.record(name, result);
    }

    /// The audit record of the call `id` names, once it has been stored.
    async fn audit_record(&self, id: &str) -> anyhow::Result<Value> {
        let started = Instant::now();
        loop {
            let (st, v) = self.api("GET", &format!("/requests/{id}/audit"), None).await?;
            if st == 200 {
                return Ok(v);
            }
            if started.elapsed() > Duration::from_secs(30) {
                bail!("no audit record after 30 s (status {st}): {}", short(&v.to_string()));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Makes one call that asks to be recorded and reads the record back (concept section 14.6).
    async fn audited(&mut self, sc: &Scene, headers: &[(&str, &str)], messages: &Value) {
        let name = "audit: a recorded call shows the four bodies and what the gateway changed";
        if !self.opt.check_events {
            self.skip(name, "not asked for (--events)");
            return;
        }
        let result: anyhow::Result<String> = async {
            let mut headers = headers.to_vec();
            headers.push(("x-gw-audit", "true"));
            // A trace of its own, to export it with its record afterwards.
            let trace = format!("smoke-audit-{}", sc.org);
            headers.push(("x-gw-trace", trace.as_str()));
            let body = json!({ "model": "@planner", "max_tokens": 8000, "messages": messages });
            let r = self.call(&sc.key, "/v1/messages", &headers, body).await?;
            Self::expect_ok(&r)?;
            let id = r.header("x-gw-request-id").unwrap_or_default().to_owned();
            let started = Instant::now();
            let record = self.audit_record(&id).await?;
            let text = record.to_string();
            if self.leaks(&text) || text.contains(&sc.key) {
                bail!("the audit record contains a credential");
            }
            let asked = record["client_request"]["body"].as_str().unwrap_or_default();
            let sent = record["provider_requests"][0]["body"].as_str().unwrap_or_default();
            if !asked.contains("\"@planner\"") || !asked.contains("8000") {
                bail!("the caller's request is not recorded as sent: {}", short(asked));
            }
            // The template named the model and lowered the output limit: both show in what was sent.
            if sent.contains("@planner") || !sent.contains("claude-mock") || !sent.contains("400") {
                bail!("the provider's request does not show what the gateway changed: {}", short(sent));
            }
            for part in ["provider_responses", "client_response"] {
                let p = if part == "client_response" { &record[part] } else { &record[part][0] };
                if p["status"] != 200 || (p["body"].as_str().unwrap_or_default().is_empty() && p["same_as"] == "") {
                    bail!("the record has no {part}");
                }
            }
            if !record["resolution"].is_object() || !record["timeline"].is_object() || !record["outcome"].is_object() {
                bail!("the record lacks its resolution, timeline or outcome");
            }
            // The trace as JSON lines, with the record. The usage event ships from its own
            // spool, so it may arrive after the record.
            let export_until = Instant::now() + Duration::from_secs(15);
            let (st, export) = loop {
                let (st, export) =
                    self.request("GET", &format!("/requests/export?trace_id={trace}&audit=true"), None).await?;
                if st != 404 || Instant::now() > export_until {
                    break (st, export);
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            };
            let lines: Vec<Value> = match export {
                Value::String(text) => text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
                one @ Value::Object(_) => vec![one],
                _ => Vec::new(),
            };
            if st != 200 || lines.len() != 1 || lines[0]["audit"]["audit_id"] != record["audit_id"] {
                bail!("exporting the trace answered {st} with {} lines", lines.len());
            }
            Ok(format!(
                "asked {} bytes, sent {} bytes, exported, after {:.1} s",
                record["client_request"]["body_bytes"],
                record["provider_requests"][0]["body_bytes"],
                started.elapsed().as_secs_f64()
            ))
        }
        .await;
        self.record(name, result);
    }

    /// Revokes the key and checks that it stops working within two seconds.
    async fn revocation(&mut self, sc: &Scene, path: &str, headers: &[(&str, &str)], body: Value) {
        let result: anyhow::Result<String> = async {
            // Without waiting: how long the revocation takes to reach the data plane is what
            // this measures.
            let (st, v) = self
                .request("POST", &format!("/orgs/{}/workspaces/{}/keys/{}/revoke", sc.org, sc.ws, sc.key_id), None)
                .await?;
            if st >= 300 {
                bail!("revoking answered {st}: {}", short(&v.to_string()));
            }
            let started = Instant::now();
            loop {
                let r = self.call(&sc.key, path, headers, body.clone()).await?;
                if r.status == 401 {
                    return Ok(format!("refused after {} ms", started.elapsed().as_millis()));
                }
                if started.elapsed() > Duration::from_secs(2) {
                    bail!("the revoked key still works after 2 s (status {})", r.status);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        .await;
        self.record("a revoked key stops working within 2 s", result);
    }

    /// The price tracker end to end: the job is pointed at the mock's price list and asked
    /// to run; the analytics service fetches the list, and the core applies the small change
    /// and proposes the large one.
    async fn price_run(&mut self, sc: &Scene, provider: &str, mock: &str) {
        let name = "price tracker: a run applies and proposes";
        let (steady, jump) =
            (format!("smoke-{}/tracked-steady", self.suffix), format!("smoke-{}/tracked-jump", self.suffix));
        let result: anyhow::Result<String> = async {
            // Two models whose current price came from an import, so the tracker may change it.
            let mut ids = Vec::new();
            for (model, upstream) in [(&steady, "tracked-steady"), (&jump, "tracked-jump")] {
                let body = json!({ "name": model, "provider_id": provider, "upstream_id": upstream });
                ids.push(Self::id(&self.create("/models", body).await?)?);
            }
            let (st, v) = self
                .api(
                    "POST",
                    "/prices/import",
                    Some(json!({ "currency": "USD", "prices": [
                        { "model": steady, "input": 2.0, "output": 10.0 },
                        { "model": jump, "input": 2.0, "output": 10.0 }
                    ] })),
                )
                .await?;
            if st != 200 {
                bail!("the import answered {st}: {}", short(&v.to_string()));
            }
            // The tracker looks after models that are in use.
            for id in &ids {
                let activate = json!({ "to": "active" });
                let (st, v) = self.api("POST", &format!("/models/{id}/transition"), Some(activate)).await?;
                if st != 200 {
                    bail!("activating a tracked model answered {st}: {}", short(&v.to_string()));
                }
            }
            // Runs that are already on record are not the one this check waits for.
            let (_, earlier) = self.api("GET", "/price-job/runs?limit=200", None).await?;
            let known: Vec<String> =
                earlier.as_array().into_iter().flatten().filter_map(|r| r["id"].as_str().map(str::to_owned)).collect();
            let (_, before) = self.api("GET", "/price-job", None).await?;
            // Keep what an operator configured, add the mock's list for this run's provider.
            let mut job = before.clone();
            for field in ["last_started_at", "next_run_at", "run_requested", "running"] {
                job.as_object_mut().context("the price job is not an object")?.remove(field);
            }
            let mut sources = job["sources"].as_array().cloned().unwrap_or_default();
            sources
                .push(json!({ "provider_id": provider, "kind": "feed", "url": format!("{mock}/__mock/prices.json") }));
            job["sources"] = json!(sources);
            let (st, v) = self.api("PUT", "/price-job", Some(job)).await?;
            if st != 200 {
                bail!("configuring the price job answered {st}: {}", short(&v.to_string()));
            }
            let (st, v) = self.api("POST", "/price-job/run", None).await?;
            if st != 202 {
                bail!("\"run now\" answered {st}: {}", short(&v.to_string()));
            }
            // The analytics service asks for work twice a minute.
            let started = Instant::now();
            let run = loop {
                let (_, runs) = self.api("GET", "/price-job/runs?limit=5", None).await?;
                let done = runs
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|r| {
                        r["trigger"] == "manual"
                            && r["finished_at"].is_string()
                            && r["id"].as_str().is_some_and(|id| !known.iter().any(|k| k == id))
                    })
                    .cloned();
                if let Some(run) = done {
                    break run;
                }
                if started.elapsed() > Duration::from_secs(90) {
                    bail!("no finished run after 90 s; is the analytics service running with PGW_CORE_ADMIN_URL set?");
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            };
            let outcome = |model: &str| {
                run["result"]["models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|m| m["model"] == model)
                    .map(|m| m["outcome"].as_str().unwrap_or_default().to_owned())
            };
            if outcome(&steady).as_deref() != Some("applied") || outcome(&jump).as_deref() != Some("proposed") {
                bail!(
                    "expected applied and proposed, got {:?} and {:?}: {}",
                    outcome(&steady),
                    outcome(&jump),
                    short(&run["result"].to_string())
                );
            }
            let (_, proposals) = self.api("GET", "/price-proposals", None).await?;
            let open = proposals.as_array().map_or(0, |p| p.iter().filter(|x| x["model"] == jump.as_str()).count());
            Ok(format!(
                "+10 % applied, +100 % proposed ({open} open proposal), after {} s",
                started.elapsed().as_secs()
            ))
        }
        .await;
        let _ = sc;
        self.record(name, result);
    }

    /// DeepSeek, GLM, Kimi, Ollama, xAI, Gemini (both APIs) and Vertex AI's express mode, each set up from its preset
    /// against the mock's emulation of it, and called through all three interfaces. Returns the
    /// number of calls made.
    async fn compatible_providers(&mut self, sc: &Scene, mock: &str, secret: &str) -> usize {
        let cases = [
            ("deepseek", "/emulate/deepseek", "deepseek-flash", "deepseek-vision"),
            ("glm", "/emulate/glm/api/paas/v4", "glm-4.7", "glm-4"),
            ("kimi", "/emulate/kimi/v1", "kimi-k3", "kimi-k3"),
            ("xai", "/emulate/xai/v1", "grok-4", "xai-reasoning"),
            ("ollama", "/emulate/ollama/v1", "qwen3", "ollama-thinking"),
            ("gemini-openai", "/emulate/gemini-openai/v1beta/openai", "gemini-3.8-flash", "gemini-3-flash"),
            ("gemini", "/emulate/gemini/v1beta", "gemini-3.8-flash", "gemini-3-flash"),
            // Vertex AI's express mode: an API key, so no token to mint against Google.
            ("vertex-express", "/emulate/vertex/v1/publishers/google", "gemini-3.8-flash", "gemini-3-flash"),
        ];
        let mut calls = 0;
        let result: anyhow::Result<String> = async {
            let (_, presets) = self.api("GET", "/provider-presets", None).await?;
            let mut served = Vec::new();
            for (preset, path, upstream, capabilities) in cases {
                let p = presets
                    .as_array()
                    .and_then(|list| list.iter().find(|p| p["name"] == preset))
                    .with_context(|| format!("no preset {preset}"))?;
                let name = format!("{preset}-{}", self.suffix);
                let mut body = json!({ "name": name, "family": p["family"], "base_url": format!("{mock}{path}"), "quirks": p["quirks"] });
                if let Some(auth) = p.get("auth") {
                    body["auth"] = auth.clone();
                }
                body["managed_api"] = p["managed_api"].clone();
                body["platform"] = p["platform"].clone();
                let id = Self::id(&self.create("/providers", body).await?)?;
                self.create(&format!("/providers/{id}/credentials"), json!({ "label": "smoke", "secret": secret })).await?;
                let model = format!("smoke-{}/{preset}", self.suffix);
                self.model(sc, &id, &model, upstream, capabilities).await?;
                let prompt = json!([{ "role": "user", "content": "hello" }]);
                let a = self.call(&sc.key, "/v1/chat/completions", &[], json!({ "model": model, "messages": prompt })).await?;
                Self::expect_ok(&a)?;
                if !a.json()?["usage"]["cost"]["amount"].is_number() {
                    bail!("{preset}: no cost in /v1/chat/completions: {}", short(&a.text));
                }
                let b = self
                    .call(
                        &sc.key,
                        "/v1/messages",
                        &[("anthropic-version", "2023-06-01")],
                        json!({ "model": model, "max_tokens": 200, "messages": prompt }),
                    )
                    .await?;
                Self::expect_ok(&b)?;
                if b.json()?["type"] != "message" {
                    bail!("{preset}: /v1/messages: {}", short(&b.text));
                }
                let c = self
                    .call(&sc.key, "/gw/v1/chat", &[], json!({ "model": model, "stream": true, "messages": prompt }))
                    .await?;
                Self::expect_ok(&c)?;
                if !c.text.contains("event: message.stop") {
                    bail!("{preset}: /gw/v1/chat: {}", short(&c.text));
                }
                calls += 3;
                served.push(preset);
            }
            Ok(format!("{} through three interfaces each", served.join(", ")))
        }
        .await;
        self.record("compatible providers from their presets", result);
        calls
    }

    // --- the mock profile ---

    /// The full flow against the mock provider.
    pub async fn run_mock(&mut self) -> anyhow::Result<()> {
        let sc = self.scene().await.context("setting up the organisation, workspace and key")?;
        let mock = self.opt.mock_url.trim_end_matches('/').to_owned();
        let secret = format!("smoke-provider-secret-{}", self.suffix);
        self.secrets.push(secret.clone());
        let (openai, openai_name) =
            self.provider("openai", &format!("{mock}/v1"), &secret).await.context("the OpenAI-dialect provider")?;
        let (anthropic, anthropic_name) =
            self.provider("anthropic", &mock, &secret).await.context("the Anthropic provider")?;
        let gpt = format!("smoke-{}/gpt-mock", self.suffix);
        let claude = format!("smoke-{}/claude-mock", self.suffix);
        let setup: anyhow::Result<String> = async {
            self.model(&sc, &openai, &gpt, "gpt-mock", "openai-chat").await?;
            self.model(&sc, &anthropic, &claude, "claude-mock", "anthropic-budget").await?;
            self.template(
                &sc,
                "planner",
                json!({ "model": claude, "max_output_tokens": 400, "tags": { "purpose": "planner" } }),
            )
            .await?;
            self.template(&sc, "writer", json!({ "model": gpt, "temperature": { "value": 0.3 } })).await?;
            Ok("two providers, two models (probed and priced), two templates".into())
        }
        .await;
        if !self.record("models pass their gates and are granted", setup) {
            return Ok(());
        }
        let user = json!([{ "role": "user", "content": "Say hello to the smoke test." }]);
        let anthropic_headers = [("anthropic-version", "2023-06-01")];
        let mut calls = 0;

        let r = self
            .call(
                &sc.key,
                &format!("/proxy/{openai_name}/v1/chat/completions"),
                &[],
                json!({ "model": "gpt-mock", "messages": user }),
            )
            .await;
        let result = r.and_then(|r| {
            Self::expect_ok(&r)?;
            let v = r.json()?;
            if v["usage"].get("cost").is_some() {
                bail!("proxy mode changed the response");
            }
            Ok(Self::delay(&r))
        });
        calls += 1;
        self.record("proxy: OpenAI dialect, relayed unchanged", result);

        let r = self
            .call(
                &sc.key,
                &format!("/proxy/{anthropic_name}/v1/messages"),
                &anthropic_headers,
                json!({ "model": "@planner", "max_tokens": 8000, "stream": true, "messages": user }),
            )
            .await;
        let result = r.and_then(|r| {
            Self::expect_ok(&r)?;
            if !r.text.contains("event: message_stop") {
                bail!("the stream did not end with message_stop");
            }
            if r.header("x-gw-model-served") != Some(claude.as_str()) {
                bail!("served by {:?}", r.header("x-gw-model-served"));
            }
            Ok(Self::delay(&r))
        });
        calls += 1;
        self.record("proxy: Anthropic stream through @planner", result);

        let r = self
            .call(
                &sc.key,
                "/v1/chat/completions",
                &[("x-gw-end-user", "smoke-user")],
                json!({ "model": "@writer", "messages": user, "gateway": { "tags": { "purpose": "writer" } } }),
            )
            .await;
        let result = r.and_then(|r| {
            Self::expect_ok(&r)?;
            let v = r.json()?;
            let cost = v["usage"]["cost"]["amount"].as_f64().context("usage.cost.amount is missing")?;
            if r.header("x-gw-cost").is_none() {
                bail!("no x-gw-cost header");
            }
            Ok(format!("cost {cost:.6} USD, {}", Self::delay(&r)))
        });
        calls += 1;
        self.record("managed: chat completion with cost", result);

        let r = self
            .call(
                &sc.key,
                "/v1/chat/completions",
                &[],
                json!({ "model": gpt, "stream": true, "stream_options": { "include_usage": true }, "messages": user }),
            )
            .await;
        let result = r.and_then(|r| {
            Self::expect_ok(&r)?;
            if !r.text.trim_end().ends_with("data: [DONE]") {
                bail!("the stream did not end with [DONE]");
            }
            if !r.text.contains("\"cost\":{") {
                bail!("the usage chunk carries no cost");
            }
            Ok(Self::delay(&r))
        });
        calls += 1;
        self.record("managed: stream with cost on the usage chunk", result);

        // A limit of one request per day for each end user: the second call is refused.
        let limits = format!("/orgs/{}/workspaces/{}/limits", sc.org, sc.ws);
        let result: anyhow::Result<String> = async {
            self.create(&limits, json!({ "name": "smoke-daily", "subject": "end_user", "metric": "requests", "window": "day", "amount": 1.0 })).await?;
            let body = json!({ "model": gpt, "messages": user });
            let first = self.call(&sc.key, "/v1/chat/completions", &[("x-gw-end-user", "limited")], body.clone()).await?;
            Self::expect_ok(&first)?;
            // With several gateways the second call may land on another one, which learns of
            // the first through PostgreSQL within about a second (runbook section 9).
            if self.data_plane_instances().await > 1 {
                tokio::time::sleep(Duration::from_millis(2500)).await;
            }
            let second = self.call(&sc.key, "/v1/chat/completions", &[("x-gw-end-user", "limited")], body).await?;
            if second.status != 402 || second.header("x-gw-error-type") != Some("quota_exceeded") {
                bail!("the second call answered {} {:?}", second.status, second.header("x-gw-error-type"));
            }
            let q = self.http.get(format!("{}/gw/v1/quota?end_user=limited", self.opt.data)).bearer_auth(&sc.key).send().await?.json::<Value>().await?;
            let used = q["limits"][0]["used"].as_f64().context("the quota state has no limits")?;
            Ok(format!("402 on the second call, quota shows {used} used"))
        }
        .await;
        calls += 2;
        self.record("limits: admission, 402 and the quota state", result);

        let result: anyhow::Result<String> = async {
            let v = self
                .http
                .get(format!("{}/v1/models", self.opt.data))
                .bearer_auth(&sc.key)
                .send()
                .await?
                .json::<Value>()
                .await?;
            let ids: Vec<&str> =
                v["data"].as_array().context("no data")?.iter().filter_map(|m| m["id"].as_str()).collect();
            for want in [gpt.as_str(), claude.as_str(), "@planner", "@writer"] {
                if !ids.contains(&want) {
                    bail!("{want} is missing from {ids:?}");
                }
            }
            Ok(format!("{} entries", ids.len()))
        }
        .await;
        self.record("the model list shows models and templates", result);

        // The same two models from the other interfaces: Anthropic's own, and translations.
        let r = self
            .call(
                &sc.key,
                "/v1/messages",
                &anthropic_headers,
                json!({ "model": "@planner", "max_tokens": 8000, "messages": user }),
            )
            .await;
        let result = r.and_then(|r| {
            Self::expect_ok(&r)?;
            let v = r.json()?;
            if v["type"] != "message" || r.header("x-gw-cost").is_none() {
                bail!("not Anthropic's answer with the cost in a header: {}", short(&r.text));
            }
            Ok(Self::delay(&r))
        });
        calls += 1;
        self.record("managed: Anthropic interface, Anthropic model", result);

        let result: anyhow::Result<String> = async {
            let to_claude = self
                .call(
                    &sc.key,
                    "/v1/chat/completions",
                    &[],
                    json!({ "model": claude, "max_tokens": 64, "messages": user }),
                )
                .await?;
            Self::expect_ok(&to_claude)?;
            let v = to_claude.json()?;
            if v["object"] != "chat.completion" || !v["usage"]["cost"]["amount"].is_number() {
                bail!("the Anthropic model did not answer in OpenAI's shape: {}", short(&to_claude.text));
            }
            let to_gpt = self
                .call(
                    &sc.key,
                    "/v1/messages",
                    &anthropic_headers,
                    json!({ "model": gpt, "max_tokens": 64, "messages": user }),
                )
                .await?;
            Self::expect_ok(&to_gpt)?;
            if to_gpt.json()?["type"] != "message" {
                bail!("the OpenAI-dialect model did not answer in Anthropic's shape: {}", short(&to_gpt.text));
            }
            Ok("OpenAI dialect to the Anthropic model, Anthropic dialect to the OpenAI one".into())
        }
        .await;
        calls += 2;
        self.record("translated: each dialect reaches the other family", result);

        let result: anyhow::Result<String> = async {
            let mut seen = Vec::new();
            for model in [&claude, &gpt] {
                let body = json!({ "model": model, "max_output_tokens": 64, "stream": true,
                                   "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Say hello to the smoke test." }] }],
                                   "gateway": { "agent_id": "smoke-agent", "user_id": "smoke-staff", "task": "greet", "complexity": "low" } });
                let r = self.call(&sc.key, "/gw/v1/chat", &[], body).await?;
                Self::expect_ok(&r)?;
                let stop = r.text.rsplit("data: ").next().unwrap_or_default();
                let stop: Value = serde_json::from_str(stop.trim()).context("the last event is not JSON")?;
                if !r.text.contains("event: message.stop") || !stop["usage"]["cost"]["amount"].is_number() {
                    bail!("the stream did not end with usage and cost: {}", short(&r.text));
                }
                seen.push(stop["gateway"]["provider"].as_str().unwrap_or_default().to_owned());
            }
            Ok(format!("streams from {}", seen.join(" and ")))
        }
        .await;
        calls += 2;
        self.record("native: one interface in front of both families", result);

        calls += self.compatible_providers(&sc, &mock, &secret).await;

        // A template with two models and a scheme: a rule picks the tier, and a failing model
        // hands the call to the next one, in the other dialect.
        let result: anyhow::Result<String> = async {
            self.create(
                &format!("/orgs/{}/routing-schemes", sc.org),
                json!({ "name": "smoke-tiers", "rules": [
                    { "name": "easy", "when": { "complexity": ["low"] }, "then": { "tiers": ["small"], "overflow": "rest_of_pool" } }
                ], "default": { "tiers": ["large", "small"] } }),
            )
            .await?;
            self.template(
                &sc,
                "router",
                json!({ "routing_scheme": "smoke-tiers", "models": [
                    { "model": claude, "tier": "large" }, { "model": gpt, "tier": "small" } ] }),
            )
            .await?;
            let body = json!({ "model": "@router", "max_tokens": 64, "messages": user });
            let easy = self.call(&sc.key, "/v1/chat/completions", &[("x-gw-complexity", "low")], body.clone()).await?;
            Self::expect_ok(&easy)?;
            if easy.header("x-gw-route") != Some("smoke-tiers/easy") || easy.header("x-gw-model-served") != Some(gpt.as_str()) {
                bail!("a low-complexity call went {:?} to {:?}", easy.header("x-gw-route"), easy.header("x-gw-model-served"));
            }
            if self.opt.mock_control.is_empty() {
                return Ok("a rule chose the small tier; failover not tried (no --mock-control-url)".into());
            }
            let control = self.opt.mock_control.trim_end_matches('/');
            let script = self.http.post(format!("{control}/__mock/script")).json(&json!(["status=503"])).send().await?;
            if !script.status().is_success() {
                bail!("the mock refused the script: {}", script.status());
            }
            let failing = self.call(&sc.key, "/v1/chat/completions", &[], body).await?;
            Self::expect_ok(&failing)?;
            if failing.header("x-gw-failovers") != Some("1") || failing.header("x-gw-model-served") != Some(gpt.as_str()) {
                bail!(
                    "the 503 of the large model was not handed on: failovers {:?}, served by {:?}",
                    failing.header("x-gw-failovers"),
                    failing.header("x-gw-model-served")
                );
            }
            if failing.json()?["object"] != "chat.completion" {
                bail!("the answer after the failover is not OpenAI's shape: {}", short(&failing.text));
            }
            Ok("a rule chose the small tier; a 503 of the large model moved the call to the small one".into())
        }
        .await;
        calls += 2;
        self.record("routing: rules choose, failover hands on", result);

        // The response cache (docs/plugin-concept.md, section 15), bound for the scene's
        // workspace, since a new installation has it off: the same call at temperature 0 twice;
        // the second is answered without the provider.
        let result: anyhow::Result<String> = async {
            self.create(
                &format!("/orgs/{}/workspaces/{}/plugin-bindings", sc.org, sc.ws),
                json!({ "plugin": "response_cache", "mode": "enforce", "settings": {} }),
            )
            .await
            .context("binding the response cache (is it installed?)")?;
            let question = format!("Remember this answer, smoke test {}.", self.suffix);
            let body = json!({ "model": gpt, "temperature": 0, "messages": [{ "role": "user", "content": question }] });
            let first = self.call(&sc.key, "/v1/chat/completions", &[], body.clone()).await?;
            let v = Self::expect_status(&first, "the first call")?;
            match first.header("x-gw-cache") {
                Some("miss") => {}
                None => bail!("no x-gw-cache header: the response cache is not bound here (switched off?)"),
                Some(other) => bail!("the first call was {other}, not a miss"),
            }
            // The answer is kept once the call has settled, off the request path.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let again = self.call(&sc.key, "/v1/chat/completions", &[], body).await?;
            let w = Self::expect_status(&again, "the repeated call")?;
            if again.header("x-gw-cache") != Some("hit") || again.header("x-gw-attempts") != Some("0") {
                bail!(
                    "the repeated call was {:?} after {:?} attempts, not a hit",
                    again.header("x-gw-cache"),
                    again.header("x-gw-attempts")
                );
            }
            if w["choices"][0]["message"]["content"] != v["choices"][0]["message"]["content"] {
                bail!("the kept answer differs: {}", short(&again.text));
            }
            Ok(format!("a hit {} s old, without the provider", again.header("age").unwrap_or("?")))
        }
        .await;
        calls += 2;
        self.record("response cache: a repeated call at temperature 0 is answered from it", result);

        self.responses_mock(&sc, &mock, &secret, &claude).await;
        calls += 2;
        calls += self.stored_responses_mock(&sc, &claude).await;

        self.system_one_mock(&sc, &mock, &secret).await;
        calls += 2;

        self.images_mock(&sc, &openai).await;
        calls += 2;

        self.transcription_mock(&sc, &openai).await;
        calls += 1;

        self.rerank_mock(&sc, &mock, &secret).await;
        calls += 1;

        self.realtime_mock(&sc, &openai).await;
        calls += 1;

        let r = crate::ws::round_trip(
            &self.opt.data,
            &format!("/proxy/{openai_name}/__ws/echo?model=gpt-mock"),
            &[("authorization", &format!("Bearer {}", sc.key))],
            "hello over a WebSocket",
        )
        .await
        .and_then(|(answer, _)| {
            anyhow::ensure!(answer == "hello over a WebSocket", "the echo answered {answer:?}");
            Ok("relayed both ways with the provider's credential".to_owned())
        });
        calls += 1;
        self.record("proxy: a WebSocket session", r);

        self.intent_mock(&sc, &gpt, &mock, &secret).await;

        self.audited(&sc, &anthropic_headers, &user).await;
        if self.opt.check_events {
            calls += 1;
        }
        self.price_run(&sc, &openai, &mock).await;
        self.events(&sc, calls).await;
        // After the request-log check, which counts the calls above.
        self.objects(&sc, &anthropic_name, &claude).await;
        self.openai_objects(&sc, &mock, &secret).await;
        self.revocation(&sc, "/v1/chat/completions", &[], json!({ "model": gpt, "messages": user })).await;
        Ok(())
    }

    // --- the intent plugin ---

    /// Installs the intent plugin, approving TypeSafe, or approves it on the install there is.
    async fn install_intent(&self) -> anyhow::Result<()> {
        let body = json!({ "plugin": "intent", "third_parties": ["typesafe"] });
        let (st, v) = self.api("POST", "/plugin-installs", Some(body.clone())).await?;
        match st {
            201 => Ok(()),
            409 => {
                let (_, list) = self.api("GET", "/plugin-installs", None).await?;
                let item = list["items"]
                    .as_array()
                    .and_then(|items| items.iter().find(|i| i["plugin"] == "intent"))
                    .context("the intent plugin's install is not listed")?;
                let mut doc = item.clone();
                doc["enabled"] = json!(true);
                let parties = doc["third_parties"].as_array().cloned().unwrap_or_default();
                if !parties.iter().any(|p| p == "typesafe") {
                    doc["third_parties"] = json!([parties, vec![json!("typesafe")]].concat());
                }
                let (st, v) = self.api("PUT", &format!("/plugin-installs/{}", Self::id(item)?), Some(doc)).await?;
                if st != 200 {
                    bail!("updating the intent plugin's install answered {st}: {}", short(&v.to_string()));
                }
                Ok(())
            }
            _ => bail!("installing the intent plugin answered {st}: {}", short(&v.to_string())),
        }
    }

    /// A workspace of the scene's organisation with an intent catalogue and a binding of its
    /// own, apart from the scene's workspace whose calls other checks count. Returns the
    /// workspace's id and a key of it.
    async fn intent_workspace(&self, sc: &Scene, mode: &str, settings: Value) -> anyhow::Result<(String, String)> {
        self.install_intent().await?;
        let ws = Self::id(&self.create(&format!("/orgs/{}/workspaces", sc.org), json!({ "name": "intent" })).await?)?;
        self.create(
            &format!("/orgs/{}/workspaces/{ws}/intent-catalogues", sc.org),
            json!({
                "name": "smoke-support",
                "intents": [
                    { "name": "billing", "description": "Charges, invoices and refunds.", "examples": ["I was charged twice"] },
                    { "name": "technical", "description": "Errors and how to use the product." },
                    { "name": "legal_advice", "description": "Advice on a person's legal situation." }
                ],
                "rules": [{ "name": "no-legal", "when": { "intents": ["legal_advice"] },
                            "then": { "action": "block", "message": "No legal advice here." } }]
            }),
        )
        .await?;
        let mut settings = settings;
        settings["catalogue"] = json!("smoke-support");
        self.create(
            &format!("/orgs/{}/workspaces/{ws}/plugin-bindings", sc.org),
            json!({ "plugin": "intent", "mode": mode, "expose": true, "settings": settings }),
        )
        .await?;
        let key = self.create(&format!("/orgs/{}/workspaces/{ws}/keys", sc.org), json!({ "name": "intent" })).await?;
        Ok((ws, key["secret"].as_str().context("the new key has no secret")?.to_owned()))
    }

    /// A decision model of TypeSafe, priced by input tokens and granted to the organisation.
    async fn decision_model(&self, sc: &Scene, provider: &str, name: &str, upstream: &str) -> anyhow::Result<String> {
        let body = json!({ "name": name, "provider_id": provider, "upstream_id": upstream,
                           "kind": "decision", "capability_preset": "decision" });
        let id = Self::id(&self.create("/models", body).await?)?;
        let price = json!({ "currency": "USD", "input": 0.042, "output": 0.0 });
        let (st, v) = self.api("PUT", &format!("/models/{id}/price"), Some(price)).await?;
        if st != 201 {
            bail!("setting the price answered {st}: {}", short(&v.to_string()));
        }
        let (st, v) = self.api("POST", &format!("/models/{id}/transition"), Some(json!({ "to": "active" }))).await?;
        if st != 200 {
            bail!("activating {name} answered {st}: {}", short(&v.to_string()));
        }
        self.create(&format!("/orgs/{}/grants", sc.org), json!({ "model_id": id })).await?;
        Ok(id)
    }

    /// OpenAI's Responses API: streamed from a provider that serves it, and answered whole by
    /// Claude through translation, both stateless and with their cost.
    async fn responses_mock(&mut self, sc: &Scene, mock: &str, secret: &str, claude: &str) {
        let result: anyhow::Result<String> = async {
            let quirks = json!({ "quirks": { "responses_api": true } });
            let (provider, _) =
                self.provider_named("openai-responses", "openai", &format!("{mock}/v1"), secret, quirks).await?;
            let gpt = format!("smoke-{}/gpt-responses", self.suffix);
            self.model(sc, &provider, &gpt, "gpt-mock", "openai-chat").await?;
            let streamed = self
                .call(&sc.key, "/v1/responses", &[], json!({ "model": gpt, "input": "hello", "stream": true }))
                .await?;
            Self::expect_ok(&streamed)?;
            let last = streamed.text.split("\n\n").filter(|e| !e.trim().is_empty()).last().unwrap_or_default();
            if !last.starts_with("event: response.completed") || !last.contains("\"cost\"") {
                bail!("the stream did not end with response.completed and its cost: {}", short(last));
            }
            let whole = self
                .call(
                    &sc.key,
                    "/v1/responses",
                    &[],
                    json!({ "model": claude, "instructions": "Be brief.", "input": "hello" }),
                )
                .await?;
            let v = Self::expect_status(&whole, "the translated call")?;
            if v["object"] != "response" || v["store"] != false || v["output"][0]["type"] != "message" {
                bail!("Claude's answer is not a response object: {}", short(&whole.text));
            }
            Ok("relayed as a stream, and translated to Claude".into())
        }
        .await;
        self.record("Responses API: stateless, relayed and translated", result);
    }

    /// Stored responses: the workspace switched on, a turn at the Responses provider continued
    /// at Claude and back, read, a conversation given a turn, and both deleted. Returns how many
    /// calls it made.
    async fn stored_responses_mock(&mut self, sc: &Scene, claude: &str) -> usize {
        let mut calls = 0;
        let gpt = format!("smoke-{}/gpt-responses", self.suffix);
        let result: anyhow::Result<String> = async {
            let path = format!("/orgs/{}/workspaces/{}", sc.org, sc.ws);
            let (_, mut doc) = self.api("GET", &path, None).await?;
            doc["settings"]["responses"] = json!({ "store": "on" });
            let (st, v) = self.api("PUT", &path, Some(doc)).await?;
            if st != 200 || v["settings"]["responses"]["store"] != "on" {
                bail!("switching stored responses on answered {st}: {}", short(&v.to_string()));
            }
            self.wait_applied().await;

            // A turn at the provider of the Responses API, streamed: kept under the gateway's id.
            let first = self
                .call(&sc.key, "/v1/responses", &[], json!({ "model": gpt, "input": "one", "stream": true }))
                .await?;
            calls += 1;
            Self::expect_ok(&first)?;
            let last = first.text.split("\n\n").filter(|e| !e.trim().is_empty()).last().unwrap_or_default();
            let data: Value = last
                .lines()
                .find_map(|l| l.strip_prefix("data: "))
                .and_then(|d| serde_json::from_str(d).ok())
                .unwrap_or_default();
            let id = data["response"]["id"].as_str().unwrap_or_default().to_owned();
            if data["response"]["store"] != true || !id.starts_with("resp_") {
                bail!("the stream's last event names no kept response: {}", short(last));
            }
            // Continued at Claude right away: it is given the first turn again.
            let second = self
                .call(
                    &sc.key,
                    "/v1/responses",
                    &[],
                    json!({ "model": claude, "input": "two", "previous_response_id": id }),
                )
                .await?;
            calls += 1;
            let v = Self::expect_status(&second, "the continued call")?;
            if v["previous_response_id"] != id.as_str() || v["store"] != true {
                bail!("Claude's answer does not continue the first turn: {}", short(&second.text));
            }
            let second_id = v["id"].as_str().unwrap_or_default().to_owned();
            // Read back, then deleted.
            let read = self.object_call(&sc.key, Method::GET, &format!("/v1/responses/{second_id}"), &[], None).await?;
            calls += 1;
            if read.status != 200 || Self::expect_status(&read, "reading it back")?["id"] != second_id.as_str() {
                bail!("reading the kept response answered {}: {}", read.status, short(&read.text));
            }
            let deleted = self.object_call(&sc.key, Method::DELETE, &format!("/v1/responses/{id}"), &[], None).await?;
            calls += 1;
            if deleted.status != 200 {
                bail!("deleting the kept response answered {}: {}", deleted.status, short(&deleted.text));
            }
            // A conversation, given a turn.
            let conv = self
                .call(&sc.key, "/v1/conversations", &[], json!({ "items": [{ "role": "user", "content": "hi" }] }))
                .await?;
            calls += 1;
            let conv =
                Self::expect_status(&conv, "creating a conversation")?["id"].as_str().unwrap_or_default().to_owned();
            let turn = self
                .call(&sc.key, "/v1/responses", &[], json!({ "model": claude, "input": "three", "conversation": conv }))
                .await?;
            calls += 1;
            Self::expect_status(&turn, "a turn of the conversation")?;
            let items =
                self.object_call(&sc.key, Method::GET, &format!("/v1/conversations/{conv}/items"), &[], None).await?;
            calls += 1;
            let n = Self::expect_status(&items, "listing the conversation")?["data"].as_array().map_or(0, Vec::len);
            if n != 3 {
                bail!("the conversation holds {n} items, not 3: {}", short(&items.text));
            }
            let gone =
                self.object_call(&sc.key, Method::DELETE, &format!("/v1/conversations/{conv}"), &[], None).await?;
            calls += 1;
            if gone.status != 200 {
                bail!("deleting the conversation answered {}: {}", gone.status, short(&gone.text));
            }
            Ok("kept, continued at Claude, read and deleted; a conversation given a turn".into())
        }
        .await;
        self.record("Responses API: stored responses and conversations", result);
        calls
    }

    /// OpenAI's Files and Batches: a batch input file written for the provider at upload, a
    /// batch run from it, and its output read through the gateway.
    async fn openai_objects(&mut self, sc: &Scene, mock: &str, secret: &str) {
        let result: anyhow::Result<String> = async {
            let quirks = json!({ "quirks": { "openai_objects": true } });
            let (provider, name) =
                self.provider_named("openai-objects", "openai", &format!("{mock}/v1"), secret, quirks).await?;
            let gpt = format!("smoke-{}/gpt-batch", self.suffix);
            self.model(sc, &provider, &gpt, "gpt-mock", "openai-chat").await?;
            let line = json!({ "custom_id": "one", "method": "POST", "url": "/v1/chat/completions",
                               "body": { "model": gpt, "messages": [{ "role": "user", "content": "hello" }] } });
            let boundary = "smoke-batch";
            let form = format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\nbatch\r\n\
                 --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"in.jsonl\"\r\n\r\n{line}\n\r\n--{boundary}--\r\n"
            );
            let req = self
                .http
                .post(format!("{}/v1/files", self.opt.data))
                .bearer_auth(&sc.key)
                .header("x-gw-provider", &name)
                // The workspace records files and batches on request: the batch's items too.
                .header("x-gw-audit", "true")
                .header("content-type", format!("multipart/form-data; boundary={boundary}"))
                .body(form);
            let file = Self::expect_status(&self.reply("/v1/files", req).await?, "the upload")?;
            let file_id = file["id"].as_str().context("no file id")?.to_owned();
            let batch = self
                .call(
                    &sc.key,
                    "/v1/batches",
                    &[],
                    json!({ "input_file_id": file_id, "endpoint": "/v1/chat/completions", "completion_window": "24h" }),
                )
                .await?;
            let batch = Self::expect_status(&batch, "the batch")?;
            let id = batch["id"].as_str().context("no batch id")?.to_owned();
            let get = self.http.get(format!("{}/v1/batches/{id}", self.opt.data)).bearer_auth(&sc.key);
            let ended = Self::expect_status(&self.reply("/v1/batches/{id}", get).await?, "reading the batch")?;
            let output = ended["output_file_id"].as_str().context("the batch has no output file")?.to_owned();
            let get = self.http.get(format!("{}/v1/files/{output}/content", self.opt.data)).bearer_auth(&sc.key);
            let content = self.reply("/v1/files/{id}/content", get).await?;
            if content.status != 200 || !content.text.contains("\"custom_id\":\"one\"") {
                bail!("the output could not be read: {} {}", content.status, short(&content.text));
            }
            if !self.opt.check_events {
                return Ok(format!(
                    "batch {} ran on a file written for the provider; its output reads through the gateway",
                    ended["status"]
                ));
            }
            // Once settled, the batch's request is recorded with its result: the trace is the batch.
            let until = Instant::now() + Duration::from_secs(90);
            let record = loop {
                let (st, export) =
                    self.request("GET", &format!("/requests/export?trace_id={id}&audit=true"), None).await?;
                let lines: Vec<Value> = match export {
                    Value::String(text) => text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
                    one @ Value::Object(_) => vec![one],
                    _ => Vec::new(),
                };
                if st == 200
                    && let Some(r) = lines.iter().find_map(|l| l.get("audit").filter(|a| a.is_object()))
                {
                    break r.clone();
                }
                if Instant::now() > until {
                    bail!("the batch's request was not recorded within 90 s (status {st})");
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            };
            let text = record.to_string();
            if self.leaks(&text) || text.contains(&sc.key) {
                bail!("the item's audit record contains a credential");
            }
            let asked = record["client_request"]["body"].as_str().unwrap_or_default();
            let sent = record["provider_requests"][0]["body"].as_str().unwrap_or_default();
            let answered = record["provider_responses"][0]["body"].as_str().unwrap_or_default();
            if !asked.contains(&gpt) || !sent.contains("gpt-mock") || !answered.contains("\"custom_id\":\"one\"") {
                bail!("the item's record does not hold its request and result: {}", short(&text));
            }
            Ok(format!(
                "batch {} ran on a file written for the provider; its output reads through the gateway; its request is \
                 recorded with its result",
                ended["status"]
            ))
        }
        .await;
        self.record("OpenAI files and batches: upload, batch, output", result);
    }

    /// A managed Realtime session with one response, priced by its tokens.
    async fn realtime_mock(&mut self, sc: &Scene, openai: &str) {
        let result: anyhow::Result<String> = async {
            let model = format!("smoke-{}/realtime", self.suffix);
            let body = json!({ "name": model, "provider_id": openai, "upstream_id": "gpt-realtime-mini",
                               "kind": "realtime", "capability_preset": "realtime" });
            let id = Self::id(&self.create("/models", body).await?)?;
            let price = json!({ "currency": "USD", "input": 0.6, "cache_read": 0.06, "input_audio": 10.0,
                                "output": 2.4, "output_audio": 20.0 });
            let (st, v) = self.api("PUT", &format!("/models/{id}/price"), Some(price)).await?;
            if st != 201 {
                bail!("setting the price answered {st}: {}", short(&v.to_string()));
            }
            let (st, v) =
                self.api("POST", &format!("/models/{id}/transition"), Some(json!({ "to": "active" }))).await?;
            if st != 200 {
                bail!("activating {model} answered {st}: {}", short(&v.to_string()));
            }
            self.create(&format!("/orgs/{}/grants", sc.org), json!({ "model_id": id })).await?;
            let path = format!("/v1/realtime?model={model}");
            // The session asks to be recorded: the workspace records Realtime sessions on request.
            let (done, id) = crate::ws::realtime_response(
                &self.opt.data,
                &path,
                &[("authorization", &format!("Bearer {}", sc.key)), ("x-gw-audit", "true")],
            )
            .await?;
            if done["response"]["status"] != "completed" || done["response"]["usage"]["output_tokens"] != 60 {
                bail!("not the mock's response with its usage: {}", short(&done.to_string()));
            }
            if !self.opt.check_events {
                return Ok("one response, its usage read from response.done".to_owned());
            }
            // The handshake's request id names the first response, and its record.
            let record = self.audit_record(&id.context("the handshake named no request")?).await?;
            let text = record.to_string();
            if self.leaks(&text) || text.contains(&sc.key) {
                bail!("the session's audit record contains a credential");
            }
            let asked = record["client_request"]["body"].as_str().unwrap_or_default();
            let answered = record["provider_responses"][0]["body"].as_str().unwrap_or_default();
            if !asked.lines().any(|l| l.contains("\"response.create\""))
                || !answered.lines().last().is_some_and(|l| l.contains("\"response.done\""))
            {
                bail!("the session's record does not hold its events, one per line: {}", short(&text));
            }
            Ok("one response, its usage read from response.done, its events in its audit record".to_owned())
        }
        .await;
        self.record("realtime: a managed session", result);
    }

    /// Rerank at a Cohere provider, priced per search unit.
    async fn rerank_mock(&mut self, sc: &Scene, mock: &str, secret: &str) {
        let result: anyhow::Result<String> = async {
            let (cohere, _) = self.provider_named("cohere", "cohere", mock, secret, json!({})).await?;
            let model = format!("smoke-{}/rerank", self.suffix);
            let body = json!({ "name": model, "provider_id": cohere, "upstream_id": "rerank-v3.5",
                               "kind": "rerank", "capability_preset": "rerank" });
            let id = Self::id(&self.create("/models", body).await?)?;
            let (st, v) = self
                .api(
                    "PUT",
                    &format!("/models/{id}/price"),
                    Some(json!({ "currency": "USD", "per_search_unit": 0.002 })),
                )
                .await?;
            if st != 201 {
                bail!("setting the price answered {st}: {}", short(&v.to_string()));
            }
            let (st, v) =
                self.api("POST", &format!("/models/{id}/transition"), Some(json!({ "to": "active" }))).await?;
            if st != 200 {
                bail!("activating {model} answered {st}: {}", short(&v.to_string()));
            }
            self.create(&format!("/orgs/{}/grants", sc.org), json!({ "model_id": id })).await?;
            let r = self
                .call(
                    &sc.key,
                    "/v1/rerank",
                    &[],
                    json!({ "model": model, "query": "capital of France", "top_n": 1,
                            "documents": ["Berlin is a city", "Paris is the capital of France"] }),
                )
                .await?;
            let v = Self::expect_status(&r, "the rerank")?;
            if v["results"][0]["index"] != 1 || r.header("x-gw-cost").is_none() {
                bail!("not the expected order, or no cost: {}", short(&r.text));
            }
            Ok(format!("Paris first, at {}", r.header("x-gw-cost").unwrap_or("?")))
        }
        .await;
        self.record("rerank: Cohere, priced per search unit", result);
    }

    /// A transcription on Whisper, priced by the seconds of audio the provider reports.
    async fn transcription_mock(&mut self, sc: &Scene, openai: &str) {
        let result: anyhow::Result<String> = async {
            let model = format!("smoke-{}/whisper", self.suffix);
            let body = json!({ "name": model, "provider_id": openai, "upstream_id": "whisper-1",
                               "kind": "transcription", "capability_preset": "transcription" });
            let id = Self::id(&self.create("/models", body).await?)?;
            let (st, v) = self
                .api("PUT", &format!("/models/{id}/price"), Some(json!({ "currency": "USD", "per_audio_second": 0.0001 })))
                .await?;
            if st != 201 {
                bail!("setting the price answered {st}: {}", short(&v.to_string()));
            }
            let (st, v) = self.api("POST", &format!("/models/{id}/transition"), Some(json!({ "to": "active" }))).await?;
            if st != 200 {
                bail!("activating {model} answered {st}: {}", short(&v.to_string()));
            }
            self.create(&format!("/orgs/{}/grants", sc.org), json!({ "model_id": id })).await?;
            let boundary = "smoke-audio";
            let mut form = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n")
                .into_bytes();
            form.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes(),
            );
            form.extend_from_slice(&[0u8; 32_000]);
            form.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
            let req = self
                .http
                .post(format!("{}/v1/audio/transcriptions", self.opt.data))
                .bearer_auth(&sc.key)
                .header("content-type", format!("multipart/form-data; boundary={boundary}"))
                .body(form);
            let r = self.reply("/v1/audio/transcriptions", req).await?;
            let v = Self::expect_status(&r, "the transcription")?;
            if v["text"] != "Transcript of hello.wav" || v["usage"]["seconds"] != 2 {
                bail!("not the transcript with its duration: {}", short(&r.text));
            }
            Ok(format!("2 s of audio at {}", r.header("x-gw-cost").unwrap_or("?")))
        }
        .await;
        self.record("transcription: Whisper, priced by the second", result);
    }

    /// Image generation and an edit (a form) on gpt-image, both priced by their tokens.
    async fn images_mock(&mut self, sc: &Scene, openai: &str) {
        let result: anyhow::Result<String> = async {
            let model = format!("smoke-{}/gpt-image", self.suffix);
            let body = json!({ "name": model, "provider_id": openai, "upstream_id": "gpt-image-1",
                               "kind": "image", "capability_preset": "image" });
            let id = Self::id(&self.create("/models", body).await?)?;
            let price = json!({ "currency": "USD", "input": 5.0, "output": 40.0 });
            let (st, v) = self.api("PUT", &format!("/models/{id}/price"), Some(price)).await?;
            if st != 201 {
                bail!("setting the price answered {st}: {}", short(&v.to_string()));
            }
            let (st, v) = self.api("POST", &format!("/models/{id}/transition"), Some(json!({ "to": "active" }))).await?;
            if st != 200 {
                bail!("activating {model} answered {st}: {}", short(&v.to_string()));
            }
            self.create(&format!("/orgs/{}/grants", sc.org), json!({ "model_id": id })).await?;
            let made = self.call(&sc.key, "/v1/images/generations", &[], json!({ "model": model, "prompt": "a cat" })).await?;
            let v = Self::expect_status(&made, "the generation")?;
            // Relayed as gpt-image wrote it: the usage is the provider's, the cost in the request log.
            if !v["data"][0]["b64_json"].is_string() || !v["usage"]["output_tokens"].is_u64() {
                bail!("no image or no usage: {}", short(&made.text));
            }
            let boundary = "smoke-boundary";
            let mut form = Vec::new();
            for (name, value) in [("model", model.as_str()), ("prompt", "add a hat")] {
                form.extend_from_slice(
                    format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
                );
            }
            form.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"cat.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes(),
            );
            form.extend_from_slice(b"\x89PNG\r\n\x1a\nCAT\r\n");
            form.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            let req = self
                .http
                .post(format!("{}/v1/images/edits", self.opt.data))
                .bearer_auth(&sc.key)
                .header("content-type", format!("multipart/form-data; boundary={boundary}"))
                .body(form);
            let edited = self.reply("/v1/images/edits", req).await?;
            let v = Self::expect_status(&edited, "the edit")?;
            if !v["data"][0]["b64_json"].is_string() {
                bail!("the edit made no image: {}", short(&edited.text));
            }
            Ok(format!("a generation of {} tokens and an edit", v["usage"]["output_tokens"]))
        }
        .await;
        self.record("images: a generation and an edit, priced by their tokens", result);
    }

    /// An application's own calls to System One: managed at `/v1/systemone`, and relayed by
    /// proxy mode, both with usage and cost.
    async fn system_one_mock(&mut self, sc: &Scene, mock: &str, secret: &str) {
        let result: anyhow::Result<String> = async {
            let (typesafe, name) =
                self.provider_named("typesafe-apps", "typesafe", &format!("{mock}/v1"), secret, json!({})).await?;
            let jev = format!("smoke-{}/jev-apps", self.suffix);
            self.decision_model(sc, &typesafe, &jev, "jev-1.13.0").await?;
            let ask = |model: &str| {
                json!({
                    "model": model,
                    "state": [{ "from": "user", "text": "I was charged twice, a billing question" }],
                    "questions": { "intent": { "type": "choice", "instructions": "What does the user want?",
                        "criteria": { "billing": { "what": "Charges and refunds." }, "unknown": { "what": "Other." } } } }
                })
            };
            let managed = self.call(&sc.key, "/v1/systemone", &[], ask(&jev)).await?;
            let v = Self::expect_status(&managed, "the managed call")?;
            if v["answers"]["intent"]["choice"] != "billing" {
                bail!("the managed call answered {}", short(&managed.text));
            }
            let proxied = self.call(&sc.key, &format!("/proxy/{name}/v1/systemone"), &[], ask("jev-1.13.0")).await?;
            let w = Self::expect_status(&proxied, "the proxied call")?;
            if w["answers"]["intent"]["choice"] != "billing" {
                bail!("the proxied call answered {}", short(&proxied.text));
            }
            Ok("billing, managed and through proxy mode".into())
        }
        .await;
        self.record("System One: an application asks Jev, managed and proxied", result);
    }

    /// The intent plugin against the mock: the LLM engine blocks and labels calls, and Jev
    /// answers the try endpoint.
    async fn intent_mock(&mut self, sc: &Scene, gpt: &str, mock: &str, secret: &str) {
        let result: anyhow::Result<String> = async {
            let (ws, key) =
                self.intent_workspace(sc, "enforce", json!({ "engine": "llm", "llm": { "model": gpt } })).await?;
            let blocked = self
                .call(
                    &key,
                    "/v1/chat/completions",
                    &[],
                    json!({ "model": gpt, "messages": [
                    { "role": "user", "content": "I need legal advice about my lease" } ] }),
                )
                .await?;
            if blocked.status != 403 || blocked.header("x-gw-error-type") != Some("intent_blocked") {
                bail!("a call for legal advice answered {} {:?}", blocked.status, blocked.header("x-gw-error-type"));
            }
            let served = self
                .call(
                    &key,
                    "/v1/chat/completions",
                    &[],
                    json!({ "model": gpt, "messages": [
                    { "role": "user", "content": "a billing question about my invoice" } ] }),
                )
                .await?;
            Self::expect_ok(&served)?;
            if served.header("x-gw-intent") != Some("billing") {
                bail!("the billing call was labelled {:?}", served.header("x-gw-intent"));
            }
            let (typesafe, _) =
                self.provider_named("typesafe", "typesafe", &format!("{mock}/v1"), secret, json!({})).await?;
            let jev = format!("smoke-{}/jev", self.suffix);
            self.decision_model(sc, &typesafe, &jev, "jev-1.13.0").await?;
            let path = format!("/orgs/{}/workspaces/{ws}/plugin-bindings", sc.org);
            let (_, list) = self.api("GET", &path, None).await?;
            let binding = list["items"].as_array().and_then(|i| i.first()).context("no binding")?.clone();
            let mut doc = binding.clone();
            doc["settings"]["jev"] = json!({ "model": jev });
            let (st, v) = self.api("PUT", &format!("{path}/{}", Self::id(&binding)?), Some(doc)).await?;
            if st != 200 {
                bail!("adding Jev to the binding answered {st}: {}", short(&v.to_string()));
            }
            let (st, v) = self
                .api(
                    "POST",
                    &format!("/orgs/{}/workspaces/{ws}/plugins/intent/try", sc.org),
                    Some(json!({ "text": "I was charged twice, a billing question", "engines": ["llm", "jev"] })),
                )
                .await?;
            if st != 200 {
                bail!("the try endpoint answered {st}: {}", short(&v.to_string()));
            }
            let found: Vec<String> = v["answers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| format!("{} {}", a["engine"].as_str().unwrap_or("?"), a["intent"].as_str().unwrap_or("error")))
                .collect();
            if found != ["llm billing", "jev billing"] {
                bail!("the engines answered {found:?}");
            }
            Ok("the LLM engine blocked legal advice and labelled billing; Jev agreed on the try endpoint".into())
        }
        .await;
        self.record("intent plugin: blocks, labels, and both engines answer", result);
    }

    /// One real Jev classification through the try endpoint, when TypeSafe's key is set.
    async fn intent_live(&mut self, sc: &Scene, key: Option<String>) {
        let name = "intent plugin: Jev classifies a text";
        let Some(secret) = key else {
            self.skip(name, "TYPESAFE_API_KEY is not set");
            return;
        };
        let result: anyhow::Result<String> = async {
            let (typesafe, _) = self.provider("typesafe", "https://api.typesafe.ai/v1", &secret).await?;
            let jev = format!("live-{}/jev-1.13.0", self.suffix);
            self.decision_model(sc, &typesafe, &jev, "jev-1.13.0").await?;
            let (ws, _) =
                self.intent_workspace(sc, "shadow", json!({ "engine": "jev", "jev": { "model": jev } })).await?;
            let (st, v) = self
                .api(
                    "POST",
                    &format!("/orgs/{}/workspaces/{ws}/plugins/intent/try", sc.org),
                    Some(json!({ "text": "I was charged twice for order A-104 and want my money back" })),
                )
                .await?;
            if st != 200 {
                bail!("the try endpoint answered {st}: {}", short(&v.to_string()));
            }
            let a = &v["answers"][0];
            if let Some(e) = a["error"].as_str() {
                bail!("Jev gave no answer: {e}");
            }
            let intent = a["intent"].as_str().unwrap_or_default();
            if intent != "billing" {
                bail!("Jev answered {intent:?} for a double charge");
            }
            Ok(format!(
                "billing, confidence {:.2}, {} ms, model {}",
                a["confidence"].as_f64().unwrap_or_default(),
                a["elapsed_ms"].as_f64().unwrap_or_default().round(),
                a["detail"]["model"].as_str().unwrap_or("?")
            ))
        }
        .await;
        self.record(name, result);
    }

    // --- the live profile ---

    /// Picks a small model from a provider's own model list, relayed by the gateway.
    async fn discover(
        &self,
        sc: &Scene,
        provider_name: &str,
        headers: &[(&str, &str)],
        prefer: &[&str],
        wanted: Option<String>,
    ) -> anyhow::Result<String> {
        if let Some(w) = wanted {
            return Ok(w);
        }
        let mut req = self.http.get(format!("{}/proxy/{provider_name}/v1/models", self.opt.data));
        req = if headers.is_empty() { req.bearer_auth(&sc.key) } else { req.header("x-api-key", &sc.key) };
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let r = req.send().await?;
        let status = r.status();
        let text = r.text().await?;
        if self.leaks(&text) {
            bail!("the provider's model list came back with a provider key in it");
        }
        if !status.is_success() {
            bail!("the provider's model list answered {status}: {}", short(&text));
        }
        let v: Value = serde_json::from_str(&text)?;
        let ids: Vec<&str> =
            v["data"].as_array().context("no model list")?.iter().filter_map(|m| m["id"].as_str()).collect();
        prefer
            .iter()
            .find_map(|p| ids.iter().find(|id| id.contains(p)))
            .map(|id| (*id).to_owned())
            .ok_or_else(|| anyhow!("none of {prefer:?} is among the provider's {} models", ids.len()))
    }

    /// A few tiny calls per provider with real keys. A provider without a key is skipped.
    pub async fn run_live(&mut self, keys: LiveKeys, models: LiveModels) -> anyhow::Result<()> {
        self.secrets.extend(keys.all());
        let (openai_key, anthropic_key) = (keys.openai.clone(), keys.anthropic.clone());
        let (openai_model, anthropic_model) = (models.openai.clone(), models.anthropic.clone());
        let sc = self.scene().await.context("setting up the organisation, workspace and key")?;
        let prompt = json!([{ "role": "user", "content": "Reply with the single word: pong" }]);
        let mut calls = 0;
        // The repository ids of the models that were set up, for the cross-interface checks.
        let (mut live_openai, mut live_anthropic): (Option<String>, Option<String>) = (None, None);
        let mut anthropic_provider: Option<String> = None;

        match openai_key {
            None => self.skip("OpenAI", "OPENAI_API_KEY is not set"),
            Some(secret) => {
                let setup: anyhow::Result<(String, String, String)> = async {
                    // As the preset sets it up: OpenAI serves the Responses API.
                    let (provider, name) = self
                        .provider_named(
                            "openai",
                            "openai",
                            "https://api.openai.com/v1",
                            &secret,
                            json!({ "quirks": { "responses_api": true } }),
                        )
                        .await?;
                    let upstream = self
                        .discover(
                            &sc,
                            &name,
                            &[],
                            &["gpt-4.1-nano", "gpt-4o-mini", "gpt-4.1-mini", "gpt-5-nano", "nano", "mini"],
                            openai_model,
                        )
                        .await?;
                    let reasoning = upstream.starts_with("gpt-5") || upstream.starts_with('o');
                    let model = format!("live-{}/{upstream}", self.suffix);
                    self.model(
                        &sc,
                        &provider,
                        &model,
                        &upstream,
                        if reasoning { "openai-reasoning" } else { "openai-chat" },
                    )
                    .await?;
                    Ok((name, upstream, model))
                }
                .await;
                match setup {
                    Err(e) => {
                        self.record("OpenAI: provider, model and probe", Err(e));
                    }
                    Ok((name, upstream, model)) => {
                        self.record("OpenAI: provider, model and probe", Ok(format!("model {upstream}")));
                        let limit = if upstream.starts_with("gpt-5") || upstream.starts_with('o') {
                            "max_completion_tokens"
                        } else {
                            "max_tokens"
                        };
                        let mut body = json!({ "model": upstream, "messages": prompt });
                        body[limit] = json!(64);
                        let r =
                            self.call(&sc.key, &format!("/proxy/{name}/v1/chat/completions"), &[], body.clone()).await;
                        let result = r.and_then(|r| {
                            Self::expect_ok(&r)?;
                            let tokens = r.json()?["usage"]["total_tokens"].as_u64().context("no usage")?;
                            Ok(format!("{tokens} tokens, {}", Self::delay(&r)))
                        });
                        self.record("OpenAI: proxy call", result);
                        body["stream"] = json!(true);
                        body["stream_options"] = json!({ "include_usage": true });
                        let r = self.call(&sc.key, &format!("/proxy/{name}/v1/chat/completions"), &[], body).await;
                        let result = r.and_then(|r| {
                            Self::expect_ok(&r)?;
                            if !r.text.contains("data: [DONE]") {
                                bail!("the stream did not finish");
                            }
                            Ok(Self::delay(&r))
                        });
                        self.record("OpenAI: proxy stream", result);
                        let r = self
                            .call(
                                &sc.key,
                                "/v1/chat/completions",
                                &[],
                                json!({ "model": model, "max_tokens": 64, "messages": prompt }),
                            )
                            .await;
                        let result = r.and_then(|r| {
                            Self::expect_ok(&r)?;
                            let v = r.json()?;
                            let cost = v["usage"]["cost"]["amount"].as_f64().context("usage.cost.amount is missing")?;
                            Ok(format!("cost {cost:.6} USD at the smoke test's own price, {}", Self::delay(&r)))
                        });
                        self.record("OpenAI: managed call with cost", result);
                        let r = self
                            .call(
                                &sc.key,
                                "/v1/responses",
                                &[],
                                json!({ "model": model, "max_output_tokens": 64, "input": "Reply with the single word: pong" }),
                            )
                            .await;
                        let result = r.and_then(|r| {
                            let v = Self::expect_status(&r, "the Responses call")?;
                            let cost = v["usage"]["cost"]["amount"].as_f64().context("usage.cost.amount is missing")?;
                            Ok(format!("status {}, cost {cost:.6} USD, {}", v["status"], Self::delay(&r)))
                        });
                        self.record("OpenAI: Responses API, relayed statelessly", result);
                        calls += 4;
                        live_openai = Some(model);
                    }
                }
            }
        }

        match anthropic_key {
            None => self.skip("Anthropic", "ANTHROPIC_API_KEY is not set"),
            Some(secret) => {
                let headers = [("anthropic-version", "2023-06-01")];
                let setup: anyhow::Result<(String, String, String)> = async {
                    let (provider, name) = self.provider("anthropic", "https://api.anthropic.com", &secret).await?;
                    let upstream = self.discover(&sc, &name, &headers, &["haiku"], anthropic_model).await?;
                    let preset = if upstream.contains("haiku-4-5") || upstream.contains("-3-") {
                        "anthropic-budget"
                    } else {
                        "anthropic-adaptive"
                    };
                    let model = format!("live-{}/{upstream}", self.suffix);
                    self.model(&sc, &provider, &model, &upstream, preset).await?;
                    self.template(&sc, "live-planner", json!({ "model": model, "max_output_tokens": 32 })).await?;
                    Ok((name, upstream, model))
                }
                .await;
                match setup {
                    Err(e) => {
                        self.record("Anthropic: provider, model and probe", Err(e));
                    }
                    Ok((name, upstream, model)) => {
                        live_anthropic = Some(model.clone());
                        anthropic_provider = Some(name.clone());
                        self.record("Anthropic: provider, model and probe", Ok(format!("model {upstream}")));
                        let path = format!("/proxy/{name}/v1/messages");
                        let r = self
                            .call(
                                &sc.key,
                                &path,
                                &headers,
                                json!({ "model": upstream, "max_tokens": 32, "messages": prompt }),
                            )
                            .await;
                        let result = r.and_then(|r| {
                            Self::expect_ok(&r)?;
                            let v = r.json()?;
                            let out = v["usage"]["output_tokens"].as_u64().context("no usage")?;
                            Ok(format!("{out} output tokens, {}", Self::delay(&r)))
                        });
                        self.record("Anthropic: proxy call", result);
                        let r = self
                            .call(&sc.key, &path, &headers, json!({ "model": "@live-planner", "max_tokens": 4000, "stream": true, "messages": prompt }))
                            .await;
                        let result = r.and_then(|r| {
                            Self::expect_ok(&r)?;
                            if !r.text.contains("event: message_stop") {
                                bail!("the stream did not finish");
                            }
                            Ok(format!("through @live-planner, {}", Self::delay(&r)))
                        });
                        self.record("Anthropic: proxy stream with a template", result);
                        let r = self
                            .call(
                                &sc.key,
                                "/v1/responses",
                                &[],
                                json!({ "model": model, "max_output_tokens": 32, "input": "Reply with the single word: pong" }),
                            )
                            .await;
                        let result = r.and_then(|r| {
                            let v = Self::expect_status(&r, "the Responses call")?;
                            if v["object"] != "response" || v["output"].as_array().is_none_or(Vec::is_empty) {
                                bail!("not a response object: {}", short(&r.text));
                            }
                            Ok(format!("translated, status {}, {}", v["status"], Self::delay(&r)))
                        });
                        self.record("Anthropic: Responses API, translated", result);
                        calls += 3;
                    }
                }
            }
        }

        // The OpenAI-compatible providers: proxy and managed calls in their own dialect. Each is
        // set up from its preset, as an administrator would: family, quirks, managed API.
        let mut live_others: Vec<(&'static str, String)> = Vec::new();
        let (_, presets) = self.api("GET", "/provider-presets", None).await?;
        let preset = |name: &str| {
            presets.as_array().and_then(|l| l.iter().find(|p| p["name"] == name)).cloned().unwrap_or(Value::Null)
        };
        let compatible = [
            (
                "DeepSeek",
                "deepseek",
                "DEEPSEEK_API_KEY",
                &keys.deepseek,
                "https://api.deepseek.com",
                &models.deepseek,
                "deepseek-flash",
            ),
            ("GLM", "glm", "ZAI_API_KEY", &keys.glm, "https://api.z.ai/api/paas/v4", &models.glm, "glm-4.7"),
            (
                "Gemini (compatible)",
                "gemini-openai",
                "GEMINI_API_KEY",
                &keys.gemini,
                "https://generativelanguage.googleapis.com/v1beta/openai",
                &models.gemini,
                "gemini-3.8-flash",
            ),
        ];
        for (label, slug, var, key, base, wanted, default_model) in compatible {
            let Some(secret) = key.clone() else {
                self.skip(label, &format!("{var} is not set"));
                continue;
            };
            let upstream = wanted.clone().unwrap_or_else(|| default_model.to_owned());
            let p = preset(slug);
            let capabilities = match slug {
                "deepseek" => "deepseek-vision",
                "glm" => "glm-4",
                _ => "gemini-3-flash",
            };
            let setup: anyhow::Result<(String, String, Option<String>)> = async {
                let family = p["family"].as_str().unwrap_or("openai").to_owned();
                let extra = json!({ "quirks": p["quirks"], "managed_api": p["managed_api"] });
                let (provider, name) = self.provider_named(slug, &family, base, &secret, extra).await?;
                let model = format!("live-{}/{slug}/{upstream}", self.suffix);
                let (_, note) = self.model_unprobed(&sc, &provider, &model, &upstream, capabilities).await?;
                Ok((name, model, note))
            }
            .await;
            let (name, model, note) = match setup {
                Err(e) => {
                    self.record(&format!("{label}: provider, model and probe"), Err(e));
                    continue;
                }
                Ok(s) => s,
            };
            self.record(
                &format!("{label}: provider, model and probe"),
                Ok(format!("model {upstream}{}", note.map(|n| format!("; {n}")).unwrap_or_default())),
            );
            // The proxy path: the provider's own path below its origin.
            let origin_path =
                base.split_once("://").and_then(|(_, rest)| rest.find('/').map(|i| &rest[i..])).unwrap_or("");
            let proxy = format!("/proxy/{name}{origin_path}/chat/completions");
            let body = json!({ "model": upstream, "max_tokens": 512, "messages": prompt });
            let r = self.call(&sc.key, &proxy, &[], body.clone()).await;
            let result = r.and_then(|r| {
                Self::expect_ok(&r)?;
                let tokens = r.json()?["usage"]["total_tokens"].as_u64().context("no usage")?;
                Ok(format!("{tokens} tokens, {}", Self::delay(&r)))
            });
            self.record(&format!("{label}: proxy call"), result);
            let mut stream = body;
            stream["stream"] = json!(true);
            stream["stream_options"] = json!({ "include_usage": true });
            let r = self.call(&sc.key, &proxy, &[], stream).await;
            let result = r.and_then(|r| {
                Self::expect_ok(&r)?;
                if !r.text.contains("data: [DONE]") {
                    bail!("the stream did not finish: {}", short(&r.text));
                }
                Ok(Self::delay(&r))
            });
            self.record(&format!("{label}: proxy stream"), result);
            let r = self
                .call(
                    &sc.key,
                    "/v1/chat/completions",
                    &[],
                    json!({ "model": model, "max_tokens": 512, "messages": prompt }),
                )
                .await;
            let result = r.and_then(|r| {
                Self::expect_ok(&r)?;
                let v = r.json()?;
                let cost = v["usage"]["cost"]["amount"].as_f64().context("usage.cost.amount is missing")?;
                Ok(format!("cost {cost:.6} USD at the smoke test's own price, {}", Self::delay(&r)))
            });
            self.record(&format!("{label}: managed call with cost"), result);
            calls += 3;
            live_others.push((label, model));
        }

        // Gemini's native API through proxy mode: the key in Google's header and in its query,
        // and both stream formats. The same provider then serves managed calls natively.
        let mut native_model: Option<String> = None;
        if let Some(secret) = keys.gemini.clone() {
            let upstream = models.gemini.clone().unwrap_or_else(|| "gemini-3.8-flash".to_owned());
            let result: anyhow::Result<String> = async {
                let (provider, name) = self
                    .provider_named(
                        "gemini-native",
                        "gemini",
                        "https://generativelanguage.googleapis.com/v1beta",
                        &secret,
                        json!({ "managed_api": "native" }),
                    )
                    .await?;
                let model = format!("live-{}/gemini-native/{upstream}", self.suffix);
                let (_, note) = self.model_unprobed(&sc, &provider, &model, &upstream, "gemini-3-flash").await?;
                native_model = Some(model.clone());
                let body = json!({ "contents": [{ "role": "user", "parts": [{ "text": "Reply with the single word: pong" }] }] });
                let send = |path: String, header: bool| {
                    let mut req = self.http.post(format!("{}{path}", self.opt.data)).header("content-type", "application/json");
                    if header {
                        req = req.header("x-goog-api-key", &sc.key);
                    }
                    req.body(body.to_string()).send()
                };
                let base = format!("/proxy/{name}/v1beta/models/{upstream}");
                let mut done = Vec::new();
                for (label, path, header) in [
                    ("generateContent", format!("{base}:generateContent"), true),
                    ("SSE stream with ?key=", format!("{base}:streamGenerateContent?alt=sse&key={}", sc.key), false),
                    ("array stream", format!("{base}:streamGenerateContent"), true),
                ] {
                    let r = send(path, header).await?;
                    let status = r.status();
                    let text = r.text().await?;
                    if self.leaks(&text) {
                        bail!("{label}: the answer contains a provider key");
                    }
                    if !status.is_success() || !text.contains("candidates") {
                        bail!("{label} answered {status}: {}", short(&text));
                    }
                    done.push(label);
                }
                Ok(format!("{}{}", done.join(", "), note.map(|n| format!("; {n}")).unwrap_or_default()))
            }
            .await;
            calls += 3;
            self.record("Gemini (native): proxy calls", result);
            if let Some(m) = native_model.take() {
                live_others.push(("Gemini (native)", m));
            }
        }

        // Each live model from all three interfaces (concept section 8.2).
        let anthropic_headers = [("anthropic-version", "2023-06-01")];
        let mut everyone: Vec<(&str, Option<String>)> =
            vec![("OpenAI", live_openai.clone()), ("Anthropic", live_anthropic.clone())];
        everyone.extend(live_others.iter().map(|(l, m)| (*l, Some(m.clone()))));
        for (provider, model) in &everyone {
            let Some(model) = model else { continue };
            let result: anyhow::Result<String> = async {
                let a = self
                    .call(
                        &sc.key,
                        "/v1/chat/completions",
                        &[],
                        json!({ "model": model, "max_tokens": 512, "messages": prompt }),
                    )
                    .await?;
                Self::expect_ok(&a)?;
                if a.json()?["object"] != "chat.completion" {
                    bail!("/v1/chat/completions: {}", short(&a.text));
                }
                let b = self
                    .call(
                        &sc.key,
                        "/v1/messages",
                        &anthropic_headers,
                        json!({ "model": model, "max_tokens": 512, "messages": prompt }),
                    )
                    .await?;
                Self::expect_ok(&b)?;
                if b.json()?["type"] != "message" {
                    bail!("/v1/messages: {}", short(&b.text));
                }
                let c = self
                    .call(
                        &sc.key,
                        "/gw/v1/chat",
                        &[],
                        json!({ "model": model, "max_output_tokens": 512, "stream": true, "messages": prompt,
                                "gateway": { "agent_id": "smoke-agent", "user_id": "smoke-staff", "task": "greet", "complexity": "low" } }),
                    )
                    .await?;
                Self::expect_ok(&c)?;
                if !c.text.contains("event: message.stop") || !c.text.contains("\"cost\":{") {
                    bail!("/gw/v1/chat: {}", short(&c.text));
                }
                Ok("chat completions, messages and the native stream".into())
            }
            .await;
            calls += 3;
            self.record(&format!("{provider}: reachable from all three interfaces"), result);
        }

        // Discovery and the probe against the real providers: their lists, with what they report.
        for (label, slug) in [("DeepSeek", "deepseek"), ("GLM", "glm"), ("Gemini (native)", "gemini-native")] {
            if find_provider(self, slug).await.is_none() {
                continue;
            }
            let result: anyhow::Result<String> = async {
                let id = find_provider(self, slug).await.context("the provider is gone")?;
                let (st, v) = self.api("GET", &format!("/providers/{id}/discovery"), None).await?;
                if st != 200 {
                    bail!("discovery answered {st}: {}", short(&v.to_string()));
                }
                let listed = v["listed"].as_array().map_or(0, Vec::len);
                let with_limits =
                    v["listed"].as_array().map_or(0, |l| l.iter().filter(|m| m["context_window"].is_u64()).count());
                if listed == 0 {
                    bail!("no models listed");
                }
                Ok(format!("{listed} models listed, {with_limits} with their context window"))
            }
            .await;
            self.record(&format!("{label}: discovery"), result);
        }

        // Embeddings through the gateway's own endpoint: relayed to OpenAI, translated for
        // Gemini's API (which reports no usage: the input is estimated).
        for (label, slug, upstream) in [
            ("OpenAI", "openai", "text-embedding-3-small"),
            ("Gemini (native)", "gemini-native", "gemini-embedding-001"),
        ] {
            let Some(provider) = find_provider(self, slug).await else { continue };
            let result: anyhow::Result<String> = async {
                let name = format!("live-{}/{slug}/embed", self.suffix);
                let body = json!({ "name": name, "provider_id": provider, "upstream_id": upstream,
                                   "kind": "embedding", "capability_preset": "embedding" });
                let id = Self::id(&self.create("/models", body).await?)?;
                let (st, v) = self
                    .api("PUT", &format!("/models/{id}/price"), Some(json!({ "currency": "USD", "input": 0.15 })))
                    .await?;
                if st != 201 {
                    bail!("setting the price answered {st}: {}", short(&v.to_string()));
                }
                let (st, v) =
                    self.api("POST", &format!("/models/{id}/transition"), Some(json!({ "to": "active" }))).await?;
                if st != 200 {
                    bail!("activating answered {st}: {}", short(&v.to_string()));
                }
                self.create(&format!("/orgs/{}/grants", sc.org), json!({ "model_id": id })).await?;
                let r = self
                    .call(
                        &sc.key,
                        "/v1/embeddings",
                        &[],
                        json!({ "model": name, "input": ["The quick brown fox.", "A lazy dog."], "dimensions": 8 }),
                    )
                    .await?;
                Self::expect_ok(&r)?;
                let v = r.json()?;
                let data = v["data"].as_array().map_or(0, Vec::len);
                let numbers = v["data"][0]["embedding"].as_array().map_or(0, Vec::len);
                let tokens = v["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
                if data != 2 || numbers != 8 || tokens == 0 {
                    bail!("/v1/embeddings: {}", short(&r.text));
                }
                Ok(format!("2 vectors of 8 numbers, {tokens} input tokens"))
            }
            .await;
            self.record(&format!("{label}: embeddings"), result);
        }
        // Counting a Gemini model's tokens through Anthropic's interface.
        if let Some(model) = live_others.iter().find(|(l, _)| *l == "Gemini (native)").map(|(_, m)| m.clone()) {
            let result: anyhow::Result<String> = async {
                let r = self
                    .call(
                        &sc.key,
                        "/v1/messages/count_tokens",
                        &[("anthropic-version", "2023-06-01")],
                        json!({ "model": model, "system": "Be brief.", "messages": [{ "role": "user", "content": "How many tokens is this?" }] }),
                    )
                    .await?;
                Self::expect_ok(&r)?;
                let n = r.json()?["input_tokens"].as_u64().unwrap_or(0);
                if n == 0 {
                    bail!("count_tokens: {}", short(&r.text));
                }
                Ok(format!("{n} input tokens, counted by Gemini"))
            }
            .await;
            self.record("Gemini (native): token counting through Anthropic's interface", result);
        }

        // Two-turn tool loops through the gateway: the reasoning or thought signature of the first
        // turn must come back on the second, or Gemini 3 and DeepSeek refuse it.
        let find = |label: &str| live_others.iter().find(|(l, _)| *l == label).map(|(_, m)| m.clone());
        for (label, iface, stream) in [
            ("Gemini (native)", "anthropic", true),
            ("Gemini (native)", "openai", false),
            ("Gemini (compatible)", "anthropic", false),
            ("Gemini (compatible)", "openai", true),
            ("DeepSeek", "anthropic", true),
            ("GLM", "anthropic", false),
        ] {
            let Some(model) = find(label) else { continue };
            let result = self.tool_loop(&sc, &model, iface, stream).await;
            calls += 2;
            self.record(&format!("{label}: a tool loop through the {iface} interface"), result.map(|(d, _)| d));
        }
        // A conversation that moves from Gemini to Claude: Claude must not get Gemini's thinking.
        if let (Some(gemini), Some(claude)) = (find("Gemini (native)"), live_anthropic.clone()) {
            let result: anyhow::Result<String> = async {
                let (_, content) = self.tool_loop(&sc, &gemini, "anthropic", false).await?;
                let use_id = content
                    .as_array()
                    .and_then(|c| c.iter().find(|b| b["type"] == "tool_use"))
                    .map(|b| b["id"].clone())
                    .context("no tool call in the first turn")?;
                let body = json!({ "model": claude, "max_tokens": 1024, "tools": [weather_tool_anthropic()], "messages": [
                    { "role": "user", "content": TOOL_PROMPT },
                    { "role": "assistant", "content": content },
                    { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": use_id, "content": TOOL_RESULT }] } ] });
                let r = self.call(&sc.key, "/v1/messages", &[("anthropic-version", "2023-06-01")], body).await?;
                Self::expect_ok(&r)?;
                let dropped = r.header("x-gw-dropped-params").unwrap_or_default().to_owned();
                if !dropped.contains("thinking") {
                    bail!("Gemini's thinking block was not left out (dropped: {dropped:?})");
                }
                Ok(format!("Claude answered; Gemini's thinking left out ({dropped})"))
            }
            .await;
            calls += 3;
            self.record("a conversation moves from Gemini to Claude", result);
        }

        // Prompt caching survives the gateway: the second identical request reads the cache.
        if let Some(model) = &live_anthropic {
            let result: anyhow::Result<String> = async {
                // Long enough to be cacheable on every current model, and new for every run.
                let system =
                    format!("Run {}. {}", self.suffix, "The quick brown fox jumps over the lazy dog. ".repeat(700));
                let body = json!({
                    "model": model, "max_tokens": 16,
                    "system": [{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }],
                    "messages": prompt
                });
                let first = self.call(&sc.key, "/v1/messages", &anthropic_headers, body.clone()).await?;
                Self::expect_ok(&first)?;
                let written = first.json()?["usage"]["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                let second = self.call(&sc.key, "/v1/messages", &anthropic_headers, body).await?;
                Self::expect_ok(&second)?;
                let read = second.json()?["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0);
                if read == 0 {
                    bail!("the second request read nothing from the cache (the first wrote {written} tokens)");
                }
                Ok(format!("first request wrote {written} tokens, second read {read}"))
            }
            .await;
            calls += 2;
            self.record("Anthropic: a second identical request reads the prompt cache", result);
        }

        if calls > 0 {
            self.events(&sc, calls).await;
        }
        if let (Some(provider), Some(model)) = (&anthropic_provider, &live_anthropic) {
            self.objects(&sc, provider, model).await;
        }
        self.intent_live(&sc, keys.typesafe.clone()).await;
        // The credentials the run stored must not come back from any listing.
        let result: anyhow::Result<String> = async {
            // This run's providers, which hold the real keys.
            let (_, providers) = self.api("GET", &format!("/providers?q={}&limit=50", self.suffix), None).await?;
            let mut seen = 0;
            for p in providers["items"].as_array().cloned().unwrap_or_default() {
                if let Some(id) = p["id"].as_str() {
                    self.api("GET", &format!("/providers/{id}/credentials"), None).await?;
                    seen += 1;
                }
            }
            Ok(format!("{seen} credential listings and every response checked"))
        }
        .await;
        self.record("no provider key in anything the gateway returned", result);
        Ok(())
    }

    /// A two-turn tool loop: the model calls the tool, the caller sends the result back with the
    /// first turn as it came. Returns a summary and the first turn's content (Anthropic shape).
    async fn tool_loop(&self, sc: &Scene, model: &str, iface: &str, stream: bool) -> anyhow::Result<(String, Value)> {
        if iface == "openai" {
            let user = json!({ "role": "user", "content": TOOL_PROMPT });
            let body =
                json!({ "model": model, "max_tokens": 2048, "tools": [weather_tool_openai()], "messages": [user] });
            let first = self.call(&sc.key, "/v1/chat/completions", &[], body).await?;
            Self::expect_ok(&first)?;
            let message = first.json()?["choices"][0]["message"].clone();
            let call_id = message["tool_calls"][0]["id"].clone();
            if call_id.is_null() {
                bail!("no tool call: {}", short(&first.text));
            }
            let signed = message["tool_calls"][0].get("extra_content").is_some();
            let body = json!({ "model": model, "max_tokens": 2048, "tools": [weather_tool_openai()], "messages": [
                user, message.clone(), { "role": "tool", "tool_call_id": call_id, "content": TOOL_RESULT } ] });
            let second = self.call(&sc.key, "/v1/chat/completions", &[], body).await?;
            Self::expect_ok(&second)?;
            let answer = second.json()?["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_owned();
            return Ok((
                format!("answered \"{}\"{}", short(&answer), if signed { ", the call signed" } else { "" }),
                message,
            ));
        }
        let headers = [("anthropic-version", "2023-06-01")];
        let user = json!({ "role": "user", "content": TOOL_PROMPT });
        let body = json!({ "model": model, "max_tokens": 2048, "stream": stream, "tools": [weather_tool_anthropic()], "messages": [user] });
        let first = self.call(&sc.key, "/v1/messages", &headers, body).await?;
        Self::expect_ok(&first)?;
        let content = if stream { assemble_anthropic(&first.text)? } else { first.json()?["content"].clone() };
        let use_id = content
            .as_array()
            .and_then(|c| c.iter().find(|b| b["type"] == "tool_use"))
            .map(|b| b["id"].clone())
            .with_context(|| format!("no tool call: {}", short(&content.to_string())))?;
        let thinking = content.as_array().map_or(0, |c| c.iter().filter(|b| b["type"] == "thinking").count());
        let body = json!({ "model": model, "max_tokens": 2048, "tools": [weather_tool_anthropic()], "messages": [
            user, { "role": "assistant", "content": content.clone() },
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": use_id, "content": TOOL_RESULT }] } ] });
        let second = self.call(&sc.key, "/v1/messages", &headers, body).await?;
        Self::expect_ok(&second)?;
        let v = second.json()?;
        let answer = v["content"]
            .as_array()
            .and_then(|c| c.iter().find(|b| b["type"] == "text"))
            .map_or("", |b| b["text"].as_str().unwrap_or(""));
        Ok((
            format!(
                "{}answered \"{}\" ({thinking} thinking blocks sent back)",
                if stream { "streamed, " } else { "" },
                short(answer)
            ),
            content,
        ))
    }

    /// Sets up what a load run needs: a key, and a priced model at the mock provider.
    /// Returns the key, the provider's name, the model's id and the provider's id for it.
    pub async fn load_scene(&mut self) -> anyhow::Result<LoadScene> {
        let sc = self.scene().await?;
        let mock = self.opt.mock_url.trim_end_matches('/').to_owned();
        let secret = format!("load-provider-secret-{}", self.suffix);
        self.secrets.push(secret.clone());
        let (provider, provider_name) = self.provider("openai", &format!("{mock}/v1"), &secret).await?;
        let model = format!("load-{}/gpt-mock", self.suffix);
        self.model(&sc, &provider, &model, "gpt-mock", "openai-chat").await?;
        Ok(LoadScene { key: sc.key, provider_name, model, upstream: "gpt-mock".into() })
    }

    /// Counts the lines of `text` that contain one of the given secrets. The secrets are not echoed.
    pub fn count_leaks(text: &str, secrets: &[String]) -> usize {
        text.lines().filter(|l| secrets.iter().any(|s| s.len() >= 8 && l.contains(s.as_str()))).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_never_part_of_a_report() {
        let s = Smoke {
            opt: Options {
                admin: String::new(),
                data: String::new(),
                email: String::new(),
                password: String::new(),
                oidc: None,
                mock_url: String::new(),
                mock_control: String::new(),
                check_events: false,
            },
            http: reqwest::Client::new(),
            token: String::new(),
            session: None,
            secrets: vec!["sk-live-0123456789".into(), String::new()],
            suffix: "x".into(),
            checks: Vec::new(),
        };
        assert_eq!(s.redact("401: bad key sk-live-0123456789 given"), "401: bad key [redacted] given");
        assert!(s.leaks("x sk-live-0123456789 y") && !s.leaks("nothing here"));
        let log = "ok line\nAuthorization: Bearer sk-live-0123456789\nanother\n";
        assert_eq!(Smoke::count_leaks(log, &s.secrets), 1);
        assert_eq!(Smoke::count_leaks(log, &["short".into()]), 0, "too short to be a key; never matched");
    }
}
