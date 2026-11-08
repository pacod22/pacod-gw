//! Which tools a caller may be served (catalogue MCP-02): deny by default. A key names the
//! workspace's bundles it may use; a bundle allows servers or single tools and denies others;
//! a server must be open to the workspace; and a tool must be approved. Bundle entries name a
//! tool by its own name on its server, which an alias set at approval does not change.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::model::{ToolBundle, ToolPin};
use crate::snapshot::{OrgRt, ToolServerRt, WorkspaceRt};

/// One entry of a bundle's `allow` or `deny`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// `mcp:<server>/*`: every approved tool of a server.
    Server(String),
    /// `mcp:<server>/<tool>`: one tool, by its own name on the server.
    Tool(String, String),
}

impl Item {
    pub fn parse(text: &str) -> Result<Self, String> {
        let rest = text.strip_prefix("mcp:").ok_or_else(|| format!("{text:?}: an entry starts with mcp:"))?;
        let (server, tool) =
            rest.split_once('/').ok_or_else(|| format!("{text:?}: mcp:<server>/* or mcp:<server>/<tool>"))?;
        if !super::names::is_server_name(server) {
            return Err(format!("{text:?}: {server:?} is not a tool server's name"));
        }
        match tool {
            "" => Err(format!("{text:?}: name a tool, or * for every tool of the server")),
            "*" => Ok(Self::Server(server.to_owned())),
            t => Ok(Self::Tool(server.to_owned(), t.to_owned())),
        }
    }

    pub fn matches(&self, server: &str, tool: &str) -> bool {
        match self {
            Self::Server(s) => s == server,
            Self::Tool(s, t) => s == server && t == tool,
        }
    }
}

/// Whether a bundle grants a server's tool. Entries that do not parse were refused when the
/// bundle was saved, and grant nothing here.
pub fn grants(bundle: &ToolBundle, server: &str, tool: &str) -> bool {
    let any = |entries: &[String]| entries.iter().filter_map(|e| Item::parse(e).ok()).any(|i| i.matches(server, tool));
    any(&bundle.allow) && !any(&bundle.deny)
}

/// One tool a caller may be served.
pub struct Granted<'a> {
    pub server: &'a Arc<ToolServerRt>,
    pub pin: &'a ToolPin,
}

/// The tools a caller of `workspace` may be served through `bundles`, by the name callers call
/// them by, in that name's order.
pub fn granted<'a>(org: &'a OrgRt, workspace: &WorkspaceRt, bundles: &[&ToolBundle]) -> BTreeMap<&'a str, Granted<'a>> {
    let mut out = BTreeMap::new();
    if bundles.is_empty() {
        return out;
    }
    for server in org.tool_servers.values().filter(|s| s.open_to(&workspace.id)) {
        for (exposed, pin) in &server.tools {
            if bundles.iter().any(|b| grants(b, &server.doc.name, &pin.tool)) {
                out.insert(exposed.as_str(), Granted { server, pin });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;

    fn bundle(allow: &[&str], deny: &[&str]) -> ToolBundle {
        ToolBundle {
            name: "b".into(),
            description: String::new(),
            allow: allow.iter().map(|s| (*s).to_owned()).collect(),
            deny: deny.iter().map(|s| (*s).to_owned()).collect(),
            status: Status::Active,
        }
    }

    #[test]
    fn a_bundle_grants_what_it_allows_and_does_not_deny() {
        let b = bundle(&["mcp:github/*", "mcp:ap-mcp/get_fact"], &["mcp:github/delete_repo"]);
        assert!(grants(&b, "github", "search_issues"));
        assert!(!grants(&b, "github", "delete_repo"), "deny wins");
        assert!(grants(&b, "ap-mcp", "get_fact"));
        assert!(!grants(&b, "ap-mcp", "list_facts"));
        assert!(!grants(&b, "gitlab", "search_issues"));
        assert!(!grants(&bundle(&[], &[]), "github", "x"), "nothing by default");
    }

    #[test]
    fn entries_have_one_form() {
        assert_eq!(Item::parse("mcp:github/*"), Ok(Item::Server("github".into())));
        assert_eq!(Item::parse("mcp:github/repo.search"), Ok(Item::Tool("github".into(), "repo.search".into())));
        for bad in ["github/*", "mcp:github", "mcp:github/", "mcp:Git_Hub/*", "a2a:researcher"] {
            assert!(Item::parse(bad).is_err(), "{bad}");
        }
    }
}
