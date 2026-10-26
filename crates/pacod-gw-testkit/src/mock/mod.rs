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

pub use behavior::{Behavior, BehaviorError, Fail};
pub use reply::{count_tokens, reply_text, thinking_text, tool_arguments};
pub use server::{CapturedRequest, MockProvider};
