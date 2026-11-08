//! The gateway as a client of tool servers (catalogue MCP-01, MCP-03, MCP-10, ADP-12): how it
//! names their tools for callers (`names`), how it pins a tool's definition (`pin`), how it
//! speaks MCP to them in both revisions (`client`), and how it probes what a server offers
//! (`probe`). The management API probes; the data plane (`dataplane::mcp`) calls.

pub mod client;
pub mod grant;
pub mod hub;
pub mod names;
pub mod pin;
pub mod probe;
