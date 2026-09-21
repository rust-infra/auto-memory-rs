//! Error types shared across the core.

use thiserror::Error;

/// Convenience result alias for core operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by parsing, domain validation, and I/O.
#[derive(Debug, Error)]
pub enum Error {
    /// Frontmatter could not be parsed as YAML.
    #[error("invalid YAML frontmatter: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),

    /// A frontmatter field has an unusable shape (for example a non-string title).
    #[error("invalid frontmatter: {message}")]
    Frontmatter {
        /// Human-readable explanation.
        message: String,
    },

    /// A permalink failed validation.
    #[error("invalid permalink: {value}")]
    Permalink {
        /// The rejected value.
        value: String,
    },

    /// A relation type failed validation.
    #[error("invalid relation type: {value}")]
    RelationType {
        /// The rejected value.
        value: String,
    },

    /// A caller-supplied argument failed validation.
    ///
    /// Rendered verbatim so CLI/MCP error surfaces match the reference (`ValueError`);
    /// it carries no prefix of its own.
    #[error("{message}")]
    InvalidArgument {
        /// Reference-compatible message.
        message: String,
    },

    /// A timeframe expression could not be resolved.
    #[error("invalid timeframe: {value}")]
    Timeframe {
        /// The rejected value.
        value: String,
        /// Human-readable explanation.
        message: String,
    },

    /// Filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// SQLite failure.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Failure from the asynchronous SQLite connection thread.
    #[error("async sqlite error: {0}")]
    AsyncSqlite(#[from] tokio_rusqlite::Error),

    /// JSON serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// The local embedding runtime could not load or run the model.
    #[error("embedding runtime error: {message}")]
    Embedding {
        /// Human-readable explanation.
        message: String,
    },
}
