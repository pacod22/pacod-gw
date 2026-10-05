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
    /// The mock provider as the gateway reaches it, for example `http://gw-mock:9000`.
    pub mock_url: String,
    /// Wait for the calls to appear in the request log. Needs the analytics service.
    pub check_events: bool,
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

pub struct Smoke {
    opt: Options,
    http: reqwest::Client,
    token: String,
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
        let suffix = uuid::Uuid::now_v7().simple().to_string()[20..].to_owned();
        Ok(Self { opt, http, token, secrets: Vec::new(), suffix, checks: Vec::new() })
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

    async fn api(&self, method: &str, path: &str, body: Option<Value>) -> anyhow::Result<(u16, Value)> {
        let mut req =
            self.http.request(method.parse()?, format!("{}/api/v1{path}", self.opt.admin)).bearer_auth(&self.token);
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
                .create(&format!("/orgs/{org}/workspaces"), json!({ "name": "main", "tag_keys": ["purpose"] }))
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
        let name = format!("{family}-{}", self.suffix);
        let id = Self::id(
            &self.create("/providers", json!({ "name": name, "family": family, "base_url": base_url })).await?,
        )?;
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
        let r = req.body(body.to_string()).send().await?;
        let reply = Reply { status: r.status().as_u16(), headers: r.headers().clone(), text: r.text().await? };
        if self.leaks(&reply.text) || reply.headers.values().any(|v| v.to_str().is_ok_and(|v| self.leaks(v))) {
            bail!("the answer of {path} contains a provider key");
        }
        Ok(reply)
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
                    return Ok(format!(
                        "{} events, {ok} ok, {exact} with provider usage, {priced} with a cost",
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

    /// Revokes the key and checks that it stops working within two seconds.
    async fn revocation(&mut self, sc: &Scene, path: &str, headers: &[(&str, &str)], body: Value) {
        let result: anyhow::Result<String> = async {
            let (st, v) = self
                .api("POST", &format!("/orgs/{}/workspaces/{}/keys/{}/revoke", sc.org, sc.ws, sc.key_id), None)
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
                                   "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Say hello to the smoke test." }] }] });
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

        self.events(&sc, calls).await;
        self.revocation(&sc, "/v1/chat/completions", &[], json!({ "model": gpt, "messages": user })).await;
        Ok(())
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
    pub async fn run_live(
        &mut self,
        openai_key: Option<String>,
        anthropic_key: Option<String>,
        openai_model: Option<String>,
        anthropic_model: Option<String>,
    ) -> anyhow::Result<()> {
        self.secrets.extend(openai_key.iter().chain(anthropic_key.iter()).cloned());
        let sc = self.scene().await.context("setting up the organisation, workspace and key")?;
        let prompt = json!([{ "role": "user", "content": "Reply with the single word: pong" }]);
        let mut calls = 0;
        // The repository ids of the models that were set up, for the cross-interface checks.
        let (mut live_openai, mut live_anthropic): (Option<String>, Option<String>) = (None, None);

        match openai_key {
            None => self.skip("OpenAI", "OPENAI_API_KEY is not set"),
            Some(secret) => {
                let setup: anyhow::Result<(String, String, String)> = async {
                    let (provider, name) = self.provider("openai", "https://api.openai.com/v1", &secret).await?;
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
                        calls += 3;
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
                        live_anthropic = Some(model);
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
                        calls += 2;
                    }
                }
            }
        }

        // Each live model from all three interfaces (concept section 8.2).
        let anthropic_headers = [("anthropic-version", "2023-06-01")];
        for (provider, model) in [("OpenAI", &live_openai), ("Anthropic", &live_anthropic)] {
            let Some(model) = model else { continue };
            let result: anyhow::Result<String> = async {
                let a = self
                    .call(
                        &sc.key,
                        "/v1/chat/completions",
                        &[],
                        json!({ "model": model, "max_tokens": 64, "messages": prompt }),
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
                        json!({ "model": model, "max_tokens": 64, "messages": prompt }),
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
                        json!({ "model": model, "max_output_tokens": 64, "stream": true, "messages": prompt }),
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
        // The credentials the run stored must not come back from any listing.
        let result: anyhow::Result<String> = async {
            let (_, providers) = self.api("GET", "/providers?limit=200", None).await?;
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
                mock_url: String::new(),
                check_events: false,
            },
            http: reqwest::Client::new(),
            token: String::new(),
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
