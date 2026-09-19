//! Parsed document model: frontmatter + body + extracted semantic layer.

use serde::Serialize;

use crate::domain::entity::Frontmatter;
use crate::domain::observation::Observation;
use crate::domain::relation::Relation;
use crate::domain::timeframe::{self, Instant};

/// A wikilink found in the note body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Wikilink {
    /// Raw inner text, including any `|label` suffix.
    pub raw_target: String,
    /// Target without the display label.
    pub target: String,
    /// Optional display label.
    pub label: Option<String>,
}

/// Storage-independent representation of one parsed markdown note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParsedDocument {
    /// Project-relative, slash-separated path.
    pub file_path: String,
    /// Whether the file had a YAML frontmatter block.
    pub had_frontmatter: bool,
    /// Whether a frontmatter block existed but failed to parse as YAML.
    pub frontmatter_error: bool,
    /// Normalized frontmatter.
    pub frontmatter: Frontmatter,
    /// Canonical `created` timestamp from frontmatter, when the note declares one.
    pub created: Option<Instant>,
    /// Canonical `modified` timestamp from frontmatter, when the note declares one.
    pub modified: Option<Instant>,
    /// Body content after the frontmatter block.
    pub content: String,
    /// Observations in document order.
    pub observations: Vec<Observation>,
    /// Relations in document order (explicit lines, then inline wikilinks).
    pub relations: Vec<Relation>,
    /// All wikilinks in document order.
    pub wikilinks: Vec<Wikilink>,
}

/// Index timestamps for one document.
///
/// The reference keeps `created`/`modified` in frontmatter as the canonical note
/// semantics and only falls back to file times when a note does not declare them
/// (`EntityMarkdown.created` / `.modified`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentTimestamps {
    /// Value written to `entity.created_at`.
    pub created_at: String,
    /// Value written to `entity.updated_at`.
    pub updated_at: String,
}

impl DocumentTimestamps {
    /// Current local time for both fields (reference `datetime.now()` fallback).
    pub fn now() -> Self {
        let now = timeframe::now_storage_timestamp();
        Self {
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Derive the stored timestamps from a parsed document.
    ///
    /// `file_created` / `file_updated` are the file metadata fallbacks the
    /// reference uses when frontmatter omits the canonical values.
    pub fn for_document(
        document: &ParsedDocument,
        file_created: Instant,
        file_updated: Instant,
    ) -> Self {
        Self {
            created_at: timeframe::storage_timestamp(document.created.unwrap_or(file_created)),
            updated_at: timeframe::storage_timestamp(document.modified.unwrap_or(file_updated)),
        }
    }
}
