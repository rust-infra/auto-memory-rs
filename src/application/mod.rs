//! Application services shared by the CLI and MCP adapters.

pub mod activity;
pub mod context;
pub mod directory;
pub mod note;
pub mod schema;
pub mod schema_text;
pub mod schema_tools;
pub mod search_text;

pub use activity::{
    ActivityContext, ActivityOptions, ActivityResult, recent_context, recent_rows,
    render_activity_text,
};
pub use context::{
    ContextOptions, ContextResult, EntitySummary, GraphContext, MemoryMetadata, ObservationSummary,
    RelationSummary, build_context,
};
pub use directory::{
    DEFAULT_DIRECTORY_PAGE_SIZE, DirectoryListResponse, DirectoryNode, DirectoryOptions,
    DirectorySortOrder, MAX_DIRECTORY_PAGE_SIZE, list_directory, render_directory_text,
};
pub use schema::{SchemaService, entity_frontmatter};
pub use schema_text::{
    format_drift_report, format_inference_report, format_validation_report, no_notes_guidance,
    no_schema_guidance, no_schemas_defined_guidance,
};
