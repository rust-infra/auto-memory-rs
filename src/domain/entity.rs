//! Frontmatter model shared by the parser and the writer.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::domain::permalink::Permalink;

/// Normalized frontmatter of one note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Frontmatter {
    /// Note title; defaults to the file stem when absent.
    pub title: String,
    /// Note type (`note` when absent).
    #[serde(rename = "type")]
    pub note_type: String,
    /// Optional explicit permalink from frontmatter (kept verbatim).
    pub permalink: Option<Permalink>,
    /// Tags parsed from `tags` (list or comma-separated string).
    pub tags: Vec<String>,
    /// Full normalized metadata map (numbers/booleans become strings, as in the
    /// reference `normalize_frontmatter_metadata`).
    pub metadata: Map<String, Value>,
}
