//! OpenAI's Responses API in managed mode (`POST /v1/responses`), stateless: relayed to a
//! provider that serves it, translated to Anthropic and to a chat-completions endpoint.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
}

/// A Claude model, an OpenAI model at a provider that serves the Responses API, and a model at
/// an OpenAI-compatible provider that serves chat completions only. All priced.
async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    let rates = json!({ "currency": "USD", "input": 2.0, "output": 10.0, "cache_read": 0.2 });
    gw.priced_model(
        &fx,
        "anthropic/claude",
        "claude-sonnet-5-5",
        json!({ "capability_preset": "anthropic-adaptive", "max_output_tokens": 16000 }),
        rates.clone(),
    )
    .await;
    for (name, quirks, model, upstream, preset) in [
        ("openai", json!({ "responses_api": true }), "openai/gpt", "gpt-x", "openai-reasoning"),
        ("compat", json!({}), "compat/chat", "gpt-compat", "openai-chat"),
    ] {
        let provider = gw
            .create(
                "/providers",
                json!({ "name": name, "family": "openai", "base_url": format!("{}/v1", mock.base_url()), "quirks": quirks }),
            )
            .await
            .id;
        gw.create(&format!("/providers/{provider}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
        let id = gw.draft_model(&provider, model, upstream, json!({ "capability_preset": preset })).await;
        let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(rates.clone())).await;
        assert_eq!(st, 201, "{v}");
        gw.activate(&id).await;
        gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    }
    World { gw, mock, sink, fx }
}

fn script(mock: &MockProvider, entries: &[&str]) {
    mock.set_script(Behavior::parse_all(entries.iter().copied()).expect("script"));
}

struct Reply {
    status: u16,
    text: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text))
    }

    /// The events of a server-sent stream: the name and the data.
    fn events(&self) -> Vec<(String, Value)> {
        self.text
            .split("\n\n")
            .filter(|e| !e.trim().is_empty())
            .filter_map(|e| {
                let name = e.lines().find_map(|l| l.strip_prefix("event: ")).unwrap_or_default().to_owned();
                let data = e.lines().find_map(|l| l.strip_prefix("data: "))?;
                Some((name, serde_json::from_str(data).unwrap_or(Value::String(data.to_owned()))))
            })
            .collect()
    }
}

async fn post(w: &World, body: Value) -> Reply {
    let r =
        w.gw.http.post(format!("{}/v1/responses", w.gw.data)).bearer_auth(&w.fx.key).json(&body).send().await.unwrap();
    Reply { status: r.status().as_u16(), text: r.text().await.unwrap() }
}

impl World {
    fn sent(&self, suffix: &str) -> Vec<Value> {
        self.mock.captured().into_iter().filter(|c| c.path.ends_with(suffix)).map(|c| c.json().unwrap()).collect()
    }
}

#[tokio::test]
async fn a_responses_provider_is_relayed_to_and_keeps_nothing() {
    let w = setup().await;
    script(&w.mock, &["thinking"]);
    let r = post(&w, json!({ "model": "openai/gpt", "input": "hello", "gateway": { "end_user": "u-1" } })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let v = r.json();
    assert_eq!(v["object"], "response");
    let text =
        v["output"].as_array().unwrap().iter().find(|i| i["type"] == "message").unwrap()["content"][0]["text"].clone();
    assert_eq!(text, "echo: hello");
    assert!(v["usage"]["cost"]["amount"].as_f64().is_some_and(|c| c > 0.0), "{}", v["usage"]);

    // The provider was asked at its own path, for its own model, to keep nothing, and to give
    // the reasoning back encrypted: without a stored response, that is how it crosses turns.
    let sent = w.sent("/v1/responses");
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0]["model"].as_str(), sent[0]["store"].as_bool()), (Some("gpt-x"), Some(false)));
    assert_eq!(sent[0]["include"], json!(["reasoning.encrypted_content"]));
    assert!(sent[0].get("gateway").is_none());
    let reasoning = v["output"].as_array().unwrap().iter().find(|i| i["type"] == "reasoning").unwrap().clone();
    assert!(reasoning["encrypted_content"].is_string(), "{reasoning}");

    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.interface.as_str(), e.path.as_str()), ("responses", "/v1/responses"));
    assert_eq!((e.usage_source.as_str(), e.end_user.as_str()), ("provider", "u-1"));
    assert!(e.usage.input_tokens > 0 && e.usage.reasoning_tokens > 0, "{:?}", e.usage);
    assert!(e.cost.is_some_and(|c| c > 0.0));
}

#[tokio::test]
async fn a_relayed_stream_gets_the_cost_on_its_last_event() {
    let w = setup().await;
    let r = post(&w, json!({ "model": "openai/gpt", "input": "hello", "stream": true })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let events = r.events();
    assert_eq!(events.first().map(|e| e.0.as_str()), Some("response.created"));
    let (last, data) = events.last().unwrap();
    assert_eq!(last, "response.completed");
    assert!(data["response"]["usage"]["cost"]["amount"].as_f64().is_some_and(|c| c > 0.0), "{data}");
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.usage_source.as_str(), e.stream), ("provider", true));
}

#[tokio::test]
async fn state_is_refused_and_nothing_stored_can_be_read() {
    let w = setup().await;
    for (body, word) in [
        (json!({ "model": "openai/gpt", "input": "hi", "store": true }), "store"),
        (
            json!({ "model": "anthropic/claude", "input": "hi", "previous_response_id": "resp_1" }),
            "previous_response_id",
        ),
        (json!({ "model": "openai/gpt", "input": "hi", "conversation": "conv_1" }), "conversation"),
        (json!({ "model": "openai/gpt", "input": "hi", "background": true }), "background"),
        (json!({ "model": "openai/gpt" }), "input is required"),
    ] {
        let r = post(&w, body.clone()).await;
        assert_eq!(r.status, 400, "{body}: {}", r.text);
        assert!(r.json()["error"]["message"].as_str().unwrap().starts_with(word), "{}", r.text);
    }
    for (method, path) in
        [("GET", "/v1/responses/resp_1"), ("DELETE", "/v1/responses/resp_1"), ("POST", "/v1/responses/resp_1/cancel")]
    {
        let r =
            w.gw.http
                .request(method.parse().unwrap(), format!("{}{path}", w.gw.data))
                .bearer_auth(&w.fx.key)
                .send()
                .await
                .unwrap();
        assert_eq!(r.status(), 404, "{method} {path}");
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["error"]["code"], "not_stored", "{v}");
    }
    assert!(w.mock.captured().is_empty(), "no provider was asked");
}

#[tokio::test]
async fn claude_answers_a_responses_caller_whole() {
    let w = setup().await;
    let r = post(
        &w,
        json!({ "model": "anthropic/claude", "instructions": "Be brief.", "input": "hello", "max_output_tokens": 500, "metadata": { "k": "v" } }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let v = r.json();
    assert!(v["id"].as_str().unwrap().starts_with("resp_"), "{}", v["id"]);
    assert_eq!(
        (v["object"].as_str(), v["status"].as_str(), v["store"].as_bool()),
        (Some("response"), Some("completed"), Some(false))
    );
    assert_eq!((v["instructions"].as_str(), v["max_output_tokens"].as_u64()), (Some("Be brief."), Some(500)));
    assert_eq!(v["output"][0]["content"][0]["text"], "echo: hello");
    assert!(v["usage"]["input_tokens"].as_u64().is_some_and(|n| n > 0));
    assert!(v["usage"]["cost"]["amount"].is_number());
    let sent = w.sent("/v1/messages");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["system"][0]["text"], "Be brief.");
    assert_eq!(sent[0]["max_tokens"], 500);
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.interface.as_str(), e.usage_source.as_str()), ("responses", "provider"));
}

#[tokio::test]
async fn claude_streams_the_events_the_sdks_read() {
    let w = setup().await;
    let r = post(&w, json!({ "model": "anthropic/claude", "input": "hello", "stream": true })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let events = r.events();
    let names: Vec<&str> = events.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names[..2], ["response.created", "response.in_progress"]);
    assert!(names.contains(&"response.output_text.delta"), "{names:?}");
    assert_eq!(names.last(), Some(&"response.completed"));
    let deltas: String = events
        .iter()
        .filter(|e| e.0 == "response.output_text.delta")
        .map(|e| e.1["delta"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(deltas, "echo: hello");
    let numbers: Vec<u64> = events.iter().map(|e| e.1["sequence_number"].as_u64().unwrap()).collect();
    assert_eq!(numbers, (0..events.len() as u64).collect::<Vec<_>>());
    let last = &events.last().unwrap().1["response"];
    assert!(last["usage"]["cost"]["amount"].is_number(), "{last}");
}

#[tokio::test]
async fn a_chat_completions_provider_is_reached_through_translation() {
    let w = setup().await;
    let r = post(&w, json!({ "model": "compat/chat", "input": [{ "role": "user", "content": "hello" }] })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.json()["output"][0]["content"][0]["text"], "echo: hello");
    assert!(w.sent("/v1/responses").is_empty());
    let sent = w.sent("/v1/chat/completions");
    assert_eq!(sent[0]["model"], "gpt-compat");
    assert_eq!(sent[0]["messages"][0]["content"], "hello");
}

#[tokio::test]
async fn built_in_tools_need_a_responses_provider() {
    let w = setup().await;
    let body = |model: &str| json!({ "model": model, "input": "search for it", "tools": [{ "type": "web_search" }] });
    let r = post(&w, body("anthropic/claude")).await;
    assert_eq!(r.status, 400, "{}", r.text);
    assert_eq!(r.json()["error"]["code"], "unsupported_feature", "{}", r.text);
    let r = post(&w, body("openai/gpt")).await;
    assert_eq!(r.status, 200, "{}", r.text);
    // The refused call has its event too.
    let e = w.sink.wait_events(2).await.into_iter().find(|e| e.status == "ok").unwrap();
    assert_eq!(e.usage.server_tool_calls, 1, "the search is counted");
}

#[tokio::test]
async fn a_tool_call_and_its_output_cross_the_translation() {
    let w = setup().await;
    script(&w.mock, &["tool=weather"]);
    let tools =
        json!([{ "type": "function", "name": "weather", "parameters": { "type": "object", "properties": {} } }]);
    let r = post(&w, json!({ "model": "anthropic/claude", "input": "Bern?", "tools": tools })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let call = r.json()["output"].as_array().unwrap().iter().find(|i| i["type"] == "function_call").unwrap().clone();
    assert_eq!(call["name"], "weather");
    let call_id = call["call_id"].as_str().unwrap().to_owned();

    let r = post(
        &w,
        json!({ "model": "anthropic/claude", "tools": tools, "input": [
            { "role": "user", "content": "Bern?" },
            call,
            { "type": "function_call_output", "call_id": call_id, "output": "sunny" }
        ] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.sent("/v1/messages");
    let messages = sent[1]["messages"].as_array().unwrap();
    assert_eq!(messages[1]["content"][0]["type"], "tool_use");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], call_id);
}

#[tokio::test]
async fn reasoning_another_provider_wrote_is_not_sent_to_openai() {
    let w = setup().await;
    let r = post(
        &w,
        json!({ "model": "openai/gpt", "input": [
            { "role": "user", "content": "one" },
            { "type": "reasoning", "summary": [], "encrypted_content": "gw1.anthropic.SIG" },
            { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "two" }] },
            { "role": "user", "content": "three" }
        ] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let input = w.sent("/v1/responses")[0]["input"].clone();
    assert_eq!(input.as_array().unwrap().len(), 3, "{input}");
    assert!(!input.to_string().contains("gw1."));
}

#[tokio::test]
async fn proxy_mode_on_the_shared_credential_does_not_reuse_stored_state() {
    let w = setup().await;
    let url = format!("{}/proxy/openai/v1/responses", w.gw.data);
    let body = json!({ "model": "gpt-x", "input": "hi", "previous_response_id": "resp_other" });
    let r = w.gw.http.post(&url).bearer_auth(&w.fx.key).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 403);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "own_credential_required", "{v}");
    let r =
        w.gw.http
            .post(&url)
            .bearer_auth(&w.fx.key)
            .json(&json!({ "model": "gpt-x", "input": "hi" }))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 200);
}
