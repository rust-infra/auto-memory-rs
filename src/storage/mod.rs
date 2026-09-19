//! Rebuildable SQLite index.
//!
//! Markdown stays the source of truth; everything in this module can be dropped
//! and recreated from the vault. Table/column names mirror Basic Memory 0.23.2.

pub mod records;
pub mod schema;
pub mod store;

pub use records::{
    Counts, DirectoryEntityRow, EntityRow, ObservationRow, ProjectRow, RelatedRow, RelationRow,
    SearchRowView, VectorChunkRow, VectorRow,
};
pub use schema::SCHEMA_VERSION;
pub use store::{Store, checksum_bytes, deterministic_uuid};
