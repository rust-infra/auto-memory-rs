//! Read models returned by the store (no SQL types leak into callers).

use serde::Serialize;
use serde_json::{Map, Value};

/// One registered project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectRow {
    /// Internal row id.
    pub id: i64,
    /// Stable external identifier.
    pub external_id: String,
    /// Project name.
    pub name: String,
    /// Project permalink (used as the generated-permalink prefix).
    pub permalink: String,
    /// Filesystem path of the vault.
    pub path: String,
}

/// One indexed note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntityRow {
    /// Internal row id.
    pub id: i64,
    /// Stable external identifier.
    pub external_id: String,
    /// Note title.
    pub title: String,
    /// Note type (frontmatter `type`).
    pub note_type: String,
    /// Permalink (project-prefixed unless explicitly set in frontmatter).
    pub permalink: Option<String>,
    /// Project-relative file path.
    pub file_path: String,
    /// SHA-256 checksum of the file at index time.
    pub checksum: Option<String>,
    /// Normalized frontmatter metadata.
    pub metadata: Map<String, Value>,
}

/// One entity row as the directory listing needs it.
///
/// Mirrors the reference `Entity` columns `DirectoryService.list_directory` reads:
/// the file path, the display metadata, and the `updated_at` timestamp used by the
/// explicit `updated_*` sort orders. Rows arrive in id order, which is the order the
/// reference repository returns them in (no `ORDER BY`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirectoryEntityRow {
    /// Internal row id.
    pub id: i64,
    /// Project-relative file path.
    pub file_path: String,
    /// Note title.
    pub title: String,
    /// Permalink (may be absent).
    pub permalink: Option<String>,
    /// Stable external identifier.
    pub external_id: String,
    /// Note type (frontmatter `type`).
    pub note_type: String,
    /// Stored content type (`text/markdown` for notes).
    pub content_type: String,
    /// Last-modified timestamp recorded at index time.
    pub updated_at: String,
}

/// One row produced by the reference `find_related` traversal.
///
/// Rows are returned in the reference order (`ORDER BY depth, type, id`, capped by
/// `max_results`), so callers must not re-sort them when building context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelatedRow {
    /// `entity` or `relation`.
    #[serde(rename = "type")]
    pub item_type: String,
    /// Entity or relation id.
    pub id: i64,
    /// Entity title, or `relation_type: to_name` for relations.
    pub title: String,
    /// Entity permalink (relations carry an empty permalink).
    pub permalink: String,
    /// Project-relative file path of the owning note.
    pub file_path: String,
    /// Relation source.
    pub from_id: Option<i64>,
    /// Relation target.
    pub to_id: Option<i64>,
    /// Relation label.
    pub relation_type: Option<String>,
    /// Raw relation target text.
    pub to_name: Option<String>,
    /// Traversal depth (`MIN(depth)` across the paths that reached this row).
    pub depth: i64,
    /// Seed entity this row was reached from.
    pub root_id: i64,
    /// Timestamp carried by the row.
    pub created_at: String,
}

/// One stored semantic chunk with its embedding.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorChunkRow {
    /// `search_vector_chunks` row id (the embedding key).
    pub id: i64,
    /// Owning entity.
    pub entity_id: i64,
    /// `type:id:index`.
    pub chunk_key: String,
    /// Chunk text that was embedded.
    pub chunk_text: String,
    /// SHA-256 of the chunk text.
    pub source_hash: String,
    /// Stored (L2-normalized) vector.
    pub embedding: Vec<f32>,
}

/// One row to write into the vector index.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorRow {
    /// Owning entity.
    pub entity_id: i64,
    /// `type:id:index`.
    pub chunk_key: String,
    /// Chunk text that was embedded.
    pub chunk_text: String,
    /// SHA-256 of the chunk text.
    pub source_hash: String,
    /// Fingerprint of the owning entity's chunks.
    pub entity_fingerprint: String,
    /// L2-normalized vector.
    pub embedding: Vec<f32>,
}

/// One `search_index` row, read back by id for hydration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRowView {
    /// Row id.
    pub id: i64,
    /// Display title.
    pub title: Option<String>,
    /// `entity`, `observation`, or `relation`.
    pub item_type: String,
    /// Row permalink.
    pub permalink: Option<String>,
    /// Project-relative file path.
    pub file_path: String,
    /// Body/content snippet.
    pub content_snippet: Option<String>,
    /// JSON metadata.
    pub metadata: Option<String>,
    /// Owning entity id.
    pub entity_id: Option<i64>,
    /// Observation category.
    pub category: Option<String>,
    /// Relation label.
    pub relation_type: Option<String>,
    /// Indexed modification timestamp.
    pub updated_at: Option<String>,
}

/// One observation row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservationRow {
    /// Internal row id.
    pub id: i64,
    /// Owning entity id.
    pub entity_id: i64,
    /// Observation category (`note` when the line had none).
    pub category: String,
    /// Observation text.
    pub content: String,
    /// Optional `(context)`.
    pub context: Option<String>,
    /// Inline tags.
    pub tags: Vec<String>,
}

/// One relation row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationRow {
    /// Source entity id.
    pub from_id: i64,
    /// Resolved target entity id, when the target exists.
    pub to_id: Option<i64>,
    /// Raw target text as written (`notes/simple`, `projects/beta|Beta`).
    pub to_name: String,
    /// Relation label.
    pub relation_type: String,
    /// Optional `(context)`.
    pub context: Option<String>,
}

/// Row counts for one project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counts {
    /// Indexed entities.
    pub entities: i64,
    /// Indexed observations.
    pub observations: i64,
    /// Indexed relations.
    pub relations: i64,
}
