//! What a tool server offers now (catalogue MCP-10): its tools as it lists them, each with the
//! part an approval covers and that part's hash. The management API stores what a probe saw
//! outside the configuration (`connector_probes`); only an approval writes configuration.

use std::collections::HashSet;

use serde_json::Value;

use super::client::{ClientError, Target, ToolClient};
use super::pin;
use crate::model::tools::MAX_TOOLS_PER_SERVER;

/// What one probe saw.
#[derive(Debug, Clone)]
pub struct Probed {
    /// The revision the server speaks.
    pub version: String,
    pub tools: Vec<Seen>,
}

/// One tool as the server lists it now.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    /// The tool's own name.
    pub name: String,
    /// What an approval covers ([`pin::definition`]).
    pub definition: Value,
    pub hash: String,
}

/// Asks the server which revision it speaks and which tools it has.
pub async fn probe(client: &ToolClient, target: &Target<'_>) -> Result<Probed, ClientError> {
    let link = client.connect(target).await?;
    let listed = client.list_tools(target, &link, MAX_TOOLS_PER_SERVER).await?;
    let mut names = HashSet::new();
    let mut tools = Vec::with_capacity(listed.len());
    for tool in listed {
        let name = tool["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| ClientError::Malformed("a tool without a name".into()))?;
        if !names.insert(name.to_owned()) {
            return Err(ClientError::Malformed(format!("the tool {name} is listed twice")));
        }
        let definition = pin::definition(&tool);
        tools.push(Seen { name: name.to_owned(), hash: pin::hash(&definition), definition });
    }
    Ok(Probed { version: link.version().to_owned(), tools })
}
