//! Adapters: the entry points that translate external input into application calls.
//!
//! `mcp` is the only one with a body. `cli` and `filesystem` are declared
//! placeholders — the command surface is `src/main.rs` and vault I/O is
//! `indexing`'s, so neither has code. See `specs/auto-memory-rs-spec.md` §6.
//!
//! Adapters translate inputs into application-service calls; they must not
//! contain parsing, ranking, or graph algorithms.

pub mod cli;
pub mod filesystem;
pub mod mcp;
