//! Tokio runtime plumbing for the two event-loop surfaces (`mcp`, `watch`).
//!
//! Only those two commands build a runtime. Every other subcommand is a one-shot
//! pass over the vault or the index, so it stays synchronous: paying for a worker
//! pool at startup to run `search` or `status` would be cost without benefit. The
//! rule behind that split is `docs/auto-memory-rs-spec.md` §6 — CPU- and
//! database-bound work stays synchronous; async covers transport, event loops and
//! background work. `docs/patterns.md` records where the boundary sits and what
//! would move it.

use std::future::Future;

use crate::error::{Error, Result};

/// Name given to the runtime's worker threads, so a stack dump of a stuck server
/// says which process it came from.
pub const THREAD_NAME: &str = "auto-memory";

/// Build the runtime the async adapters run on.
///
/// The multi-thread flavor is a requirement, not a tuning choice: both adapters hand
/// their synchronous core work (SQLite, ONNX, file reads) to
/// [`tokio::task::block_in_place`], which panics on a current-thread runtime because
/// there is no other worker to keep the reactor alive.
pub fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name(THREAD_NAME)
        .build()
        .map_err(Error::from)
}

/// Run `future` to completion on a fresh runtime.
///
/// This is the blocking entry point for callers that are not themselves async: the
/// CLI (`block_on` it from `main`) and the integration tests that drive
/// [`crate::indexing::watch_vault`] from a plain `#[test]`.
pub fn block_on<F: Future>(future: F) -> Result<F::Output> {
    Ok(runtime()?.block_on(future))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_on_drives_a_future_and_leaves_the_reactor_free_for_blocking_core_work() {
        // `block_in_place` panics on a current-thread runtime, so this also pins the
        // multi-thread requirement the adapters depend on.
        let value = block_on(async { tokio::task::block_in_place(|| 41) + 1 }).expect("runtime");
        assert_eq!(value, 42);
    }

    #[test]
    fn block_on_propagates_the_future_output() {
        let value = block_on(async { "ok" }).expect("runtime");
        assert_eq!(value, "ok");
    }
}
