//! Indexing pipeline: vault scan, parse, full rebuild, and incremental updates.

pub mod debounce;
pub mod document;
pub mod rebuild;
pub mod service;
pub mod watcher;

pub use debounce::{ChangeKind, Debouncer, FileEvent};
pub use document::{IndexedDocument, LoadOutcome, PermalinkPolicy, load_indexed_document};
pub use rebuild::{RebuildOptions, RebuildReport, rebuild_vault};
pub use service::{EmbeddingReport, IndexOptions, IndexOutcome, IndexService, ReconcileReport};
pub use watcher::{
    DEFAULT_WATCH_WINDOW, IgnoreRules, VaultWatcher, WATCH_TICK, WatchReport, log_watch_report,
    map_notify_event, shutdown_when, watch_once, watch_vault,
};
