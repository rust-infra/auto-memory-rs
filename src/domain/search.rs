//! Search domain types (field names follow the reference JSON contract).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use strum::{EnumIter, EnumString, IntoStaticStr};

/// Kinds of indexed search rows.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumIter, EnumString, IntoStaticStr,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum SearchItemType {
    /// A whole note.
    Entity,
    /// One observation row.
    Observation,
    /// One relation row.
    Relation,
}

/// Query parameters passed to the search layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Full-text query.
    pub query: Option<String>,
    /// Exact permalink filter.
    pub permalink: Option<String>,
    /// Glob permalink filter (`*` supported).
    pub permalink_match: Option<String>,
    /// Title filter.
    pub title: Option<String>,
    /// Note type filter (frontmatter `type`).
    pub note_types: Vec<String>,
    /// Indexed row type filter.
    pub entity_types: Vec<SearchItemType>,
    /// Observation category filter.
    pub categories: Vec<String>,
    /// Tag filter.
    pub tags: Vec<String>,
    /// One-based page number.
    pub page: u32,
    /// Page size.
    pub page_size: u32,
    /// Optional per-query similarity threshold.
    pub min_similarity: Option<f32>,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            query: None,
            permalink: None,
            permalink_match: None,
            title: None,
            note_types: Vec::new(),
            entity_types: Vec::new(),
            categories: Vec::new(),
            tags: Vec::new(),
            page: 1,
            page_size: 10,
            min_similarity: None,
        }
    }
}

/// One search result, matching the reference `SearchResult` shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    /// Result title.
    pub title: String,
    /// Indexed row type.
    #[serde(rename = "type")]
    pub item_type: SearchItemType,
    /// Retrieval score (bm25-derived, cosine similarity, or fused score).
    pub score: f32,
    /// Entity permalink, when applicable.
    pub entity: Option<String>,
    /// Stable external identifier.
    pub external_id: Option<String>,
    /// Result permalink.
    pub permalink: Option<String>,
    /// Body/content preview.
    pub content: Option<String>,
    /// Matched vector chunk or content snippet.
    pub matched_chunk: Option<String>,
    /// Project-relative file path.
    pub file_path: String,
    /// Indexed modification timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Row metadata (`note_type` for entities, `tags` for observations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    /// Owning entity id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<i64>,
    /// Observation id (observation rows only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_id: Option<i64>,
    /// Relation id (relation rows only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation_id: Option<i64>,
    /// Observation category (observation rows only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Source entity permalink (relation rows only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_entity: Option<String>,
    /// Target entity permalink (relation rows only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_entity: Option<String>,
    /// Relation label (relation rows only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation_type: Option<String>,
}
