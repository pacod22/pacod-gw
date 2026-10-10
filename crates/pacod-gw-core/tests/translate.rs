//! Every interface reaches every provider (concept sections 8.2 to 8.5), end to end: the
//! OpenAI-compatible, the Anthropic-compatible and the native interface against an Anthropic
//! model and an OpenAI-compatible one.

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

/// One gateway with a Claude-like and a GPT-like model, both priced.
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
    let openai = gw
        .create(
            "/providers",
            json!({ "name": "openai", "family": "openai", "base_url": format!("{}/v1", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{openai}/credentials"), json!({ "secret": "sk-openai-side" })).await;
    let gpt = gw.draft_model(&openai, "openai/gpt", "gpt-x", json!({ "capability_preset": "openai-reasoning" })).await;
    let (st, v) = gw.api("PUT", &format!("/models/{gpt}/price"), Some(rates)).await;
    assert_eq!(st, 201, "{v}");
    gw.activate(&gpt).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": gpt })).await;
    World { gw, mock, sink, fx }
}

fn script(mock: &MockProvider, entries: &[&str]) {
    mock.set_script(Behavior::parse_all(entries.iter().copied()).expect("script"));
}

struct Reply {
    status: u16,
    headers: reqwest::header::HeaderMap,
    text: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text))
    }

    /// The `data:` payloads of a server-sent event stream, with their event names.
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

async fn post(w: &World, path: &str, headers: &[(&str, &str)], body: Value) -> Reply {
    let mut req =
        w.gw.http
            .post(format!("{}{path}", w.gw.data))
            .bearer_auth(&w.fx.key)
            .header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let r = req.body(body.to_string()).send().await.expect("request");
    Reply { status: r.status().as_u16(), headers: r.headers().clone(), text: r.text().await.expect("body") }
}

#[tokio::test]
async fn an_openai_dialect_caller_reaches_an_anthropic_model() {
    let w = setup().await;
    script(&w.mock, &["thinking,cache_read=50,request_id=req_t1"]);
    let r = post(
        &w,
        "/v1/chat/completions",
        &[],
        json!({
            "model": "anthropic/claude",
            "messages": [
                { "role": "system", "content": [{ "type": "text", "text": "You are terse.", "cache_control": { "type": "ephemeral" } }] },
                { "role": "user", "content": "What is the capital of France?" }
            ],
            "reasoning_effort": "high", "temperature": 0.4, "seed": 7,
            "user": "u_7", "gateway": { "tags": { "purpose": "qa" } }
        }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);

    // What Anthropic received: its own shape, with the limit it requires and the caller's marker.
    let c = &w.mock.captured()[0];
    assert_eq!(c.path, "/v1/messages");
    assert_eq!(c.header("x-api-key"), Some(PROVIDER_SECRET));
    assert_eq!(c.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(
        c.json().unwrap(),
        json!({
            "model": "claude-sonnet-5-5",
            "max_tokens": 16000,
            "system": [{ "type": "text", "text": "You are terse.", "cache_control": { "type": "ephemeral" } }],
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "What is the capital of France?" }] }],
            "thinking": { "type": "adaptive", "display": "summarized" },
            "output_config": { "effort": "high" }
        })
    );
    // The model does not take a temperature; `seed` means nothing to Anthropic.
    assert_eq!(r.header("x-gw-dropped-params"), Some("temperature,seed"));

    // What the caller received: OpenAI's shape, the gateway's model id, usage in OpenAI's
    // convention and the cost.
    let v = r.json();
    assert_eq!((v["object"].as_str(), v["model"].as_str()), (Some("chat.completion"), Some("anthropic/claude")));
    let message = &v["choices"][0]["message"];
    assert!(message["content"].as_str().unwrap().starts_with("echo: "));
    assert!(message["reasoning_content"].as_str().unwrap().starts_with("thinking about"));
    let block = &message["provider_specific_fields"]["thinking_blocks"][0];
    assert_eq!(block["type"], "thinking");
    assert!(!block["signature"].as_str().unwrap().is_empty(), "the signature travels with the block");
    assert_eq!(v["choices"][0]["finish_reason"], "stop");
    let u = &v["usage"];
    assert_eq!(u["prompt_tokens_details"]["cached_tokens"], 50);
    let fresh = u["prompt_tokens"].as_u64().unwrap() - 50;
    let expected = (fresh as f64 * 2.0 + 50.0 * 0.2 + u["completion_tokens"].as_f64().unwrap() * 10.0) / 1e6;
    assert!((u["cost"]["amount"].as_f64().unwrap() - expected).abs() < 1e-9, "{u}");
    assert_eq!(r.header("x-gw-upstream-request-id"), Some("req_t1"));
    assert!(r.header("x-gw-cost").is_some());
    assert_eq!(r.header("content-type"), Some("application/json"));

    // The next turn sends the assistant message back as it came: Anthropic gets its block.
    w.mock.clear();
    let r2 = post(
        &w,
        "/v1/chat/completions",
        &[],
        json!({ "model": "anthropic/claude", "max_tokens": 64, "messages": [
            { "role": "user", "content": "What is the capital of France?" },
            message.clone(),
            { "role": "user", "content": "And of Austria?" }
        ] }),
    )
    .await;
    assert_eq!(r2.status, 200, "{}", r2.text);
    let replayed = &w.mock.captured()[0].json().unwrap()["messages"][1]["content"];
    assert_eq!(
        replayed[0],
        json!({ "type": "thinking", "thinking": block["thinking"], "signature": block["signature"] })
    );
    assert_eq!(replayed[1]["type"], "text");

    let ev = w.sink.wait_events(2).await;
    let e = &ev[0];
    assert_eq!(
        (e.interface.as_str(), e.provider.as_str(), e.model_served.as_str()),
        ("openai", "anthropic", "anthropic/claude")
    );
    assert_eq!(
        (e.end_user.as_str(), e.finish_reason.as_str()),
        ("u_7", "end_turn"),
        "the provider's own word is recorded"
    );
    assert_eq!(e.usage.cache_read_tokens, 50);
    assert!((e.cost.unwrap() - expected).abs() < 1e-9);
    assert_eq!(e.dropped_params, ["temperature", "seed"]);
}

#[tokio::test]
async fn tools_cross_between_the_dialects() {
    let w = setup().await;
    // An OpenAI-dialect conversation with a tool round trip, served by Anthropic.
    script(&w.mock, &["tool=weather"]);
    let tools = json!([{ "type": "function", "function": { "name": "weather", "description": "w", "parameters": { "type": "object", "properties": { "city": { "type": "string" } } } } }]);
    let r = post(&w, "/v1/chat/completions", &[], json!({ "model": "anthropic/claude", "max_tokens": 100, "tools": tools, "messages": [{ "role": "user", "content": "Sofia" }] })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let v = r.json();
    let call = &v["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(
        (v["choices"][0]["finish_reason"].as_str(), call["function"]["name"].as_str()),
        (Some("tool_calls"), Some("weather"))
    );
    assert_eq!(
        serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap()).unwrap(),
        json!({ "input": "Sofia" })
    );
    assert_eq!(
        w.mock.captured()[0].json().unwrap()["tools"],
        json!([{ "name": "weather", "description": "w", "input_schema": { "type": "object", "properties": { "city": { "type": "string" } } } }])
    );

    // The results of two calls arrive as two tool messages and reach Anthropic as one turn.
    w.mock.clear();
    let r = post(
        &w,
        "/v1/chat/completions",
        &[],
        json!({ "model": "anthropic/claude", "max_tokens": 100, "tools": tools, "messages": [
            { "role": "user", "content": "Sofia and Wien" },
            { "role": "assistant", "content": null, "tool_calls": [
                { "id": "c1", "type": "function", "function": { "name": "weather", "arguments": "{\"city\":\"Sofia\"}" } },
                { "id": "c2", "type": "function", "function": { "name": "weather", "arguments": "{\"city\":\"Wien\"}" } }
            ] },
            { "role": "tool", "tool_call_id": "c1", "content": "sunny" },
            { "role": "tool", "tool_call_id": "c2", "content": "rain" }
        ] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.mock.captured()[0].json().unwrap();
    assert_eq!(sent["messages"].as_array().unwrap().len(), 3);
    assert_eq!(
        sent["messages"][2],
        json!({ "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "c1", "content": [{ "type": "text", "text": "sunny" }] },
            { "type": "tool_result", "tool_use_id": "c2", "content": [{ "type": "text", "text": "rain" }] }
        ] })
    );

    // The other way: an Anthropic-dialect conversation with a tool, served by an OpenAI-compatible model.
    w.mock.clear();
    script(&w.mock, &["tool=lookup"]);
    let r = post(
        &w,
        "/v1/messages",
        &[("anthropic-version", "2023-06-01")],
        json!({ "model": "openai/gpt", "max_tokens": 200,
                "tools": [{ "name": "lookup", "description": "d", "input_schema": { "type": "object" } }],
                "tool_choice": { "type": "any" },
                "messages": [
                    { "role": "user", "content": "find it" },
                    { "role": "assistant", "content": [{ "type": "tool_use", "id": "toolu_1", "name": "lookup", "input": { "q": "it" } }] },
                    { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "toolu_1", "content": "nothing yet" }, { "type": "text", "text": "try again" }] }
                ] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.mock.captured()[0].json().unwrap();
    assert_eq!(w.mock.captured()[0].header("authorization"), Some("Bearer sk-openai-side"));
    assert_eq!(
        sent["messages"],
        json!([
            { "role": "user", "content": "find it" },
            { "role": "assistant", "content": null, "tool_calls": [{ "id": "toolu_1", "type": "function", "function": { "name": "lookup", "arguments": "{\"q\":\"it\"}" } }] },
            { "role": "tool", "tool_call_id": "toolu_1", "content": "nothing yet" },
            { "role": "user", "content": "try again" }
        ])
    );
    assert_eq!((sent["tool_choice"].as_str(), sent["max_completion_tokens"].as_u64()), (Some("required"), Some(200)));
    let v = r.json();
    assert_eq!(
        (v["type"].as_str(), v["stop_reason"].as_str(), v["model"].as_str()),
        (Some("message"), Some("tool_use"), Some("openai/gpt"))
    );
    assert_eq!(v["content"][0]["type"], "tool_use");
    assert_eq!(v["content"][0]["input"], json!({ "input": "try again" }));
    assert!(v["usage"]["input_tokens"].as_u64().unwrap() > 0);
    assert!(r.header("x-gw-cost").is_some(), "Anthropic's shape has no place for the cost, so it is in the header");
}

#[tokio::test]
async fn translated_streams_are_stable_and_complete() {
    let w = setup().await;
    let ask = || async {
        script(&w.mock, &["thinking,split=9,request_id=req_fixed"]);
        post(
            &w,
            "/v1/chat/completions",
            &[],
            json!({ "model": "anthropic/claude", "max_tokens": 200, "stream": true, "stream_options": { "include_usage": true },
                    "reasoning_effort": "low", "messages": [{ "role": "user", "content": "tell me something at length" }] }),
        )
        .await
    };
    let first = ask().await;
    assert_eq!(first.status, 200, "{}", first.text);
    assert!(first.header("content-type").unwrap().starts_with("text/event-stream"));
    assert!(first.header("x-gw-cost").is_none());
    assert!(first.text.ends_with("data: [DONE]\n\n"));

    let events = first.events();
    let chunks: Vec<&Value> = events.iter().map(|(_, v)| v).filter(|v| v.is_object()).collect();
    assert_eq!(chunks[0]["choices"][0]["delta"], json!({ "role": "assistant", "content": "" }));
    assert_eq!(chunks[0]["model"], "anthropic/claude");
    assert!(chunks[0]["id"].as_str().unwrap().starts_with("msg_"), "the provider's id");
    let reasoning: String =
        chunks.iter().filter_map(|c| c["choices"][0]["delta"]["reasoning_content"].as_str()).collect();
    let content: String = chunks.iter().filter_map(|c| c["choices"][0]["delta"]["content"].as_str()).collect();
    assert!(reasoning.starts_with("thinking about") && content.starts_with("echo: tell me"));
    assert!(
        chunks.iter().any(
            |c| c["choices"][0]["delta"]["provider_specific_fields"]["thinking_blocks"][0]["signature"].is_string()
        )
    );
    let last = chunks.last().unwrap();
    assert_eq!(last["choices"], json!([]));
    assert!(last["usage"]["cost"]["amount"].as_f64().unwrap() > 0.0);
    assert_eq!(chunks[chunks.len() - 2]["choices"][0]["finish_reason"], "stop");

    // The same provider stream gives the same bytes; only the creation time is the gateway's.
    let second = ask().await;
    let stable = |text: &str| {
        let created = text.find("\"created\":").map(|i| &text[i..i + 21]).unwrap_or_default().to_owned();
        text.replace(&created, "\"created\":0")
    };
    assert_eq!(stable(&first.text), stable(&second.text));

    // A caller who did not ask for usage gets none, and is accounted all the same.
    script(&w.mock, &["request_id=req_quiet"]);
    let quiet = post(&w, "/v1/chat/completions", &[], json!({ "model": "anthropic/claude", "max_tokens": 50, "stream": true, "messages": [{ "role": "user", "content": "hi" }] })).await;
    assert!(!quiet.text.contains("\"usage\"") && quiet.text.ends_with("data: [DONE]\n\n"));

    // Anthropic's event sequence for an Anthropic-dialect caller of an OpenAI-compatible model.
    w.mock.clear();
    let r = post(
        &w,
        "/v1/messages",
        &[("anthropic-version", "2023-06-01")],
        json!({ "model": "openai/gpt", "max_tokens": 100, "stream": true, "messages": [{ "role": "user", "content": "one two three" }] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.mock.captured()[0].json().unwrap();
    assert_eq!(
        sent["stream_options"],
        json!({ "include_usage": true }),
        "the gateway always asks the provider for usage"
    );
    let events = r.events();
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names.first(), Some(&"message_start"));
    assert_eq!(&names[names.len() - 2..], ["message_delta", "message_stop"]);
    assert!(names.contains(&"content_block_start") && names.contains(&"content_block_stop"));
    let text: String = events.iter().filter_map(|(_, v)| v["delta"]["text"].as_str()).collect();
    assert_eq!(text, "echo: one two three");
    let delta = &events[events.len() - 2].1;
    assert_eq!(delta["delta"]["stop_reason"], "end_turn");
    assert!(
        delta["usage"]["input_tokens"].as_u64().unwrap() > 0 && delta["usage"]["output_tokens"].as_u64().unwrap() > 0
    );

    let ev = w.sink.wait_events(4).await;
    assert!(ev.iter().all(|e| e.status == "ok" && e.stream && e.usage_source == "provider" && e.cost.unwrap() > 0.0));
}

#[tokio::test]
async fn the_native_interface_serves_both_families() {
    let w = setup().await;
    w.gw.template(
        &w.fx,
        "synthesizer",
        json!({ "model": "anthropic/claude", "max_output_tokens": 4000, "temperature": { "value": 0.3 } }),
    )
    .await;
    // The request of concept section 9.2.
    let request = json!({
        "model": "@synthesizer",
        "system": [{ "type": "text", "text": "You are a careful analyst.", "cache": { "ttl": "5m" } }],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "Compare the two offers." }] }],
        "max_output_tokens": 9000,
        "reasoning": { "effort": "high", "summary": true },
        "gateway": { "end_user": "u_18342", "trace_id": "turn_9f3", "tags": { "purpose": "synthesizer", "nope": "x" } }
    });
    script(&w.mock, &["thinking"]);
    let r = post(&w, "/gw/v1/chat", &[], request.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.mock.captured()[0].json().unwrap();
    assert_eq!(sent["max_tokens"], 4000, "the template's ceiling");
    assert_eq!(sent["system"][0]["cache_control"], json!({ "type": "ephemeral", "ttl": "5m" }));
    assert_eq!(sent["output_config"]["effort"], "high");
    assert!(sent.get("temperature").is_none(), "the template's default is not accepted by this model");

    let v = r.json();
    let id = v["id"].as_str().unwrap();
    assert!(id.starts_with("gw_") && r.header("x-gw-request-id") == Some(id));
    assert_eq!(v["model"], "anthropic/claude");
    assert_eq!((v["content"][0]["type"].as_str(), v["content"][1]["type"].as_str()), (Some("reasoning"), Some("text")));
    assert!(v["content"][0]["signature"].is_string());
    assert_eq!((v["finish_reason"].as_str(), v["native_finish_reason"].as_str()), (Some("stop"), Some("end_turn")));
    assert_eq!(v["usage"]["source"], "provider");
    assert!(v["usage"]["cost"]["amount"].as_f64().unwrap() > 0.0);
    let g = &v["gateway"];
    assert_eq!(
        (g["request_id"].as_str(), g["workspace"].as_str(), g["provider"].as_str()),
        (Some(id), Some("prod"), Some("anthropic"))
    );
    assert_eq!((g["template"].as_str(), g["template_version"].as_u64()), (Some("synthesizer"), Some(1)));
    assert_eq!(
        (g["model_requested"].as_str(), g["model_served"].as_str(), g["attempts"].as_u64()),
        (Some("@synthesizer"), Some("anthropic/claude"), Some(1))
    );
    assert_eq!(g["dropped_params"], json!(["temperature"]));
    assert!(g["warnings"][0].as_str().unwrap().contains("\"nope\""), "{g}");
    assert!(g["warnings"][1].as_str().unwrap().contains("lowered from 9000 to 4000"), "{g}");
    assert!(
        g["latency_ms"]["gateway_delay"].as_f64().unwrap() >= 0.0 && g["latency_ms"]["total"].as_f64().unwrap() > 0.0
    );

    // The same interface in front of an OpenAI-compatible model, as a stream.
    w.mock.clear();
    let mut streamed = request;
    streamed["model"] = json!("openai/gpt");
    streamed["stream"] = json!(true);
    let r = post(&w, "/gw/v1/chat", &[], streamed).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.mock.captured()[0].json().unwrap();
    assert_eq!(sent["messages"][0], json!({ "role": "system", "content": "You are a careful analyst." }));
    assert_eq!((sent["max_completion_tokens"].as_u64(), sent["reasoning_effort"].as_str()), (Some(9000), Some("high")));
    assert_eq!(r.header("x-gw-dropped-params"), Some("cache_control"));
    let events = r.events();
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!((names.first(), names.last()), (Some(&"message.start"), Some(&"message.stop")));
    assert_eq!(names.iter().filter(|n| **n == "message.stop" || **n == "error").count(), 1, "exactly one end");
    let stop = &events.last().unwrap().1;
    assert!(stop["usage"]["cost"]["amount"].as_f64().unwrap() > 0.0, "usage and cost are always on the last event");
    assert_eq!(
        (stop["gateway"]["model_served"].as_str(), stop["finish_reason"].as_str()),
        (Some("openai/gpt"), Some("stop"))
    );
    assert!(events[0].1["id"].as_str().unwrap().starts_with("gw_"));

    // This interface is the gateway's own: a field it does not know is a mistake.
    let r = post(&w, "/gw/v1/chat", &[], json!({ "model": "openai/gpt", "messages": [], "max_tokens": 5 })).await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("invalid_request")));
    assert_eq!(r.json()["error"]["source"], "gateway");

    let ev = w.sink.wait_events(3).await;
    assert_eq!(ev.iter().filter(|e| e.interface == "native").count(), 3);
    assert_eq!(ev[0].tags.get("purpose").map(String::as_str), Some("synthesizer"));
}

#[tokio::test]
async fn agent_user_task_and_complexity_attribute_a_call_on_every_interface() {
    let w = setup().await;
    let messages = json!([{ "role": "user", "content": "hi" }]);
    let attribution = json!({ "agent_id": "planner", "user_id": "staff-1", "task": "plan", "complexity": "high" });

    // The native interface takes them in its `gateway` object.
    let r = post(
        &w,
        "/gw/v1/chat",
        &[],
        json!({ "model": "anthropic/claude", "messages": messages, "gateway": attribution }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);

    // The OpenAI-compatible interface takes them as headers; the body wins where both name one.
    let headers =
        [("x-gw-agent-id", "writer"), ("x-gw-user-id", "staff-2"), ("x-gw-task", "write"), ("x-gw-complexity", "low")];
    let body = json!({ "model": "openai/gpt", "messages": messages, "gateway": { "complexity": "medium" } });
    let r = post(&w, "/v1/chat/completions", &headers, body).await;
    assert_eq!(r.status, 200, "{}", r.text);
    for sent in w.mock.captured() {
        let sent = sent.json().unwrap();
        assert!(sent.get("gateway").is_none(), "the attribution is the gateway's, not the provider's");
    }

    // Complexity is one of three values, in the body as in a header.
    for (headers, gateway) in
        [(vec![], json!({ "complexity": "extreme" })), (vec![("x-gw-complexity", "HIGH")], json!({}))]
    {
        let body = json!({ "model": "anthropic/claude", "messages": messages, "gateway": gateway });
        let r = post(&w, "/gw/v1/chat", &headers, body).await;
        assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("invalid_request")), "{}", r.text);
    }

    let ev = w.sink.wait_events(2).await;
    let got: Vec<(&str, &str, &str, &str, &str)> = ev
        .iter()
        .map(|e| {
            (e.interface.as_str(), e.agent_id.as_str(), e.user_id.as_str(), e.task.as_str(), e.complexity.as_str())
        })
        .collect();
    assert!(got.contains(&("native", "planner", "staff-1", "plan", "high")), "{got:?}");
    assert!(got.contains(&("openai", "writer", "staff-2", "write", "medium")), "{got:?}");
    assert!(ev.iter().all(|e| e.end_user.is_empty()), "user_id is not the end user");

    // Of the four, only complexity is a metric label: its values are a closed set.
    let metrics_url = format!("http://{}/metrics", w.gw.app.metrics_addr);
    let metrics = w.gw.http.get(&metrics_url).send().await.unwrap().text().await.unwrap();
    let requests: Vec<&str> = metrics.lines().filter(|l| l.starts_with("gw_requests_total{")).collect();
    assert!(requests.iter().any(|l| l.contains("complexity=\"high\"")), "{requests:?}");
    assert!(requests.iter().all(|l| !l.contains("planner") && !l.contains("staff-1")), "{requests:?}");
}

#[tokio::test]
async fn what_cannot_be_translated_is_said_in_the_callers_dialect() {
    let w = setup().await;
    let user = json!([{ "role": "user", "content": "hi" }]);

    // More than one answer, or JSON mode without a schema, cannot be had from Anthropic.
    for extra in
        [json!({ "n": 2 }), json!({ "response_format": { "type": "json_object" } }), json!({ "logprobs": true })]
    {
        let mut body = json!({ "model": "anthropic/claude", "messages": user });
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let r = post(&w, "/v1/chat/completions", &[], body).await;
        assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("unsupported_feature")), "{extra}");
        assert_eq!(r.json()["error"]["code"], "unsupported_feature");
    }
    // A schema is translated.
    let r = post(
        &w,
        "/v1/chat/completions",
        &[],
        json!({ "model": "anthropic/claude", "max_tokens": 50, "messages": user,
                "response_format": { "type": "json_schema", "json_schema": { "name": "out", "schema": { "type": "object" } } } }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(
        w.mock.captured().last().unwrap().json().unwrap()["output_config"]["format"],
        json!({ "type": "json_schema", "schema": { "type": "object" } })
    );

    // A caller can ask to be told about anything that was left out.
    let body = json!({ "model": "anthropic/claude", "max_tokens": 50, "messages": user, "seed": 7 });
    let r = post(&w, "/v1/chat/completions", &[("x-gw-unsupported-params", "reject")], body.clone()).await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("unsupported_feature")));
    assert!(r.json()["error"]["message"].as_str().unwrap().contains("seed"));
    let r = post(&w, "/v1/chat/completions", &[], body).await;
    assert_eq!((r.status, r.header("x-gw-dropped-params")), (200, Some("seed")));

    // Anthropic's own tools exist only at Anthropic.
    let r = post(
        &w,
        "/v1/messages",
        &[("anthropic-version", "2023-06-01")],
        json!({ "model": "openai/gpt", "max_tokens": 50, "messages": user, "tools": [{ "type": "web_search_20260209", "name": "web_search" }] }),
    )
    .await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("unsupported_feature")));
    assert_eq!(
        (r.json()["type"].as_str(), r.json()["error"]["type"].as_str()),
        (Some("error"), Some("invalid_request_error"))
    );

    // What the provider refuses is reported with its status, in the caller's envelope.
    script(&w.mock, &["status=400"]);
    let r = post(
        &w,
        "/v1/chat/completions",
        &[],
        json!({ "model": "anthropic/claude", "max_tokens": 50, "messages": user }),
    )
    .await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("upstream_error")));
    let v = r.json();
    assert_eq!(v["error"]["type"], "invalid_request_error");
    assert!(v["error"]["message"].as_str().unwrap().contains("mock: scripted error 400"), "{v}");
    // A stream that breaks ends with an error event the caller's SDK understands.
    script(&w.mock, &["chunks=6,reset_after=2"]);
    let r = post(
        &w,
        "/v1/chat/completions",
        &[],
        json!({ "model": "anthropic/claude", "max_tokens": 50, "stream": true, "messages": [{ "role": "user", "content": "a b c d e f g h" }] }),
    )
    .await;
    assert_eq!(r.status, 200);
    let last = r.events().pop().unwrap().1;
    assert_eq!(last["error"]["code"], "upstream_error", "{}", r.text);
    assert!(!r.text.contains("[DONE]"));
}

#[tokio::test]
async fn token_counting_speech_and_model_details() {
    let w = setup().await;
    w.gw.template(&w.fx, "planner", json!({ "model": "anthropic/claude", "max_output_tokens": 400 })).await;

    // Token counting: resolved like a message, forwarded to Anthropic, free.
    let r = post(
        &w,
        "/v1/messages/count_tokens",
        &[("anthropic-version", "2023-06-01")],
        json!({ "model": "@planner", "gateway": { "end_user": "u_1" }, "messages": [{ "role": "user", "content": "How many tokens is this?" }] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.json()["input_tokens"].as_u64().unwrap() > 0);
    let c = &w.mock.captured()[0];
    assert_eq!(c.path, "/v1/messages/count_tokens");
    assert_eq!(
        c.json().unwrap(),
        json!({ "model": "claude-sonnet-5-5", "messages": [{ "role": "user", "content": "How many tokens is this?" }] })
    );
    let r = post(
        &w,
        "/v1/messages/count_tokens",
        &[("anthropic-version", "2023-06-01")],
        json!({ "model": "openai/gpt", "messages": [] }),
    )
    .await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("unsupported_feature")));

    // Speech: priced by the characters of the text.
    let (_, providers) = w.gw.api("GET", "/providers", None).await;
    let openai = providers["items"].as_array().unwrap().iter().find(|p| p["name"] == "openai").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tts =
        w.gw.draft_model(&openai, "openai/tts", "tts-1", json!({ "kind": "speech", "capability_preset": "speech" }))
            .await;
    let (st, v) =
        w.gw.api("PUT", &format!("/models/{tts}/price"), Some(json!({ "currency": "USD", "per_character": 0.000015 })))
            .await;
    assert_eq!(st, 201, "{v}");
    w.gw.activate(&tts).await;
    w.gw.create(&format!("/orgs/{}/grants", w.fx.org), json!({ "model_id": tts })).await;

    w.mock.clear();
    let text = "Grüße aus Sofia!";
    let r = post(
        &w,
        "/v1/audio/speech",
        &[("x-gw-end-user", "u_2")],
        json!({ "model": "openai/tts", "input": text, "voice": "alloy" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.header("content-type").unwrap().starts_with("audio/"));
    assert!(!r.text.is_empty());
    assert_eq!(w.mock.captured()[0].json().unwrap(), json!({ "model": "tts-1", "input": text, "voice": "alloy" }));

    w.mock.clear();
    let r = post(
        &w,
        "/gw/v1/speech",
        &[],
        json!({ "model": "openai/tts", "input": text, "voice": "alloy", "format": "wav" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.header("content-type"), Some("audio/wav"));
    assert_eq!(
        w.mock.captured()[0].json().unwrap(),
        json!({ "model": "tts-1", "input": text, "voice": "alloy", "response_format": "wav" })
    );
    // A chat model does not speak, and a text must be given.
    let r = post(&w, "/v1/audio/speech", &[], json!({ "model": "openai/gpt", "input": "x", "voice": "alloy" })).await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("invalid_request")));
    let r = post(&w, "/gw/v1/speech", &[], json!({ "model": "openai/tts", "voice": "alloy" })).await;
    assert_eq!(
        (r.status, r.json()["error"]["type"].as_str().map(str::to_owned)),
        (400, Some("invalid_request".to_owned()))
    );

    let ev = w.sink.wait_events(6).await;
    let counted = ev.iter().find(|e| e.path == "/v1/messages/count_tokens" && e.status == "ok").unwrap();
    assert_eq!((counted.cost, counted.usage_source.as_str(), counted.end_user.as_str()), (None, "missing", "u_1"));
    assert_eq!((counted.template.as_str(), counted.model_served.as_str()), ("planner", "anthropic/claude"));
    let spoken: Vec<_> = ev.iter().filter(|e| e.path.ends_with("speech") && e.status == "ok").collect();
    assert_eq!(spoken.len(), 2);
    for e in &spoken {
        assert_eq!(e.usage.characters, 16, "characters, not bytes");
        assert!((e.cost.unwrap() - 16.0 * 0.000015).abs() < 1e-12, "{:?}", e.cost);
        assert_eq!(e.usage.input_tokens + e.usage.output_tokens, 0);
    }
    assert_eq!(spoken[0].end_user, "u_2");

    // One model, or one template, as the native interface describes it.
    let get = |reference: &'static str| {
        let (http, url, key) = (w.gw.http.clone(), format!("{}/gw/v1/models/{reference}", w.gw.data), w.fx.key.clone());
        async move {
            let r = http.get(url).bearer_auth(key).send().await.unwrap();
            (r.status().as_u16(), r.json::<Value>().await.unwrap())
        }
    };
    let (st, m) = get("anthropic/claude").await;
    assert_eq!((st, m["provider"].as_str(), m["max_output_tokens"].as_u64()), (200, Some("anthropic"), Some(16000)));
    assert_eq!(m["capabilities"]["reasoning"], "adaptive");
    let (st, t) = get("@planner").await;
    assert_eq!(
        (st, t["model"].as_str(), t["spec"]["max_output_tokens"].as_u64()),
        (200, Some("anthropic/claude"), Some(400))
    );
    let (st, e) = get("openai/nothing").await;
    assert_eq!((st, e["error"]["type"].as_str()), (404, Some("model_not_found")));
}

#[tokio::test]
async fn provider_tools_are_checked_whichever_interface_asks_for_them() {
    let w = setup().await;
    let (st, v) =
        w.gw.api(
            "PUT",
            &format!("/orgs/{}/workspaces/{}", w.fx.org, w.fx.ws),
            Some(
                json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "provider_tools": ["bash"] } }),
            ),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let hi = json!([{ "role": "user", "content": "hi" }]);

    // An OpenAI-interface caller asks for Anthropic's web search as a tool of its own type.
    let openai = |tools: Value| json!({ "model": "anthropic/claude", "messages": hi, "tools": tools });
    let r =
        post(&w, "/v1/chat/completions", &[], openai(json!([{ "type": "web_search_20260209", "name": "web_search" }])))
            .await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (403, Some("not_allowed")), "{}", r.text);
    assert_eq!(r.json()["error"]["type"], "permission_error", "in OpenAI's envelope");

    // The native interface passes Anthropic's MCP connector through `provider_options`.
    let native = json!({
        "model": "anthropic/claude",
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }],
        "provider_options": { "anthropic": { "mcp_servers": [{ "type": "url", "url": "https://mcp.example", "name": "x" }] } }
    });
    let r = post(&w, "/gw/v1/chat", &[], native).await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (403, Some("not_allowed")), "{}", r.text);
    assert!(w.mock.captured().is_empty(), "nothing reached the provider");

    // An allowed provider tool, and the caller's own functions, pass.
    let r = post(
        &w,
        "/v1/chat/completions",
        &[],
        openai(json!([
            { "type": "bash_20250124", "name": "bash" },
            { "type": "function", "function": { "name": "f", "parameters": { "type": "object" } } }
        ])),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.mock.captured()[0].json().unwrap();
    assert_eq!(sent["tools"][0]["type"], "bash_20250124");
}
