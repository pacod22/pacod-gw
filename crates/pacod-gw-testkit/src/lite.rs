//! The smoke test of the minimal gateway (`gw-smoke lite`, docs/lite.md).
//!
//! The minimal gateway has no management API: nothing is set up here. The test calls what the
//! gateway's configuration offers, which `deploy/lite/docker-compose.dev.yml` fixes for the
//! mock provider, and the live script for real providers. Each call's request id is printed,
//! so that a script can find its line in the gateway's log.

use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};

use crate::smoke::{Check, Outcome, Reply};

/// What to call. Empty names skip their checks.
#[derive(Debug, Clone)]
pub struct LiteOptions {
    /// The gateway.
    pub data: String,
    /// A client key that may call everything below.
    pub key: String,
    /// A client key limited to `limited_rpm` requests a minute. Empty: not checked.
    pub limited_key: String,
    pub limited_rpm: u32,
    /// The mock provider as this tool reaches it, to script a failover. Empty: not checked.
    pub mock_control: String,
    /// A chat model called through OpenAI's API.
    pub openai_model: String,
    /// A chat model of another provider, called through both APIs.
    pub anthropic_model: String,
    /// A pool, without its `@`.
    pub pool: String,
    pub embedding_model: String,
    pub speech_model: String,
    /// A decision model, asked through `/v1/systemone`.
    pub decision_model: String,
    /// An image model, asked for one image.
    pub image_model: String,
    /// A transcription model, given a second of audio.
    pub transcription_model: String,
    /// A rerank model, given two documents.
    pub rerank_model: String,
    /// A realtime model, asked for one response in a WebSocket session.
    pub realtime_model: String,
    /// A provider for proxy mode, and the model id it knows.
    pub proxy_provider: String,
    pub proxy_model: String,
}

pub struct LiteSmoke {
    opts: LiteOptions,
    http: reqwest::Client,
    pub checks: Vec<Check>,
    /// (check, request id) of every call that should be in the call log.
    pub request_ids: Vec<(String, String)>,
}

impl LiteSmoke {
    pub fn new(opts: LiteOptions) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(120)).build()?;
        Ok(Self { opts, http, checks: Vec::new(), request_ids: Vec::new() })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.opts.data.trim_end_matches('/'))
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> anyhow::Result<Reply> {
        let r = req.send().await.context("the gateway did not answer")?;
        let status = r.status().as_u16();
        let headers = r.headers().clone();
        let text = r.text().await.unwrap_or_default();
        Ok(Reply { status, headers, text })
    }

    async fn openai(&self, key: &str, body: Value, extra: &[(&str, &str)]) -> anyhow::Result<Reply> {
        let mut req = self.http.post(self.url("/v1/chat/completions")).bearer_auth(key).json(&body);
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        self.send(req).await
    }

    async fn anthropic(&self, body: Value) -> anyhow::Result<Reply> {
        let req = self
            .http
            .post(self.url("/v1/messages"))
            .header("x-api-key", &self.opts.key)
            .header("anthropic-version", "2023-06-01")
            .json(&body);
        self.send(req).await
    }

    fn record(&mut self, name: &str, result: anyhow::Result<(String, Option<String>)>) {
        let outcome = match result {
            Ok((detail, id)) => {
                if let Some(id) = id {
                    self.request_ids.push((name.to_owned(), id));
                }
                Outcome::Passed(detail)
            }
            Err(e) => Outcome::Failed(format!("{e:#}")),
        };
        self.checks.push(Check { name: name.to_owned(), outcome });
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
            println!("{mark}  {:<40} {detail}", c.name);
        }
        for (name, id) in &self.request_ids {
            println!("request-id {id} {name}");
        }
        let passed = self.checks.iter().filter(|c| matches!(c.outcome, Outcome::Passed(_))).count();
        let skipped = self.checks.iter().filter(|c| matches!(c.outcome, Outcome::Skipped(_))).count();
        println!("{passed} passed, {} failed, {skipped} skipped", self.failed());
    }

    pub async fn run(&mut self) {
        let o = self.opts.clone();

        let r = self.ready().await;
        self.record("ready", r);
        let r = self.models().await;
        self.record("model list", r);

        if o.openai_model.is_empty() {
            self.skip("chat (OpenAI API)", "no model given");
        } else {
            let r = self.chat(&o.openai_model, false).await;
            self.record("chat (OpenAI API)", r);
            let r = self.chat(&o.openai_model, true).await;
            self.record("chat stream (OpenAI API)", r);
        }
        if o.anthropic_model.is_empty() {
            self.skip("messages (Anthropic API)", "no model given");
        } else {
            let r = self.messages(&o.anthropic_model).await;
            self.record("messages (Anthropic API)", r);
            let r = self.chat(&o.anthropic_model, false).await;
            self.record("translated (OpenAI API, other provider)", r);
            let r = self.audited(&o.anthropic_model).await;
            self.record("call with x-gw-audit", r);
        }
        if !o.openai_model.is_empty() && !o.anthropic_model.is_empty() {
            let r = self.messages(&o.openai_model).await;
            self.record("translated (Anthropic API, OpenAI model)", r);
        }
        match (o.pool.is_empty(), o.mock_control.is_empty()) {
            (true, _) => self.skip("pool failover", "no pool given"),
            (false, true) => {
                let r = self.chat(&format!("@{}", o.pool), false).await;
                self.record("pool", r);
            }
            (false, false) => {
                let r = self.failover(&o.pool).await;
                self.record("pool failover", r);
            }
        }
        if o.proxy_provider.is_empty() {
            self.skip("proxy mode", "no provider given");
        } else {
            let r = self.proxy().await;
            self.record("proxy mode", r);
        }
        if o.embedding_model.is_empty() {
            self.skip("embeddings", "no model given");
        } else {
            let r = self.embeddings(&o.embedding_model).await;
            self.record("embeddings", r);
        }
        if o.speech_model.is_empty() {
            self.skip("speech", "no model given");
        } else {
            let r = self.speech(&o.speech_model).await;
            self.record("speech", r);
        }
        let r = self.responses(&o.openai_model).await;
        self.record("Responses API", r);
        if o.image_model.is_empty() {
            self.skip("images", "no model given");
        } else {
            let r = self.image(&o.image_model).await;
            self.record("images", r);
        }
        if o.transcription_model.is_empty() {
            self.skip("transcription", "no model given");
        } else {
            let r = self.transcription(&o.transcription_model).await;
            self.record("transcription", r);
        }
        if o.rerank_model.is_empty() {
            self.skip("rerank", "no model given");
        } else {
            let r = self.rerank(&o.rerank_model).await;
            self.record("rerank", r);
        }
        if o.decision_model.is_empty() {
            self.skip("System One", "no model given");
        } else {
            let r = self.system_one(&o.decision_model).await;
            self.record("System One", r);
        }
        if o.realtime_model.is_empty() {
            self.skip("realtime", "no model given");
        } else {
            let path = format!("/v1/realtime?model={}", o.realtime_model);
            let auth = format!("Bearer {}", o.key);
            let r = crate::ws::realtime_response(&o.data, &path, &[("authorization", &auth)]).await.and_then(
                |(done, id)| {
                    let used = done["response"]["usage"]["output_tokens"].as_u64().unwrap_or(0);
                    anyhow::ensure!(done["response"]["status"] == "completed" && used > 0, "the response: {done}");
                    Ok((format!("one response, {used} tokens out"), id))
                },
            );
            self.record("realtime", r);
        }
        if o.proxy_provider.is_empty() {
            self.skip("proxy mode: a WebSocket session", "no provider given");
        } else {
            let path = format!("/proxy/{}/__ws/echo?model={}", o.proxy_provider, o.proxy_model);
            let auth = format!("Bearer {}", o.key);
            let r = crate::ws::round_trip(&o.data, &path, &[("authorization", &auth)], "hello").await.and_then(
                |(a, id)| {
                    anyhow::ensure!(a == "hello", "the echo answered {a:?}");
                    Ok(("relayed both ways".to_owned(), id))
                },
            );
            self.record("proxy mode: a WebSocket session", r);
        }
        let r = self.refusals().await;
        self.record("bad key", r);
        let r = self.files().await;
        self.record("files", r);
        if o.limited_key.is_empty() {
            self.skip("requests per minute", "no limited key given");
        } else {
            let r = self.rate_limit().await;
            self.record("requests per minute", r);
        }
    }

    async fn ready(&self) -> anyhow::Result<(String, Option<String>)> {
        let r = self.send(self.http.get(self.url("/readyz"))).await?;
        anyhow::ensure!(r.status == 200, "/readyz answered {}: {}", r.status, r.text);
        Ok(("/readyz 200".into(), None))
    }

    async fn models(&self) -> anyhow::Result<(String, Option<String>)> {
        let r = self.send(self.http.get(self.url("/v1/models")).bearer_auth(&self.opts.key)).await?;
        anyhow::ensure!(r.status == 200, "/v1/models answered {}: {}", r.status, r.text);
        let v = r.json()?;
        let ids: Vec<&str> = v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str()).collect();
        for want in [&self.opts.openai_model, &self.opts.anthropic_model].into_iter().filter(|m| !m.is_empty()) {
            anyhow::ensure!(ids.contains(&want.as_str()), "{want} is not listed: {ids:?}");
        }
        Ok((format!("{} models", ids.len()), None))
    }

    async fn chat(&self, model: &str, stream: bool) -> anyhow::Result<(String, Option<String>)> {
        let mut body = json!({ "model": model, "max_tokens": 16,
            "messages": [{ "role": "user", "content": "Say ok." }] });
        if stream {
            body["stream"] = json!(true);
            body["stream_options"] = json!({ "include_usage": true });
        }
        let r = self.openai(&self.opts.key, body, &[]).await?;
        anyhow::ensure!(r.status == 200, "{model} answered {}: {}", r.status, r.text);
        let id = r.header("x-gw-request-id").map(str::to_owned);
        let served = r.header("x-gw-model-served").unwrap_or("?").to_owned();
        if stream {
            anyhow::ensure!(r.text.contains("data: [DONE]"), "the stream did not end with [DONE]");
        } else {
            let v = r.json()?;
            anyhow::ensure!(v["choices"][0]["message"].is_object(), "no message in {}", r.text);
        }
        Ok((format!("{model}, served by {served}"), id))
    }

    async fn messages(&self, model: &str) -> anyhow::Result<(String, Option<String>)> {
        let r = self
            .anthropic(
                json!({ "model": model, "max_tokens": 16, "messages": [{ "role": "user", "content": "Say ok." }] }),
            )
            .await?;
        anyhow::ensure!(r.status == 200, "{model} answered {}: {}", r.status, r.text);
        let v = r.json()?;
        anyhow::ensure!(v["type"] == "message", "not an Anthropic message: {}", r.text);
        Ok((model.to_owned(), r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn audited(&self, model: &str) -> anyhow::Result<(String, Option<String>)> {
        let body =
            json!({ "model": model, "max_tokens": 16, "messages": [{ "role": "user", "content": "Audited call." }] });
        let r = self.openai(&self.opts.key, body, &[("x-gw-audit", "true")]).await?;
        anyhow::ensure!(r.status == 200, "{model} answered {}: {}", r.status, r.text);
        Ok(("bodies logged where PGW_LOG_BODIES allows".into(), r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn failover(&self, pool: &str) -> anyhow::Result<(String, Option<String>)> {
        let script = format!("{}/__mock/script", self.opts.mock_control.trim_end_matches('/'));
        let r = self.http.post(&script).json(&json!(["fail=503x1"])).send().await.context("the mock")?;
        anyhow::ensure!(r.status().is_success(), "the mock refused the script: {}", r.status());
        let body = json!({ "model": format!("@{pool}"), "max_tokens": 16,
            "messages": [{ "role": "user", "content": "Fail over." }] });
        let r = self.openai(&self.opts.key, body, &[]).await;
        let _ = self.http.delete(&script).send().await;
        let r = r?;
        anyhow::ensure!(r.status == 200, "@{pool} answered {}: {}", r.status, r.text);
        let served = r.header("x-gw-model-served").unwrap_or("?").to_owned();
        let failovers: u32 = r.header("x-gw-failovers").and_then(|v| v.parse().ok()).unwrap_or(0);
        anyhow::ensure!(failovers >= 1, "@{pool} was served by {served} without a failover");
        Ok((format!("a 503 moved @{pool} on to {served}"), r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn proxy(&self) -> anyhow::Result<(String, Option<String>)> {
        let o = &self.opts;
        let body = json!({ "model": o.proxy_model, "max_tokens": 16,
            "messages": [{ "role": "user", "content": "Say ok." }] });
        let url = self.url(&format!("/proxy/{}/v1/chat/completions", o.proxy_provider));
        let r = self.send(self.http.post(url).bearer_auth(&o.key).json(&body)).await?;
        anyhow::ensure!(r.status == 200, "proxy mode answered {}: {}", r.status, r.text);
        Ok((format!("/proxy/{}/v1/chat/completions", o.proxy_provider), r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn embeddings(&self, model: &str) -> anyhow::Result<(String, Option<String>)> {
        let req = self
            .http
            .post(self.url("/v1/embeddings"))
            .bearer_auth(&self.opts.key)
            .json(&json!({ "model": model, "input": "ok" }));
        let r = self.send(req).await?;
        anyhow::ensure!(r.status == 200, "{model} answered {}: {}", r.status, r.text);
        let n = r.json()?["data"][0]["embedding"].as_array().map_or(0, Vec::len);
        anyhow::ensure!(n > 0, "no vector in {}", r.text);
        Ok((format!("{n} dimensions"), r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn responses(&self, model: &str) -> anyhow::Result<(String, Option<String>)> {
        let req = self
            .http
            .post(self.url("/v1/responses"))
            .bearer_auth(&self.opts.key)
            .json(&json!({ "model": model, "input": "ok" }));
        let r = self.send(req).await?;
        anyhow::ensure!(r.status == 200, "{model} answered {}: {}", r.status, r.text);
        let v = r.json()?;
        anyhow::ensure!(v["object"] == "response" && v["store"] == false, "not a stateless response: {}", r.text);
        Ok((v["status"].as_str().unwrap_or_default().to_owned(), r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn transcription(&self, model: &str) -> anyhow::Result<(String, Option<String>)> {
        let boundary = "lite-audio";
        let mut form =
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n").into_bytes();
        form.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"ok.wav\"\r\n\r\n")
                .as_bytes(),
        );
        form.extend_from_slice(&[0u8; 16_000]);
        form.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let req = self
            .http
            .post(self.url("/v1/audio/transcriptions"))
            .bearer_auth(&self.opts.key)
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(form);
        let r = self.send(req).await?;
        anyhow::ensure!(r.status == 200, "{model} answered {}: {}", r.status, r.text);
        let text = r.json()?["text"].as_str().unwrap_or_default().to_owned();
        anyhow::ensure!(!text.is_empty(), "no transcript in {}", r.text);
        Ok((text, r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn rerank(&self, model: &str) -> anyhow::Result<(String, Option<String>)> {
        let req = self.http.post(self.url("/v1/rerank")).bearer_auth(&self.opts.key).json(&json!({
            "model": model, "query": "capital of France", "documents": ["Berlin", "Paris is the capital of France"]
        }));
        let r = self.send(req).await?;
        anyhow::ensure!(r.status == 200, "{model} answered {}: {}", r.status, r.text);
        anyhow::ensure!(r.json()?["results"][0]["index"] == 1, "not the expected order: {}", r.text);
        Ok(("Paris first".into(), r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn image(&self, model: &str) -> anyhow::Result<(String, Option<String>)> {
        let req = self
            .http
            .post(self.url("/v1/images/generations"))
            .bearer_auth(&self.opts.key)
            .json(&json!({ "model": model, "prompt": "a cat" }));
        let r = self.send(req).await?;
        anyhow::ensure!(r.status == 200, "{model} answered {}: {}", r.status, r.text);
        let n = r.json()?["data"].as_array().map_or(0, Vec::len);
        anyhow::ensure!(n == 1, "{n} images in {}", r.text);
        Ok(("one image".into(), r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn system_one(&self, model: &str) -> anyhow::Result<(String, Option<String>)> {
        let req = self.http.post(self.url("/v1/systemone")).bearer_auth(&self.opts.key).json(&json!({
            "model": model,
            "state": [{ "from": "user", "text": "a billing question" }],
            "questions": { "intent": { "type": "choice", "instructions": "What does the user want?",
                "criteria": { "billing": { "what": "Charges." }, "unknown": { "what": "Other." } } } }
        }));
        let r = self.send(req).await?;
        anyhow::ensure!(r.status == 200, "{model} answered {}: {}", r.status, r.text);
        let choice = r.json()?["answers"]["intent"]["choice"].as_str().unwrap_or_default().to_owned();
        anyhow::ensure!(choice == "billing", "{model} chose {choice:?}: {}", r.text);
        Ok((choice, r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn speech(&self, model: &str) -> anyhow::Result<(String, Option<String>)> {
        let req = self
            .http
            .post(self.url("/v1/audio/speech"))
            .bearer_auth(&self.opts.key)
            .json(&json!({ "model": model, "input": "ok", "voice": "alloy" }));
        let r = req.send().await?;
        let status = r.status().as_u16();
        let id = r.headers().get("x-gw-request-id").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let kind = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
        anyhow::ensure!(status == 200 && kind.starts_with("audio/"), "{model} answered {status} ({kind})");
        Ok((kind, id))
    }

    async fn refusals(&self) -> anyhow::Result<(String, Option<String>)> {
        let wrong = "pgw_smoke_NotAKeyOfThisGateway0123";
        let r = self.openai(wrong, json!({ "model": "x", "messages": [] }), &[]).await?;
        anyhow::ensure!(r.status == 401, "a wrong key got {}", r.status);
        Ok(("401".into(), None))
    }

    /// A file uploaded to Anthropic through the gateway, listed and deleted: its record
    /// (PGW_OBJECTS_DIR) works.
    async fn files(&self) -> anyhow::Result<(String, Option<String>)> {
        let anthropic = |req: reqwest::RequestBuilder| {
            req.header("x-api-key", &self.opts.key).header("anthropic-version", "2023-06-01")
        };
        let boundary = "lite-file";
        let form = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"smoke.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--{boundary}--\r\n"
        );
        let req = anthropic(self.http.post(self.url("/v1/files")))
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(form);
        let r = self.send(req).await?;
        anyhow::ensure!(r.status == 200, "the upload answered {}: {}", r.status, r.text);
        let id = r.json()?["id"].as_str().unwrap_or_default().to_owned();
        let listed = self.send(anthropic(self.http.get(self.url("/v1/files")))).await?;
        anyhow::ensure!(listed.text.contains(&id), "the file is not listed: {}", listed.text);
        let gone = self.send(anthropic(self.http.delete(self.url(&format!("/v1/files/{id}"))))).await?;
        anyhow::ensure!(gone.status == 200, "the delete answered {}", gone.status);
        Ok(("uploaded, listed and deleted".into(), r.header("x-gw-request-id").map(str::to_owned)))
    }

    async fn rate_limit(&self) -> anyhow::Result<(String, Option<String>)> {
        let model = &self.opts.openai_model;
        let mut statuses = Vec::new();
        for _ in 0..=self.opts.limited_rpm {
            let body = json!({ "model": model, "max_tokens": 16, "messages": [{ "role": "user", "content": "x" }] });
            statuses.push(self.openai(&self.opts.limited_key, body, &[]).await?.status);
        }
        let n = self.opts.limited_rpm as usize;
        anyhow::ensure!(
            statuses[..n].iter().all(|s| *s == 200) && statuses[n] == 429,
            "expected {n} × 200 then 429, got {statuses:?} (another run within the minute counts too)"
        );
        Ok((format!("{n} served, then 429"), None))
    }
}
