//! Configuration knobs that change observable behavior.
//!
//! Defaults mirror Basic Memory 0.23.2 (see `docs/reference.md`). Unknown keys in
//! a reference `config.json` are ignored so the same file can be loaded.

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Behavior-affecting configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Prefix generated permalinks with the project slug (reference default: `true`).
    pub permalinks_include_project: bool,
    /// Write derived `title`/`type`/`permalink` into files that lack frontmatter
    /// (reference default: `true`; this mutates user markdown).
    pub ensure_frontmatter_on_sync: bool,
    /// Never generate permalinks (reference default: `false`).
    pub disable_permalinks: bool,
    /// Watch and index local file changes (reference default: `true`).
    pub index_changes: bool,
    /// Rewrite permalinks when a file moves (reference default: `false`).
    pub update_permalinks_on_move: bool,
}

impl Default for Config {
    fn default() -> Self {
        // Reference defaults from basic_memory/config_models.py (0.23.2).
        Self {
            permalinks_include_project: true,
            ensure_frontmatter_on_sync: true,
            disable_permalinks: false,
            index_changes: true,
            update_permalinks_on_move: false,
        }
    }
}

impl Config {
    /// Parse configuration from JSON, ignoring unknown reference keys.
    pub fn from_json_str(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }
}
