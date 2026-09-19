//! Domain model.
//!
//! Types here are storage-agnostic: they describe Markdown semantics, not SQL rows.

pub mod dateparser;
pub mod document;
pub mod entity;
pub mod ids;
pub mod note_type;
pub mod observation;
pub mod permalink;
pub mod relation;
pub mod search;
pub mod timeframe;

pub use dateparser::parse_after_date;
pub use document::{DocumentTimestamps, ParsedDocument};
pub use entity::Frontmatter;
pub use ids::{DocumentId, EntityId, ProjectId};
pub use note_type::normalize_note_type;
pub use observation::Observation;
pub use permalink::{Permalink, RelationType};
pub use relation::Relation;
pub use search::{SearchItemType, SearchQuery, SearchResult};
