//! Scripted mock LLM providers.
//!
//! One server emulates the OpenAI-compatible, Anthropic, Gemini (AI Studio and Vertex AI) and
//! Ollama APIs, and Google's token endpoint for service accounts. It records
//! every request byte for byte, answers deterministically (the reply is derived from the
//! request and token counts from text lengths), and can be scripted per request to delay,
//! fail, break or stall. `crates/pacod-gw-testkit/README.md` documents the endpoints, the
//! `x-mock` options and the control endpoints.
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use pacod_gw_testkit::mock::{Behavior, MockProvider};
//!
//! let mock = MockProvider::spawn().await;
//! mock.set_script(Behavior::parse_all(["fail=503x2,retry_after=1"])?);
//! // ... point the gateway at mock.base_url() and make it call the provider ...
//! for request in mock.captured() {
//!     println!("{} {} ({} bytes)", request.method, request.path, request.body.len());
//! }
//! # Ok(())
//! # }
//! ```

mod anthropic;
mod audio;
mod behavior;
mod flavour;
mod gemini;
mod google;
mod images;
mod objects;
mod ollama;
mod openai;
mod openai_objects;
mod realtime;
mod reply;
mod rerank;
mod responses;
mod server;
mod stream;
mod typesafe;
mod websocket;

use axum::Router;

pub use behavior::{Behavior, BehaviorError, Fail};
pub use reply::{count_tokens, reply_text, thinking_text, tool_arguments};
pub use server::{CapturedRequest, MockProvider};

use crate::a2a::MockAgents;
use crate::mcp::{Era, MockTools, mock_tools};

/// The tool servers and the agent `gw-mock` serves next to the providers.
#[derive(Clone)]
pub struct ToolsAndAgents {
    /// The 2026-07-28 revision, at `/mcp`.
    pub modern: MockTools,
    /// The 2025-11-25 revision, at `/mcp/legacy`.
    pub legacy: MockTools,
    /// The A2A agent: its card at `/.well-known/agent-card.json`, naming the interface at the
    /// host the request named, and the interface at `/a2a`.
    pub agent: MockAgents,
}

/// What `gw-mock` serves besides the providers, keeping at most `capture_limit` requests of
/// each (0: none), with their control endpoints (`crates/pacod-gw-testkit/README.md`):
///
/// - `POST /mcp` and `POST /mcp/legacy`: a tool server of each revision, with [`mock_tools`];
/// - `GET /.well-known/agent-card.json` and `POST /a2a`: the agent;
/// - `POST`, `DELETE /__mock/mcp/modern/tools` and `/__mock/mcp/legacy/tools`, and
///   `/__mock/a2a/card`: change a definition (a new hash at the next probe) or restore it.
pub fn tools_and_agents(capture_limit: usize) -> (Router, ToolsAndAgents) {
    let mocks = ToolsAndAgents {
        modern: MockTools::with_tools(Era::Modern, mock_tools()),
        legacy: MockTools::with_tools(Era::Legacy, mock_tools()),
        agent: MockAgents::new(),
    };
    mocks.modern.set_capture_limit(capture_limit);
    mocks.legacy.set_capture_limit(capture_limit);
    mocks.agent.set_capture_limit(capture_limit);
    let router = Router::new()
        .merge(mocks.modern.routes("/mcp"))
        .merge(mocks.legacy.routes("/mcp/legacy"))
        .merge(mocks.agent.routes())
        .merge(mocks.modern.control_routes("/__mock/mcp/modern/tools"))
        .merge(mocks.legacy.control_routes("/__mock/mcp/legacy/tools"))
        .merge(mocks.agent.control_routes("/__mock/a2a/card"));
    (router, mocks)
}
