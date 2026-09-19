//! `auto-memory-rs` — a local-first Rust implementation of the Basic Memory core.
//!
//! The crate keeps the **observable behavior** of the reference implementation
//! (Basic Memory 0.23.2, see `docs/reference.md`) while organizing the code in
//! idiomatic Rust. Markdown is the source of truth; every index is rebuildable.
//!
//! Layering rule: adapters (CLI/MCP/filesystem) depend on application services,
//! which depend on the domain and infrastructure. The domain must not depend on
//! CLI, MCP, SQLite, or a concrete embedding runtime.
#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod adapters;
pub mod application;
pub mod config;
pub mod domain;
pub mod error;
pub mod graph;
pub mod hooks;
pub mod indexing;
pub mod markdown;
pub mod pycompat;
pub mod runtime;
pub mod schema;
pub mod search;
pub mod storage;
