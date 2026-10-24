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
                    "expected_checksum": { "type": "string" },
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
                    "include_content": { "type": "boolean" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" },
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
                    "expected_checksum": { "type": "string" },
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
                    "projects": { "type": "array", "items": { "type": "string" } },
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
                    "valid_at": { "type": "string" },
                    "valid_overlaps": { "type": "array", "items": { "type": "string" } },
                    "time_kind": { "type": "string" },
                    "min_similarity": { "type": "number" },
                    "compact": { "type": "boolean" },
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
                    "compact": { "type": "boolean" },
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
            Self::AutoMemoryDiagnostics => {
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
                "Create a new Auto Memory project.",
                &["project_name", "project_path"],
                json!({
                    "project_name": { "type": "string" },
                    "project_path": { "type": "string" },
                    "set_default": { "type": "boolean" },
                    "output_format": { "type": "string", "enum": ["text", "json"] },
                }),
            ),
            Self::Cat => (
                "Print a note's body, or one section, line range, or budget of it.",
                &["path"],
                json!({
                    "path": { "type": "string" },
                    "section": { "type": "string" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" },
                    "max_chars": { "type": "integer" },
                }),
            ),
            Self::Grep => (
                "Find lines matching a pattern across notes.",
                &["pattern"],
                json!({
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "context_lines": { "type": "integer" },
                    "max_matches": { "type": "integer" },
                    "semantic": { "type": "boolean" },
                }),
            ),
            Self::Ls => (
                "List a directory. `/` lists the project mount points.",
                &[],
                json!({
                    "path": { "type": "string" },
                }),
            ),
            Self::Find => (
                "Find notes by metadata predicates such as `status=active`.",
                &[],
                json!({
                    "path": { "type": "string" },
                    "predicate": { "type": "array", "items": { "type": "string" } },
                    "fields": { "type": "array", "items": { "type": "string" } },
                }),
            ),
            Self::Tail => (
                "Print the last lines of recently updated notes.",
                &[],
                json!({
                    "path": { "type": "string" },
                    "count": { "type": "integer" },
                }),
            ),
            Self::Man => (
                "Print a manual page, or list the available ones.",
                &[],
                json!({
                    "topic": { "type": "string" },
                    "list": { "type": "boolean" },
                }),
            ),
            Self::DeleteProject => (
                "Delete an Auto Memory project.",
                &["project_name"],
                json!({
                    "project_name": { "type": "string" },
                    "delete_notes": { "type": "boolean" },
                }),
            ),
        };
        let mut definition = tool(self.into(), description, required, properties);
        definition["annotations"] = self.annotations();
        definition
    }

    /// MCP behavior hints for this tool.
    ///
    /// Upstream 0.24.0 (#1031, #1036, #1037): every tool carries `readOnlyHint`,
    /// `destructiveHint`, `idempotentHint` and `openWorldHint` so an agent can pick the
    /// right tool on demand instead of trying them to see what they do. The hints are
    /// derived from [`ToolName::is_mutating`] and a small table of the destructive and
    /// non-idempotent cases, so a tool cannot ship with hints that disagree with its
    /// read-only enforcement.
    pub(crate) fn annotations(self) -> Value {
        let read_only = !self.is_mutating();
        // Destructive: it can remove or replace what the user wrote. A `write_note`
        // create is not destructive, but `overwrite=True` is, so the hint is set on the
        // tool rather than the call.
        let destructive = matches!(
            self,
            Self::DeleteNote | Self::DeleteProject | Self::WriteNote | Self::EditNote
        );
        // Idempotent: repeating the same call reaches the same state.
        let idempotent = matches!(
            self,
            Self::ReadNote
                | Self::SearchNotes
                | Self::Search
                | Self::Fetch
                | Self::BuildContext
                | Self::ListDirectory
                | Self::ReadContent
                | Self::ViewNote
                | Self::RecentActivity
                | Self::ListMemoryProjects
                | Self::SchemaValidate
                | Self::SchemaInfer
                | Self::SchemaDiff
                | Self::AutoMemoryDiagnostics
                | Self::Cat
                | Self::Grep
                | Self::Ls
                | Self::Find
                | Self::Tail
                | Self::Man
                | Self::MoveNote
                | Self::DeleteNote
                | Self::DeleteProject
        );
        // Open world: the tool reaches beyond this vault. Only the ChatGPT `fetch`
        // resolves arbitrary ids, and `write_note` accepts free-form content, so both are
        // marked; everything else is closed over the local project.
        let open_world = matches!(self, Self::Fetch | Self::WriteNote | Self::EditNote);
        json!({
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "idempotentHint": idempotent,
            "openWorldHint": open_world,
        })
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
