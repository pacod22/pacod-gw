//! The names callers call tools by.
//!
//! The gateway serves the tools of several servers on one endpoint, so each tool is exposed
//! under its server's name: `<server>__<tool>`. Agent frameworks turn MCP tools into the
//! functions of a model call, and OpenAI, Anthropic and Gemini accept function names of at most
//! 64 letters, digits, underscores and hyphens, with a letter or an underscore first. MCP's own
//! rule is wider (dots, 128 characters), so a tool whose name does not fit is exposed under an
//! alias an administrator sets, never under a name the gateway shortened by itself.
//!
//! A server's name is a slug of lower-case letters, digits and hyphens: it has no underscore,
//! so the first `__` of an exposed name ends it.

use crate::model::is_slug;
use crate::model::tools::MAX_SERVER_NAME;

/// Between a server's name and its tool's.
pub const SEPARATOR: &str = "__";
/// The longest function name providers accept.
pub const MAX_EXPOSED: usize = 64;

/// Whether providers accept `name` as a function's name.
pub fn is_function_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.len() <= MAX_EXPOSED
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether `name` may name a tool server.
pub fn is_server_name(name: &str) -> bool {
    is_slug(name) && name.len() <= MAX_SERVER_NAME && name.starts_with(|c: char| c.is_ascii_lowercase())
}

/// The name a tool is exposed under unless an alias is set; `None` when that would not be a
/// function name, and an alias is needed.
pub fn exposed(server: &str, tool: &str) -> Option<String> {
    let name = format!("{server}{SEPARATOR}{tool}");
    is_function_name(&name).then_some(name)
}

/// Whether `name` may expose a tool of `server`: a function name in the server's namespace.
pub fn fits(server: &str, name: &str) -> bool {
    is_function_name(name)
        && name.strip_prefix(server).and_then(|rest| rest.strip_prefix(SEPARATOR)).is_some_and(|t| !t.is_empty())
}

/// The server's part of an exposed name.
pub fn server_of(name: &str) -> Option<&str> {
    name.split_once(SEPARATOR).map(|(server, _)| server).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposed_names_are_function_names_in_the_servers_namespace() {
        assert_eq!(exposed("github", "search_issues").as_deref(), Some("github__search_issues"));
        assert_eq!(exposed("github", "repo.search"), None, "a dot needs an alias");
        assert_eq!(exposed("github", &"x".repeat(60)), None, "too long for providers");
        assert!(fits("github", "github__repo_search"));
        assert!(!fits("github", "gitlab__repo_search"));
        assert!(!fits("github", "github__"));
        assert!(!fits("github", "github__repo.search"));
        assert_eq!(server_of("ap-mcp__get_fact"), Some("ap-mcp"));
        assert_eq!(server_of("discover_resources"), None, "the broker's tools are not namespaced");
    }

    #[test]
    fn server_names_are_short_slugs_that_start_with_a_letter() {
        assert!(is_server_name("ap-mcp"));
        assert!(!is_server_name("1password"));
        assert!(!is_server_name("my_server"));
        assert!(!is_server_name(&"a".repeat(21)));
        assert!(!is_function_name("1abc") && !is_function_name("") && is_function_name("_x-1"));
    }
}
