//! Markdown parsing: frontmatter, observations, relations, and wikilinks.
//!
//! Pure functions over text — no filesystem, database, or MCP dependencies, so the
//! parse layer can be compared directly against `tests/golden/parse/`.

pub mod edit;
pub mod frontmatter;
pub mod observations;
pub mod parser;
pub mod relations;
pub mod serialize;
pub mod wikilinks;

pub use edit::{EditOperation, EditOptions, apply_edit_operation, merge_metadata_into_markdown};
pub use parser::parse_document;
pub use serialize::{merge_frontmatter, split_frontmatter};
