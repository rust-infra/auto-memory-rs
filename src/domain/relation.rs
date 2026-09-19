//! Relations: `- relation_type [[target]] (context)` lines and prose wikilinks.

use serde::Serialize;

use crate::domain::permalink::RelationType;

/// One directed relation extracted from a note body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Relation {
    /// Relation label (`depends_on`, `implemented by`, `links_to`, ...).
    #[serde(rename = "type")]
    pub relation_type: RelationType,
    /// Raw target text inside `[[...]]`, including any `|label` suffix.
    pub target: String,
    /// Optional trailing `(context)`.
    pub context: Option<String>,
}

impl Relation {
    /// Render the canonical markdown form used when writing notes.
    pub fn to_markdown(&self) -> String {
        let mut line = format!("- {} [[{}]]", self.relation_type, self.target);
        if let Some(context) = &self.context {
            line.push_str(&format!(" ({context})"));
        }
        line
    }
}
