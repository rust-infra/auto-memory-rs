//! Observations: `- [category] content #tag (context)` lines.

use serde::Serialize;

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
