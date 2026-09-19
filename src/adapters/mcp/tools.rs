//! `tools/list` schemas for every [`ToolName`].
//!
//! One arm per variant, so adding a tool is an enum variant plus a schema arm; the wire
//! name itself comes from `ToolName`'s `strum` rendering.

use serde_json::{Value, json};

use super::server::ToolName;

impl ToolName {
    /// The `tools/list` entry: description, required arguments, and JSON schema.
    ///
    /// The wire name is the variant's `strum` rendering, so an entry cannot disagree
    /// with the name `tools/call` dispatches on.
    pub(crate) fn definition(self) -> Value {
        let (description, required, properties): (&str, &[&str], Value) = match self {
            Self::WriteNote => (
                "Create a markdown note in the vault.",
                &["title", "content"],
                json!({
                    "title": { "type": "string" },
                    "content": { "type": "string" },
                    "directory": { "type": "string" },
                    "note_type": { "type": "string" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "overwrite": { "type": "boolean" },
                    "metadata": { "type": "object" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::ReadNote => (
                "Read a markdown note by title or permalink.",
                &["identifier"],
                json!({
                    "identifier": { "type": "string" },
                    "page": { "type": "integer" },
                    "page_size": { "type": "integer" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                    "include_frontmatter": { "type": "boolean" },
                }),
            ),
            Self::EditNote => (
                "Edit a note (append, prepend, find_replace, replace_section, insert section).",
                &["identifier", "operation"],
                json!({
                    "identifier": { "type": "string" },
                    "operation": { "type": "string" },
                    "content": { "type": "string" },
                    "section": { "type": "string" },
                    "find_text": { "type": "string" },
                    "expected_replacements": { "type": "integer" },
                    "replace_subsections": { "type": "boolean" },
                    "metadata": { "type": "object" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::MoveNote => (
                "Move or rename a note.",
                &["identifier"],
                json!({
                    "identifier": { "type": "string" },
                    "destination_path": { "type": "string" },
                    "destination_folder": { "type": "string" },
                    "is_directory": { "type": "boolean" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::DeleteNote => (
                "Delete a note from the vault and the index.",
                &["identifier"],
                json!({
                    "identifier": { "type": "string" },
                    "is_directory": { "type": "boolean" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::SearchNotes => (
                "Search indexed notes (FTS5 syntax).",
                &[],
                json!({
                    "query": { "type": "string" },
                    "search_type": {
                        "type": "string",
                        "enum": ["hybrid", "permalink", "semantic", "text", "title", "vector"],
                    },
                    "search_all_projects": { "type": "boolean" },
                    "page": { "type": "integer" },
                    "page_size": { "type": "integer" },
                    "title": { "type": "string" },
                    "permalink": { "type": "string" },
                    "permalink_match": { "type": "string" },
                    "note_types": { "type": "array", "items": { "type": "string" } },
                    "entity_types": { "type": "array", "items": { "type": "string" } },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "categories": { "type": "array", "items": { "type": "string" } },
                    "status": { "type": "string" },
                    "metadata_filters": { "type": "object" },
                    "after_date": { "type": "string" },
                    "min_similarity": { "type": "number" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::Search => (
                "Search for content across the knowledge base.",
                &["query"],
                json!({ "query": { "type": "string" } }),
            ),
            Self::Fetch => (
                "Fetch the full contents of a search result document.",
                &["id"],
                json!({ "id": { "type": "string" } }),
            ),
            Self::BuildContext => (
                "Build memory:// graph context.",
                &["url"],
                json!({
                    "url": { "type": "string" },
                    "depth": { "type": "integer" },
                    "timeframe": { "type": "string" },
                    "page": { "type": "integer" },
                    "page_size": { "type": "integer" },
                    "max_related": { "type": "integer" },
                    "output_format": { "type": "string", "enum": ["json", "text"] },
                }),
            ),
            Self::SchemaValidate => (
                "Validate notes against their Picoschema definitions.",
                &[],
                json!({
                    "note_type": { "type": "string" },
                    "identifier": { "type": "string" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::SchemaInfer => (
                "Analyze existing notes and suggest a Picoschema definition.",
                &["note_type"],
                json!({
                    "note_type": { "type": "string" },
                    "threshold": { "type": "number" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::SchemaDiff => (
                "Detect drift between a schema definition and actual note usage.",
                &["note_type"],
                json!({
                    "note_type": { "type": "string" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::BasicMemoryDiagnostics => {
                ("Report version, project, and index counts.", &[], json!({}))
            }
            Self::ListDirectory => (
                "List directory contents with filtering and depth control.",
                &[],
                json!({
                    "dir_name": { "type": "string" },
                    "depth": { "type": "integer" },
                    "file_name_glob": { "type": "string" },
                    "sort": {
                        "type": "string",
                        "enum": ["title_asc", "title_desc", "updated_asc", "updated_desc"],
                    },
                    "page": { "type": "integer" },
                    "page_size": { "type": "integer" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::ReadContent => (
                "Read a file's raw content by path or permalink.",
                &["path"],
                json!({ "path": { "type": "string" } }),
            ),
            Self::ViewNote => (
                "View a note as a formatted artifact for better readability.",
                &["identifier"],
                json!({ "identifier": { "type": "string" } }),
            ),
            Self::RecentActivity => (
                "Get recent activity for a project or across all projects.",
                &[],
                json!({
                    "type": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["entity", "relation", "observation"],
                        },
                    },
                    "depth": { "type": "integer" },
                    "timeframe": { "type": "string" },
                    "page": { "type": "integer" },
                    "page_size": { "type": "integer" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::ListMemoryProjects => (
                "List available projects with their status.",
                &[],
                json!({ "output_format": { "type": "string", "enum": ["text", "json"] } }),
            ),
            Self::CreateMemoryProject => (
                "Create a new Basic Memory project.",
                &["project_name", "project_path"],
                json!({
                    "project_name": { "type": "string" },
                    "project_path": { "type": "string" },
                    "set_default": { "type": "boolean" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::DeleteProject => (
                "Delete a Basic Memory project.",
                &["project_name"],
                json!({
                    "project_name": { "type": "string" },
                    "delete_notes": { "type": "boolean" },
                }),
            ),
        };
        tool(self.into(), description, required, properties)
    }
}

pub(crate) fn tool(
    name: &'static str,
    description: &str,
    required: &[&str],
    properties: Value,
) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        },
    })
}
