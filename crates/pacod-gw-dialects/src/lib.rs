//! The provider-neutral model of pacod-gw and the translators between it and the dialects
//! clients and providers speak (concept sections 8.2 to 8.5).
//!
//! Everything in this crate is a pure function of its input: no I/O, no clock, no randomness.
//! The same input always gives the same bytes, which is what keeps provider prompt caches
//! hitting and reasoning blocks valid when a conversation passes through a translation.
//!
//! | Module | Client half (callers speak it) | Provider half (providers speak it) |
//! |---|---|---|
//! | [`openai`] | `parse_request`, `write_response`, `client_stream` | `build_request`, `read_response`, `provider_stream` |
//! | [`anthropic`] | the same | the same |
//! | [`native`] | the same | – (no provider speaks it) |
//! | [`gemini`] | the same | the same |

pub mod anthropic;
pub mod canonical;
pub mod embeddings;
pub mod error;
pub mod extract;
pub mod gemini;
pub mod native;
pub mod openai;
pub mod sse;
mod util;

use bytes::BytesMut;
use serde_json::Value;

pub use canonical::{Event, Notes, Profile, Request, Response, Stamp};
pub use error::DialectError;
pub use util::{add_hidden_reasoning, first_count, openai_usage_value, usage_from_openai};

/// Writes a response stream in a caller's dialect, one neutral event at a time.
pub trait ClientStream: Send {
    /// Sets what the gateway adds to the last event: the cost object and, on the native
    /// interface, the `gateway` block. Called before `MessageStop` is written, once the
    /// usage is known.
    fn finalize(&mut self, cost: Option<Value>, gateway: Option<Value>);

    /// Appends the bytes for `event` to `out`. Some events write nothing; the last one also
    /// writes the dialect's end marker (`data: [DONE]` on the OpenAI dialect).
    fn write(&mut self, event: &Event, out: &mut BytesMut);

    /// The stream ended, after its last event or without one. Appends what the framing still
    /// owes (the closing bracket of Gemini's JSON-array stream).
    fn end(&mut self, _out: &mut BytesMut) {}
}

/// Reads a provider's response stream into neutral events.
pub trait ProviderStream: Send {
    /// Takes one server-sent event of the provider (`event` is empty where the dialect uses
    /// none) and appends the neutral events it gives rise to.
    fn read(&mut self, event: &str, data: &[u8], out: &mut Vec<Event>);

    /// The provider's stream ended. Appends what is still owed, for example the
    /// `MessageStop` of a stream whose provider sent no closing event.
    fn finish(&mut self, out: &mut Vec<Event>);
}
