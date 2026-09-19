//! MCP adapter: the stdio and Streamable HTTP transports over the local core tools.

pub(crate) mod helpers;
pub mod http;
pub mod server;
pub mod tools;

pub use server::{MCP_PROTOCOL_VERSION, McpServer, SERVER_NAME, ensure_project, tool_definitions};
