//! Deterministic derivations: what the mock answers, how many tokens it reports, and how a
//! streamed string is cut into chunks. Nothing here depends on time or randomness.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;

use super::behavior::Behavior;

/// The `created` timestamp of every OpenAI-compatible response (2023-11-14T22:13:20Z).
pub(crate) const CREATED: u64 = 1_700_000_000;

/// The token count the mock reports for `text`: its characters divided by four, rounded up.
///
/// Input tokens are `count_tokens` of all prompt text of the request concatenated without
/// separators; output tokens are `count_tokens` of the reply text (or of the tool arguments).
pub fn count_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// The reply text for a request whose last user message has the text `user_text`.
pub fn reply_text(user_text: &str) -> String {
    format!("echo: {user_text}")
}

/// The reasoning text emitted with the `thinking` option.
pub fn thinking_text(user_text: &str) -> String {
    format!("thinking about: {user_text}")
}

/// The JSON text of the arguments of a `tool=<name>` call: `{"input":"<user_text>"}`.
pub fn tool_arguments(user_text: &str) -> String {
    json!({ "input": user_text }).to_string()
}

/// FNV-1a over the parts, with a separator so that part boundaries matter. Stable across Rust
/// versions, unlike the standard hasher, so ids never change under a toolchain update.
pub(crate) fn fnv1a(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |byte: u8| {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for part in parts {
        part.iter().copied().for_each(&mut feed);
        feed(0xff);
    }
    hash
}

/// An opaque, base64 signature for a thinking block.
pub(crate) fn signature(seed: u64) -> String {
    let mut bytes = Vec::with_capacity(48);
    for round in 0u64..6 {
        let word = fnv1a(&[&seed.to_be_bytes(), &round.to_be_bytes()]);
        bytes.extend_from_slice(&word.to_be_bytes());
    }
    STANDARD.encode(bytes)
}

/// Cuts a streamed string into content chunks: `chunks` nearly equal pieces (at most one per
/// character), or one piece per word when no count is given.
pub(crate) fn split_text(text: &str, chunks: Option<usize>) -> Vec<String> {
    match chunks {
        Some(count) => split_even(text, count),
        None => split_words(text),
    }
}

/// Each piece but the first starts with the whitespace that precedes its word.
fn split_words(text: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut after_space = true;
    for ch in text.chars() {
        let space = ch.is_whitespace();
        if space && !after_space {
            pieces.push(std::mem::take(&mut current));
        }
        current.push(ch);
        after_space = space;
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

fn split_even(text: &str, count: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let count = count.clamp(1, chars.len());
    let (base, extra) = (chars.len() / count, chars.len() % count);
    let mut pieces = Vec::with_capacity(count);
    let mut start = 0;
    for index in 0..count {
        let len = base + usize::from(index < extra);
        pieces.push(chars[start..start + len].iter().collect());
        start += len;
    }
    pieces
}

/// What the model "said": text, or a call to a tool.
pub(crate) enum Output {
    Text(String),
    Tool {
        name: String,
        /// The arguments as JSON text.
        arguments: String,
    },
}

/// Provider-neutral token counts; each provider maps them onto its own usage fields.
pub(crate) struct Usage {
    /// Prompt tokens that were not read from the cache.
    pub input: u64,
    /// Tokens of the visible output, without reasoning.
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// One answer, derived from the request and its scripted behaviour.
pub(crate) struct Turn {
    /// A hash of the request path and body; the source of every id in the response.
    pub seed: u64,
    /// The model the response reports.
    pub model: String,
    pub thinking: Option<String>,
    pub output: Output,
    /// `None` with `no_usage`.
    pub usage: Option<Usage>,
    /// The service tier the response reports, when the script names one.
    pub tier: Option<String>,
    stop: Option<String>,
    chunks: Option<usize>,
}

impl Turn {
    /// `prompt` is all prompt text of the request, `user_text` the text of its last user
    /// message, and `text` replaces the echo reply (for structured output).
    pub fn new(
        seed: u64,
        request_model: &str,
        prompt: &str,
        user_text: &str,
        text: Option<String>,
        behavior: &Behavior,
    ) -> Self {
        let thinking = behavior.thinking.then(|| thinking_text(user_text));
        let output = match &behavior.tool {
            Some(name) => Output::Tool { name: name.clone(), arguments: tool_arguments(user_text) },
            None => Output::Text(behavior.reply.clone().or(text).unwrap_or_else(|| reply_text(user_text))),
        };
        let usage = (!behavior.no_usage).then(|| Usage {
            input: count_tokens(prompt),
            output: match &output {
                Output::Text(text) => count_tokens(text),
                Output::Tool { arguments, .. } => count_tokens(arguments),
            },
            reasoning: behavior.reasoning.unwrap_or_else(|| thinking.as_deref().map_or(0, count_tokens)),
            cache_read: behavior.cache_read.unwrap_or(0),
            cache_write: behavior.cache_write.unwrap_or(0),
        });
        Self {
            seed,
            model: behavior.model.clone().unwrap_or_else(|| request_model.to_owned()),
            thinking,
            output,
            usage,
            tier: behavior.tier.clone(),
            stop: behavior.stop.clone(),
            chunks: behavior.chunks,
        }
    }

    /// The scripted stop reason, or the provider's default for text or for a tool call.
    pub fn stop_reason<'a>(&'a self, text: &'a str, tool: &'a str) -> &'a str {
        match (&self.stop, &self.output) {
            (Some(stop), _) => stop,
            (None, Output::Text(_)) => text,
            (None, Output::Tool { .. }) => tool,
        }
    }

    /// The content chunks of one streamed string.
    pub fn pieces(&self, text: &str) -> Vec<String> {
        split_text(text, self.chunks)
    }

    pub fn signature(&self) -> String {
        signature(self.seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_a_quarter_of_the_characters_rounded_up() {
        assert_eq!(count_tokens(""), 0);
        assert_eq!(count_tokens("a"), 1);
        assert_eq!(count_tokens("abcd"), 1);
        assert_eq!(count_tokens("abcde"), 2);
        assert_eq!(count_tokens("echo: hello world"), 5);
        // Characters, not bytes.
        assert_eq!(count_tokens("\u{e9}\u{e9}\u{e9}\u{e9}"), 1);
    }

    #[test]
    fn replies_are_derived_from_the_user_text() {
        assert_eq!(reply_text("hi"), "echo: hi");
        assert_eq!(thinking_text("hi"), "thinking about: hi");
        assert_eq!(tool_arguments("say \"hi\""), r#"{"input":"say \"hi\""}"#);
    }

    #[test]
    fn default_chunks_are_words() {
        assert_eq!(split_text("echo: hello  world", None), ["echo:", " hello", "  world"]);
        assert_eq!(split_text("  lead", None), ["  lead"]);
        assert!(split_text("", None).is_empty());
    }

    #[test]
    fn counted_chunks_are_nearly_equal_and_lossless() {
        assert_eq!(split_text("abcdefgh", Some(3)), ["abc", "def", "gh"]);
        assert_eq!(split_text("abc", Some(8)), ["a", "b", "c"]);
        assert_eq!(split_text("h\u{e9}llo", Some(2)), ["h\u{e9}l", "lo"]);
        assert!(split_text("", Some(4)).is_empty());
        for count in 1..20 {
            let text = "echo: the quick brown fox";
            assert_eq!(split_text(text, Some(count)).concat(), text);
        }
    }

    #[test]
    fn hash_is_fnv1a_and_respects_part_boundaries() {
        // The published FNV-1a 64 test vector for the empty input, before any separator.
        assert_eq!(fnv1a(&[]), 0xcbf2_9ce4_8422_2325);
        assert_ne!(fnv1a(&[b"ab", b"c"]), fnv1a(&[b"a", b"bc"]));
        assert_eq!(fnv1a(&[b"ab", b"c"]), fnv1a(&[b"ab", b"c"]));
    }

    #[test]
    fn signature_is_stable_base64() {
        let first = signature(42);
        assert_eq!(first, signature(42));
        assert_ne!(first, signature(43));
        assert_eq!(STANDARD.decode(&first).unwrap().len(), 48);
    }
}
