//! Helpers shared by the mock tests.

use std::time::Duration;

use futures_util::StreamExt;
use pacod_gw_testkit::mock::MockProvider;
use reqwest::{RequestBuilder, Response};
use serde_json::Value;

/// The user message most tests send, and what the mock derives from it.
pub const USER: &str = "hello world";
pub const REPLY: &str = "echo: hello world";

pub fn client() -> reqwest::Client {
    reqwest::Client::new()
}

pub fn post(mock: &MockProvider, path: &str) -> RequestBuilder {
    client().post(format!("{}{path}", mock.base_url()))
}

pub fn get(mock: &MockProvider, path: &str) -> RequestBuilder {
    client().get(format!("{}{path}", mock.base_url()))
}

/// A POST with the headers the Anthropic API requires.
pub fn anthropic_post(mock: &MockProvider, path: &str) -> RequestBuilder {
    post(mock, path).header("x-api-key", "sk-ant-test").header("anthropic-version", "2023-06-01")
}

pub fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|v| v.to_str().ok())
}

/// One server-sent event.
#[derive(Debug)]
pub struct Event {
    pub event: Option<String>,
    pub data: String,
}

impl Event {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.data).expect("event data is JSON")
    }
}

/// Parses complete events; an incomplete tail (a broken stream) is ignored.
pub fn sse_events(body: &str) -> Vec<Event> {
    let body = body.replace("\r\n", "\n");
    let complete = body.rfind("\n\n").map_or("", |end| &body[..end]);
    complete
        .split("\n\n")
        .filter(|block| !block.is_empty())
        .map(|block| {
            let mut event = Event { event: None, data: String::new() };
            for line in block.lines() {
                if let Some(name) = line.strip_prefix("event: ") {
                    event.event = Some(name.to_owned());
                } else if let Some(data) = line.strip_prefix("data: ") {
                    event.data.push_str(data);
                }
            }
            event
        })
        .collect()
}

/// How reading a response body ended.
#[derive(Debug, PartialEq, Eq)]
pub enum BodyEnd {
    Complete,
    Broken,
    /// Nothing arrived for the given time while the connection stayed open.
    Silent,
}

/// Reads the body chunk by chunk until it ends, breaks, or goes silent for `patience`.
pub async fn read_body(response: Response, patience: Duration) -> (Vec<Vec<u8>>, BodyEnd) {
    let mut stream = response.bytes_stream();
    let mut chunks = Vec::new();
    loop {
        match tokio::time::timeout(patience, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunks.push(chunk.to_vec()),
            Ok(Some(Err(_))) => return (chunks, BodyEnd::Broken),
            Ok(None) => return (chunks, BodyEnd::Complete),
            Err(_) => return (chunks, BodyEnd::Silent),
        }
    }
}

pub fn text(chunks: &[Vec<u8>]) -> String {
    String::from_utf8(chunks.concat()).expect("body is UTF-8")
}
