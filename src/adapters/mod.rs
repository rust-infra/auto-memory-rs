//! Adapters: CLI, MCP, and filesystem entry points.
//!
//! Adapters translate inputs into application-service calls; they must not
//! contain parsing, ranking, or graph algorithms.

pub mod cli;
pub mod filesystem;
pub mod mcp;
