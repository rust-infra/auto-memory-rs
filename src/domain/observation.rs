//! Observations: `- [category] content #tag (context)` lines.

use serde::Serialize;

use crate::domain::valid_time::ValidTime;

/// One observation extracted from a note body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observation {
    /// Optional category bracket (`decision`, `fact`, ...). `None` when absent;
    /// the reference indexer stores the DB default `note` later.
    pub category: Option<String>,
    /// Observation text (tags are kept inside the text, as in the reference).
    pub content: String,
    /// Inline `#tags` extracted from the content.
    pub tags: Vec<String>,
    /// Optional trailing `(context)`.
    pub context: Option<String>,
    /// When the claim holds, from a leading `@effective`/`@valid`/... qualifier
    /// (upstream 0.24.0 #1426).
    ///
    /// `None` when the observation carries no qualifier, or when the qualifier did not
    /// parse — in which case it stays part of `content` so a typo is visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_time: Option<ValidTime>,
}

impl Observation {
    /// Render the canonical markdown form used when writing notes.
    pub fn to_markdown(&self) -> String {
        let category = self.category.as_deref().unwrap_or("Note");
        let mut line = format!("- [{category}] {}", self.content);
        if let Some(context) = &self.context {
            line.push_str(&format!(" ({context})"));
        }
        line
    }
}
