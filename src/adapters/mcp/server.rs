//! Minimal MCP stdio server exposing the local core tools.
//!
//! Transport contract (`docs/mcp-spec.md` §1): newline-delimited JSON-RPC 2.0 on
//! **stdout**, logs and diagnostics on stderr. Implemented methods: `initialize`,
//! `notifications/initialized`, `ping`, `tools/list`, and `tools/call` for the note,
//! search, graph, and diagnostics tools this port supports.

use std::collections::BTreeMap;
use std::future::Future;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use super::helpers::*;

use strum::{Display, EnumIter, EnumString, IntoEnumIterator, IntoStaticStr};

use crate::application::activity::{
    ActivityOptions, recent_context, recent_rows, render_activity_text,
};
use crate::application::context::{ContextOptions, build_context};
use crate::application::directory::{
    DEFAULT_DIRECTORY_PAGE_SIZE, DirectoryOptions, DirectorySortOrder, list_directory,
    render_directory_text,
};
use crate::application::note::{NoteDocument, NoteService};
use crate::application::schema::SchemaService;
use crate::application::schema_tools;
use crate::application::search_text::{
    SearchType, no_criteria_guidance, render_search_markdown, search_failed_guidance,
    semantic_disabled_guidance,
};
use crate::domain::permalink::generate_permalink;
use crate::domain::search::SearchItemType;
use crate::domain::timeframe;
use crate::error::{Error, Result};
use crate::indexing::{IndexOptions, IndexService};
use crate::markdown::{EditOperation, EditOptions};
use crate::runtime::rerank::{
    DEFAULT_RERANKER_CANDIDATES, DEFAULT_RERANKER_MAX_DOCUMENT_CHARS, RerankProvider, RerankRequest,
};
use crate::search::embedding::EmbeddingProvider;
use crate::search::text::TextSearchOptions;
use crate::search::vector::{search_hybrid, search_vector};
use crate::storage::Store;

/// JSON-RPC protocol version reported in `initialize`.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
/// Server name reported in `initialize`.
pub const SERVER_NAME: &str = "auto-memory-rs";

/// The tools `tools/list` advertises and `tools/call` accepts.
///
/// The wire names are the compatibility contract (`docs/mcp-spec.md` §3–5); the
/// `strum` derives generate them from the variant names (`WriteNote` → `write_note`,
/// `AutoMemoryDiagnostics` → `auto_memory_diagnostics`), so the enum is the only
/// place a tool name is written down. `EnumIter` supplies the advertised order for
/// `tools/list` and the diagnostics report, `FromStr` parses a `tools/call` name, and
/// `call_tool` matches on the variant exhaustively — a tool that reaches one surface
/// but not the others is a compile error (or a test failure) instead of silent drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ToolName {
    /// `write_note` — create a markdown note in the vault.
    WriteNote,
    /// `read_note` — read a note by title or permalink.
    ReadNote,
    /// `edit_note` — append, prepend, find/replace, replace or insert a section.
    EditNote,
    /// `move_note` — move or rename a note (or a whole directory).
    MoveNote,
    /// `delete_note` — delete a note (or a whole directory) from vault and index.
    DeleteNote,
    /// `search_notes` — the FTS5/vector/hybrid search surface.
    SearchNotes,
    /// `search` — the OpenAI-client-only ChatGPT compatibility adapter.
    Search,
    /// `fetch` — the OpenAI-client-only ChatGPT compatibility adapter.
    Fetch,
    /// `build_context` — `memory://` graph context.
    BuildContext,
    /// `schema_validate` — validate notes against their Picoschema definitions.
    SchemaValidate,
    /// `schema_infer` — suggest a Picoschema definition from observed usage.
    SchemaInfer,
    /// `schema_diff` — detect drift between a definition and observed usage.
    SchemaDiff,
    /// `auto_memory_diagnostics` — version, project, and index report.
    AutoMemoryDiagnostics,
    /// `list_directory` — browse the vault directory tree.
    ListDirectory,
    /// `read_content` — read a file's raw content by path or permalink.
    ReadContent,
    /// `view_note` — render a note as a markdown artifact.
    ViewNote,
    /// `recent_activity` — recent rows for one project or across all projects.
    RecentActivity,
    /// `list_memory_projects` — list projects with their status.
    ListMemoryProjects,
    /// `create_memory_project` — create an Auto Memory project.
    CreateMemoryProject,
    /// `delete_project` — delete an Auto Memory project.
    DeleteProject,
}

/// How a tool renders its result (`output_format`).
///
/// The reference never rejects the value: an unknown one falls back to the tool's own
/// default (text everywhere, except `build_context`), so
/// [`OutputFormat::from_arguments`] parses leniently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, EnumString, IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum OutputFormat {
    /// The markdown/text surface (reference default).
    #[default]
    Text,
    /// The JSON payload.
    Json,
}

impl OutputFormat {
    /// The `output_format` argument of one tool call, falling back to `default` when the
    /// caller omits it or names a value the tool does not recognize.
    ///
    /// The default is per tool: every tool renders text unless asked for JSON, except
    /// `build_context`, whose payload is JSON unless asked for text.
    pub fn from_arguments(arguments: &Value, default: Self) -> Self {
        arguments["output_format"]
            .as_str()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    /// Whether the caller asked for the JSON payload.
    pub fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}

impl ToolName {
    /// Whether the tool can change the vault (hidden by `--read-only`).
    pub fn is_mutating(self) -> bool {
        matches!(
            self,
            Self::WriteNote
                | Self::EditNote
                | Self::MoveNote
                | Self::DeleteNote
                | Self::CreateMemoryProject
                | Self::DeleteProject
        )
    }
}

/// One MCP session bound to a project.
pub struct McpServer<'a> {
    store: &'a mut Store,
    project_id: i64,
    project_name: String,
    /// Project external id (UUID); the reference passes this to onboarding examples.
    project_external_id: String,
    permalink: String,
    vault: PathBuf,
    /// `clientInfo` reported by `initialize`, used by the ChatGPT-only adapters.
    client_info: Option<ClientInfo>,
    /// Cross-encoder reranker for the semantic search types, when one is configured.
    ///
    /// Stored as an `Arc` (rather than a `Box`) so one reranker can be shared by
    /// every HTTP session the Streamable HTTP transport spawns.
    reranker: Option<Arc<dyn RerankProvider + Send + Sync>>,
    /// How many top candidates the reranker rescores.
    reranker_candidates: usize,
    /// Per-candidate character cap for the reranker.
    reranker_max_document_chars: usize,
    /// When set, the mutating tools are neither advertised nor callable.
    read_only: bool,
    /// Embedding runtime for `search_type="vector"`/`"hybrid"`/`"semantic"`.
    ///
    /// `None` means semantic search is unavailable, which the tool reports as guidance
    /// rather than falling back to a text search. Shared across HTTP sessions for the
    /// same reason as [`Self::reranker`].
    provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
}

/// What one `search_notes` call turned into.
enum SearchOutcome {
    /// A search page, rendered as JSON or markdown by the caller.
    Payload(Value),
    /// Guidance text (no criteria, an unknown `search_type`, semantic search disabled).
    Guidance(String),
}

/// How `search_notes` should retrieve.
enum SearchMode {
    /// FTS5 text search.
    Text,
    /// Vector-only retrieval.
    Vector,
    /// Fused FTS + vector retrieval.
    Hybrid,
}

/// The `initialize` `clientInfo` fields that decide client identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ClientInfo {
    /// Reported client name.
    name: Option<String>,
    /// Reported client title.
    title: Option<String>,
}

impl ClientInfo {
    /// Parse `params.clientInfo`; `None` when the client reported nothing.
    fn from_initialize(params: &Value) -> Option<Self> {
        let info = params.get("clientInfo")?;
        Self::from_parts(
            info.get("name").and_then(Value::as_str).map(str::to_owned),
            info.get("title").and_then(Value::as_str).map(str::to_owned),
        )
    }

    /// Build from already-extracted name/title (the HTTP transport's typed
    /// `initialize` hands these over directly instead of a raw `Value`).
    ///
    /// Blank and whitespace-only values are dropped, matching [`Self::from_initialize`],
    /// and a pair with nothing left is reported as `None`.
    pub(crate) fn from_parts(name: Option<String>, title: Option<String>) -> Option<Self> {
        let clean = |value: Option<String>| -> Option<String> {
            let value = value?.trim().to_owned();
            (!value.is_empty()).then_some(value)
        };
        let info = Self {
            name: clean(name),
            title: clean(title),
        };
        (info.name.is_some() || info.title.is_some()).then_some(info)
    }

    /// The reported client name, for logging.
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Whether this is OpenAI's MCP client (`openai-mcp` or `openai-mcp/<version>`).
    fn is_openai_mcp(&self) -> bool {
        [&self.name, &self.title]
            .into_iter()
            .flatten()
            .any(|value| {
                let normalized = value.to_lowercase();
                normalized == "openai-mcp" || normalized.starts_with("openai-mcp/")
            })
    }
}

impl<'a> McpServer<'a> {
    /// Create a session for one indexed project.
    pub fn new(
        store: &'a mut Store,
        project_id: i64,
        project_name: impl Into<String>,
        project_external_id: impl Into<String>,
        permalink: impl Into<String>,
        vault: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            project_id,
            project_name: project_name.into(),
            project_external_id: project_external_id.into(),
            permalink: permalink.into(),
            vault: vault.into(),
            client_info: None,
            provider: None,
            reranker: None,
            reranker_candidates: DEFAULT_RERANKER_CANDIDATES,
            reranker_max_document_chars: DEFAULT_RERANKER_MAX_DOCUMENT_CHARS,
            read_only: false,
        }
    }

    /// Serve a read-only session: the mutating tools are hidden from `tools/list` and
    /// refused by `tools/call`.
    #[must_use]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// The tools this session advertises.
    fn tools(&self) -> Vec<Value> {
        advertised_tools(self.read_only)
    }

    /// Attach an embedding runtime, enabling the semantic `search_type`s.
    ///
    /// Accepts either the `Box<dyn ...>` the CLI builds or an `Arc<...>`; the HTTP
    /// transport shares one `Arc` across sessions.
    pub fn with_provider(
        mut self,
        provider: impl Into<Arc<dyn EmbeddingProvider + Send + Sync>>,
    ) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Attach a cross-encoder reranker for the semantic search types.
    pub fn with_reranker(
        mut self,
        reranker: impl Into<Arc<dyn RerankProvider + Send + Sync>>,
    ) -> Self {
        self.reranker = Some(reranker.into());
        self
    }

    /// Record the `clientInfo` the caller reported.
    ///
    /// The stdio transport learns it from the raw `initialize` frame inside
    /// [`Self::handle`]; the HTTP transport gets it from rmcp's typed request and
    /// hands the parsed value over here.
    pub(crate) fn with_client_info(mut self, client_info: Option<ClientInfo>) -> Self {
        self.client_info = client_info;
        self
    }

    /// Override the reranker's candidate window (reference `reranker_candidates`).
    pub fn with_reranker_candidates(mut self, candidates: usize) -> Self {
        self.reranker_candidates = candidates;
        self
    }

    /// Override the reranker's per-document character cap.
    pub fn with_reranker_max_document_chars(mut self, max_document_chars: usize) -> Self {
        self.reranker_max_document_chars = max_document_chars;
        self
    }

    /// Serve requests until the input stream ends.
    ///
    /// Only protocol frames are written to `output`; anything else goes to stderr,
    /// which keeps the transport clean for MCP clients. This is the blocking entry
    /// point (library callers and embedding); the CLI runs [`Self::serve_async`].
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> Result<()> {
        for line in input.lines() {
            let line = line?;
            if let Some(response) = self.handle_line(&line) {
                writeln!(output, "{response}")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// Serve requests until the input ends or `shutdown` resolves (the CLI path).
    ///
    /// Frames are read on the runtime instead of a blocked thread, and each tool call
    /// goes through [`tokio::task::block_in_place`]: the core is synchronous by design
    /// (SQLite, ONNX, file reads — `docs/auto-memory-rs-spec.md` §6), so it is moved
    /// off the reactor rather than made async, which is what lets this server share a
    /// runtime with other work. That requires a multi-thread runtime; see
    /// [`crate::runtime::executor`].
    ///
    /// `shutdown` is checked between frames, never mid-request: a call already in
    /// flight still answers, and no frame is written partially. Response order stays
    /// the request order — the tools serialize on one `Store` connection, so
    /// pipelining them onto the blocking pool would buy nothing but nondeterminism in
    /// a stdout contract that clients (and the golden captures) read as a sequence.
    pub async fn serve_async(
        &mut self,
        input: impl AsyncBufRead + Unpin,
        mut output: impl AsyncWrite + Unpin,
        shutdown: impl Future<Output = ()>,
    ) -> Result<()> {
        let mut lines = input.lines();
        tokio::pin!(shutdown);
        loop {
            let line = tokio::select! {
                line = lines.next_line() => line?,
                () = &mut shutdown => return Ok(()),
            };
            let Some(line) = line else {
                return Ok(());
            };
            let Some(response) = tokio::task::block_in_place(|| self.handle_line(&line)) else {
                continue;
            };
            output.write_all(response.to_string().as_bytes()).await?;
            output.write_all(b"\n").await?;
            output.flush().await?;
        }
    }

    /// Turn one transport line into a response frame.
    ///
    /// `None` covers the three frames that produce no response: a blank keep-alive
    /// line, a line that is not JSON (reported on stderr, never stdout), and a
    /// notification.
    fn handle_line(&mut self, line: &str) -> Option<Value> {
        if line.trim().is_empty() {
            return None;
        }
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("auto-memory mcp: ignoring malformed frame: {error}");
                return None;
            }
        };
        self.handle(&request)
    }

    /// Handle one JSON-RPC request; `None` for notifications.
    pub fn handle(&mut self, request: &Value) -> Option<Value> {
        let method = request["method"].as_str().unwrap_or_default();
        let id = request.get("id").cloned();
        // A notification (`notifications/initialized` and friends) carries no id and
        // expects no response.
        let id = id?;
        let result = match method {
            "initialize" => {
                // The ChatGPT adapters are gated on the client identity reported here.
                self.client_info = ClientInfo::from_initialize(&request["params"]);
                Ok(json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.tools() })),
            "tools/call" => self.call_tool(&request["params"]),
            other => Err(Error::InvalidArgument {
                message: format!("unsupported method: {other}"),
            }),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32603, "message": error.to_string() },
            }),
        })
    }

    /// Dispatch one `tools/call`. Exposed to the HTTP transport, which translates
    /// rmcp's typed request into this same `{name, arguments}` shape.
    pub(crate) fn call_tool(&mut self, params: &Value) -> Result<Value> {
        let name = params["name"].as_str().unwrap_or_default();
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let tool = name
            .parse::<ToolName>()
            .map_err(|_| Error::InvalidArgument {
                message: format!("unknown tool: {name}"),
            })?;
        if self.read_only && tool.is_mutating() {
            return Err(Error::InvalidArgument {
                message: format!("tool not available in read-only mode: {name}"),
            });
        }
        // Every branch returns a complete MCP tool result. Text surfaces (for
        // example `build_context(output_format="text")`) must not be re-encoded as
        // JSON, so wrapping happens once, here and in the helpers below. The match is
        // exhaustive over `ToolName`, so a new variant cannot reach `tools/list`
        // without a dispatch arm.
        Ok(match tool {
            ToolName::ReadNote => self.read_note(&arguments)?,
            ToolName::WriteNote => self.write_note(&arguments)?,
            ToolName::EditNote => self.edit_note(&arguments)?,
            ToolName::MoveNote => self.move_note(&arguments)?,
            ToolName::DeleteNote => self.delete_note(&arguments)?,
            ToolName::SearchNotes => self.search_notes(&arguments)?,
            ToolName::Search => self.chatgpt_search(&arguments)?,
            ToolName::Fetch => self.chatgpt_fetch(&arguments)?,
            ToolName::BuildContext => self.build_context(&arguments)?,
            ToolName::ListDirectory => self.list_directory(&arguments)?,
            ToolName::ReadContent => self.read_content(&arguments)?,
            ToolName::ViewNote => self.view_note(&arguments)?,
            ToolName::RecentActivity => self.recent_activity(&arguments)?,
            ToolName::ListMemoryProjects => self.list_memory_projects(&arguments)?,
            ToolName::CreateMemoryProject => self.create_memory_project(&arguments)?,
            ToolName::DeleteProject => self.delete_project(&arguments)?,
            ToolName::SchemaValidate => self.schema_validate(&arguments)?,
            ToolName::SchemaInfer => self.schema_infer(&arguments)?,
            ToolName::SchemaDiff => self.schema_diff(&arguments)?,
            ToolName::AutoMemoryDiagnostics => self.diagnostics()?,
        })
    }

    fn read_note(&mut self, arguments: &Value) -> Result<Value> {
        let identifier = required_str(arguments, "identifier")?;
        let identifier = identifier.to_owned();
        let output_format = OutputFormat::from_arguments(arguments, OutputFormat::Text);
        let include_frontmatter = arguments["include_frontmatter"].as_bool().unwrap_or(false);
        let page = arguments["page"]
            .as_u64()
            .or_else(|| arguments["page_number"].as_u64())
            .unwrap_or(1) as u32;
        let page_size = arguments["page_size"]
            .as_u64()
            .or_else(|| arguments["limit"].as_u64())
            .or_else(|| arguments["per_page"].as_u64())
            .unwrap_or(10) as u32;

        if !output_format.is_json() {
            return Ok(text_result(self.note_text(&identifier, page, page_size)?));
        }
        match self.read_note_source(&identifier)? {
            Some((document, raw)) => {
                let (body, frontmatter) = parse_opening_frontmatter(&raw);
                json_result(json!({
                    "title": document.title,
                    "permalink": document.permalink,
                    "file_path": document.file_path,
                    "content": if include_frontmatter { raw } else { body },
                    "frontmatter": frontmatter,
                }))
            }
            None => self.missing_note(&identifier, page, page_size),
        }
    }

    /// The reference's text surface: raw markdown, or not-found guidance.
    fn note_text(&mut self, identifier: &str, page: u32, page_size: u32) -> Result<String> {
        if let Some((_, raw)) = self.read_note_source(identifier)? {
            return Ok(raw);
        }
        if let Some(permalink) = self.find_by_exact_title(identifier)?
            && let Some((_, raw)) = self.read_note_source(&permalink)?
        {
            return Ok(raw);
        }
        let candidates = self.search_candidates(identifier, page, page_size)?;
        if candidates.is_empty() {
            return Ok(format_not_found_message(&self.project_name, identifier));
        }
        Ok(format_related_results(
            &self.project_name,
            identifier,
            &candidates,
        ))
    }

    /// Resolve and read one note, returning `None` instead of an error when absent.
    fn read_note_source(&mut self, identifier: &str) -> Result<Option<(NoteDocument, String)>> {
        let document = match self.notes().read_note(identifier) {
            Ok(document) => document,
            Err(error) => {
                // Only a miss falls through to the lookup chain; an I/O failure is real.
                return match error {
                    Error::InvalidArgument { .. } => Ok(None),
                    other => Err(other),
                };
            }
        };
        let raw = std::fs::read_to_string(self.vault.join(&document.file_path))?;
        Ok(Some((document, raw)))
    }

    /// The JSON miss payload: nulls, plus suggestion rows when the search found any.
    fn missing_note(&mut self, identifier: &str, page: u32, page_size: u32) -> Result<Value> {
        let candidates = self.search_candidates(identifier, page, page_size)?;
        let mut payload = json!({
            "title": Value::Null,
            "permalink": Value::Null,
            "file_path": Value::Null,
            "content": Value::Null,
            "frontmatter": Value::Null,
        });
        if !candidates.is_empty() {
            // The JSON surface names the fallback rows by title/permalink/file_path; the
            // `type` field only exists in the text surface's rendering.
            payload["related_results"] = Value::Array(
                candidates
                    .iter()
                    .map(|candidate| {
                        json!({
                            "title": candidate["title"],
                            "permalink": candidate["permalink"],
                            "file_path": candidate["file_path"],
                        })
                    })
                    .collect(),
            );
        }
        json_result(payload)
    }

    /// Walk title-search pages looking for a case-insensitive exact title match.
    fn find_by_exact_title(&mut self, identifier: &str) -> Result<Option<String>> {
        const LOOKUP_PAGE_SIZE: u32 = 10;
        const MAX_PAGES: u32 = 10;
        for page in 1..=MAX_PAGES {
            let found = self.store.search_text(
                self.project_id,
                &TextSearchOptions {
                    title: Some(identifier.to_owned()),
                    page,
                    page_size: LOOKUP_PAGE_SIZE,
                    ..TextSearchOptions::default()
                },
            )?;
            if let Some(row) = found
                .results
                .iter()
                .find(|row| row.title.trim().to_lowercase() == identifier.trim().to_lowercase())
            {
                return Ok(row.permalink.clone());
            }
            if !found.has_more {
                break;
            }
        }
        Ok(None)
    }

    /// Text-search suggestions, shaped for `format_related_results`.
    fn search_candidates(
        &mut self,
        identifier: &str,
        page: u32,
        page_size: u32,
    ) -> Result<Vec<Value>> {
        let found = self.store.search_text(
            self.project_id,
            &TextSearchOptions {
                query: Some(identifier.to_owned()),
                page,
                page_size,
                ..TextSearchOptions::default()
            },
        )?;
        Ok(found
            .results
            .iter()
            .map(|row| {
                json!({
                    "title": row.title,
                    "type": <&str>::from(row.item_type),
                    "permalink": row.permalink,
                    "file_path": row.file_path,
                })
            })
            .collect())
    }

    fn write_note(&mut self, arguments: &Value) -> Result<Value> {
        let title = required_str(arguments, "title")?.to_owned();
        let content = arguments["content"].as_str().unwrap_or_default().to_owned();
        let mut directory = arguments["directory"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        // `"/"` means the project root, and it is normalized before the guard runs.
        if directory == "/" {
            directory.clear();
        }
        let overwrite = arguments["overwrite"].as_bool().unwrap_or(false);
        let output_format = OutputFormat::from_arguments(arguments, OutputFormat::Text);

        // The reference refuses a directory that could leave the project, and answers with
        // a structured payload rather than an error.
        if !directory.is_empty() && !is_valid_project_directory(&directory) {
            if output_format.is_json() {
                return json_result(json!({
                    "title": title,
                    "permalink": Value::Null,
                    "file_path": Value::Null,
                    "checksum": Value::Null,
                    "action": "created",
                    "error": "SECURITY_VALIDATION_ERROR",
                }));
            }
            return Ok(text_result(format!(
                "# Error\n\nDirectory path '{directory}' is not allowed - paths must stay within \
                 project boundaries"
            )));
        }

        // `note_type` supplies the frontmatter `type` unless the content already
        // declares one; explicit `tags` win over `metadata["tags"]` — both mirroring the
        // reference's merge order.
        let note_type = arguments["note_type"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("note")
            .to_owned();
        let note_type = (!content_declares_type(&content)).then_some(note_type);
        let mut metadata = metadata_pairs(&arguments["metadata"])?;
        let tags = crate::markdown::frontmatter::parse_tags(arguments.get("tags"));
        if !tags.is_empty() {
            metadata.push((
                "tags".to_owned(),
                serde_yaml_ng::Value::Sequence(
                    tags.iter()
                        .cloned()
                        .map(serde_yaml_ng::Value::String)
                        .collect(),
                ),
            ));
        }

        // Notes are markdown files; the reference appends the extension when the
        // caller gives a bare title.
        let file_name = if title.to_ascii_lowercase().ends_with(".md") {
            title.clone()
        } else {
            format!("{title}.md")
        };
        let file_path = if directory.is_empty() {
            file_name
        } else {
            format!("{}/{file_name}", directory.trim_matches('/'))
        };

        // The reference writes optimistically and blocks a conflict unless `overwrite`
        // (or the configured default) allows it; the refusal is a structured payload,
        // not a transport error.
        let existed = self.vault.join(&file_path).is_file()
            || self
                .store
                .entity_by_file_path(self.project_id, &file_path)?
                .is_some();
        let generated_permalink = generate_permalink(&title);
        if existed && !overwrite {
            if output_format.is_json() {
                return json_result(json!({
                    "title": title,
                    "permalink": generated_permalink,
                    "file_path": Value::Null,
                    "checksum": Value::Null,
                    "action": "conflict",
                    "error": "NOTE_ALREADY_EXISTS",
                }));
            }
            return Ok(text_result(overwrite_error(
                &title,
                &generated_permalink,
                &self.project_name,
            )));
        }

        let written = {
            let mut notes = self.notes();
            notes.write_note_with_type(
                &file_path,
                &content,
                &metadata,
                note_type.as_deref(),
                overwrite,
            )?
        };
        let action = if existed { "updated" } else { "created" };
        let entity = self
            .store
            .entity_by_file_path(self.project_id, &written.file_path)?;

        if output_format.is_json() {
            return json_result(json!({
                "title": written.title,
                "permalink": written.permalink,
                "file_path": written.file_path,
                // The reference's MCP write path never carries a checksum back.
                "checksum": Value::Null,
                "action": action,
            }));
        }

        let mut summary = vec![
            format!("# {} note", capitalize(action)),
            format!("project: {}", self.project_name),
            format!("file_path: {}", written.file_path),
            format!(
                "permalink: {}",
                written.permalink.clone().unwrap_or_default()
            ),
            "checksum: unknown".to_owned(),
        ];
        if let Some(entity) = &entity {
            let observations = self.store.observations_for_entity(entity.id)?;
            if !observations.is_empty() {
                let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
                for observation in &observations {
                    *counts.entry(observation.category.as_str()).or_default() += 1;
                }
                summary.push(String::new());
                summary.push("## Observations".to_owned());
                for (category, count) in counts {
                    summary.push(format!("- {category}: {count}"));
                }
            }
            let relations: Vec<crate::storage::RelationRow> = self
                .store
                .relations(self.project_id)?
                .into_iter()
                .filter(|relation| relation.from_id == entity.id)
                .collect();
            if !relations.is_empty() {
                let unresolved = relations
                    .iter()
                    .filter(|relation| relation.to_id.is_none())
                    .count();
                let resolved = relations.len() - unresolved;
                summary.push(String::new());
                summary.push("## Relations".to_owned());
                summary.push(format!("- Resolved: {resolved}"));
                if unresolved > 0 {
                    summary.push(format!("- Unresolved: {unresolved}"));
                    summary.push(String::new());
                    summary.push(
                        "Note: Unresolved relations point to entities that don't exist yet."
                            .to_owned(),
                    );
                    summary.push(
                        "They will be automatically resolved when target entities are created \
                         or during sync operations."
                            .to_owned(),
                    );
                }
            }
        }
        if !tags.is_empty() {
            summary.push(String::new());
            summary.push("## Tags".to_owned());
            summary.push(format!("- {}", tags.join(", ")));
        }
        Ok(text_result(format!(
            "{}\n\n[Session: Using project '{}']",
            summary.join("\n"),
            self.project_name
        )))
    }

    fn edit_note(&mut self, arguments: &Value) -> Result<Value> {
        let identifier = required_str(arguments, "identifier")?.to_owned();
        let operation = EditOperation::parse(required_str(arguments, "operation")?)?;
        let content = arguments["content"].as_str().unwrap_or_default().to_owned();
        let mut options = EditOptions::new();
        options.section = arguments["section"].as_str().map(str::to_owned);
        options.find_text = arguments["find_text"].as_str().map(str::to_owned);
        if let Some(expected) = arguments["expected_replacements"].as_u64() {
            options.expected_replacements = expected as usize;
        }
        if let Some(replace) = arguments["replace_subsections"].as_bool() {
            options.replace_subsections = replace;
        }
        let metadata = metadata_pairs(&arguments["metadata"])?;
        let output_format = OutputFormat::from_arguments(arguments, OutputFormat::Text);
        let (document, file_created) = {
            let mut notes = self.notes();
            notes.edit_note_with_status(&identifier, operation, &content, &options, &metadata)?
        };

        if output_format.is_json() {
            return json_result(json!({
                "title": document.title,
                "permalink": document.permalink,
                "file_path": document.file_path,
                "checksum": Value::Null,
                "operation": <&str>::from(operation),
                "fileCreated": file_created,
            }));
        }

        let op = <&str>::from(operation);
        let mut summary = if file_created {
            vec![
                format!("# Created note ({op})"),
                format!("project: {}", self.project_name),
                format!("file_path: {}", document.file_path),
                format!(
                    "permalink: {}",
                    document.permalink.clone().unwrap_or_default()
                ),
                "checksum: unknown".to_owned(),
                "fileCreated: true".to_owned(),
                format!(
                    "operation: Created note with {} lines",
                    content.split('\n').count()
                ),
            ]
        } else {
            let mut summary = vec![
                format!("# Edited note ({op})"),
                format!("project: {}", self.project_name),
                format!("file_path: {}", document.file_path),
                format!(
                    "permalink: {}",
                    document.permalink.clone().unwrap_or_default()
                ),
                "checksum: unknown".to_owned(),
            ];
            let lines_added = content.split('\n').count();
            let detail = match operation {
                EditOperation::Append => {
                    format!("operation: Added {lines_added} lines to end of note")
                }
                EditOperation::Prepend => {
                    format!("operation: Added {lines_added} lines to beginning of note")
                }
                EditOperation::FindReplace => {
                    "operation: Find and replace operation completed".to_owned()
                }
                EditOperation::ReplaceSection => format!(
                    "operation: Replaced content under section '{}'",
                    options.section.as_deref().unwrap_or_default()
                ),
                EditOperation::InsertBeforeSection => format!(
                    "operation: Inserted content before section '{}'",
                    options.section.as_deref().unwrap_or_default()
                ),
                EditOperation::InsertAfterSection => format!(
                    "operation: Inserted content after section '{}'",
                    options.section.as_deref().unwrap_or_default()
                ),
            };
            summary.push(detail);
            summary
        };
        // The reference's edit response carries no observation/relation collections, so
        // its conditional `## Observations` / `## Relations` sections never render here.
        let _ = &mut summary;
        Ok(text_result(format!(
            "{}\n\n[Session: Using project '{}']",
            summary.join("\n"),
            self.project_name
        )))
    }

    fn move_note(&mut self, arguments: &Value) -> Result<Value> {
        let identifier = required_str(arguments, "identifier")?.to_owned();
        let is_directory = arguments["is_directory"].as_bool().unwrap_or(false);
        let destination_path = arguments["destination_path"].as_str();
        let destination_folder = arguments["destination_folder"].as_str();
        let output_format = OutputFormat::from_arguments(arguments, OutputFormat::Text);

        if is_directory {
            // A directory move needs a full destination path: a folder name has no
            // filename to keep, so the reference refuses it with a specific code.
            let Some(destination) = destination_path else {
                if destination_folder.is_some() {
                    return move_result(
                        output_format,
                        json!({
                            "moved": false,
                            "title": Value::Null,
                            "permalink": Value::Null,
                            "file_path": Value::Null,
                            "source": identifier,
                            "destination": Value::Null,
                            "error": "DESTINATION_FOLDER_NOT_FOR_DIRECTORIES",
                        }),
                        &self.project_name,
                    );
                }
                return Err(Error::InvalidArgument {
                    message: "move_note requires destination_path or destination_folder".to_owned(),
                });
            };
            return self.move_directory(&identifier, destination, output_format);
        }

        let Some(entity) =
            crate::graph::resolve_entity_path(self.store, self.project_id, &identifier)?
        else {
            return move_result(
                output_format,
                json!({
                    "moved": false,
                    "title": Value::Null,
                    "permalink": Value::Null,
                    "file_path": Value::Null,
                    "source": identifier,
                    "destination": destination_path.or(destination_folder),
                    "error": format!("Entity not found: '{identifier}'"),
                }),
                &self.project_name,
            );
        };

        // `destination_folder` keeps the note's filename; `destination_path` is exact.
        let destination = match (destination_path, destination_folder) {
            (Some(path), _) => path.to_owned(),
            (None, Some(folder)) => {
                let name = entity
                    .file_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&entity.file_path);
                format!("{}/{name}", folder.trim_matches('/'))
            }
            (None, None) => {
                return Err(Error::InvalidArgument {
                    message: "move_note requires destination_path or destination_folder".to_owned(),
                });
            }
        };
        if destination == entity.file_path {
            return move_result(
                output_format,
                json!({
                    "moved": false,
                    "title": entity.title,
                    "permalink": entity.permalink,
                    "file_path": entity.file_path,
                    "source": identifier,
                    "destination": destination,
                    "error": "DESTINATION_SAME_AS_SOURCE",
                }),
                &self.project_name,
            );
        }

        let moved = {
            let mut notes = self.notes();
            notes.move_note(&identifier, &destination)?
        };
        move_result(
            output_format,
            json!({
                "moved": true,
                "title": moved.title,
                "permalink": moved.permalink,
                "file_path": moved.file_path,
                "source": identifier,
                "destination": destination,
            }),
            &self.project_name,
        )
    }

    /// Move every note under a directory prefix to a new prefix.
    fn move_directory(
        &mut self,
        identifier: &str,
        destination: &str,
        output_format: OutputFormat,
    ) -> Result<Value> {
        let prefix = identifier.trim_matches('/').trim_end_matches('/');
        let prefix_with_slash = format!("{prefix}/");
        let destination_prefix = destination.trim_matches('/').trim_end_matches('/');
        let mut files: Vec<String> = crate::indexing::document::markdown_files(&self.vault)
            .into_iter()
            .map(|(relative, _)| relative)
            .filter(|path| path.starts_with(&prefix_with_slash))
            .collect();
        files.sort();

        let mut moved: Vec<String> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for file in &files {
            let suffix = &file[prefix_with_slash.len()..];
            let target = format!("{destination_prefix}/{suffix}");
            let mut notes = self.notes();
            match notes.move_note(file, &target) {
                Ok(_) => moved.push(target),
                Err(_) => failed.push(file.clone()),
            }
        }

        // The reference only carries `error` when nothing matched, so the key is absent
        // rather than null on success.
        let mut payload = json!({
            "moved": !files.is_empty() && failed.is_empty(),
            "title": Value::Null,
            "permalink": Value::Null,
            "file_path": Value::Null,
            "source": identifier,
            "destination": destination,
            "is_directory": true,
            "total_files": files.len(),
            "successful_moves": moved.len(),
            "failed_moves": failed.len(),
        });
        if files.is_empty() {
            payload["error"] = json!("Directory not found or empty: no files matched");
        }
        if output_format.is_json() {
            return json_result(payload);
        }

        if files.is_empty() {
            return Ok(text_result(format!(
                "# Directory Move Failed - No Files Found\n\n\
                 No files found for source directory `{identifier}`.\nTotal files: 0.\n\n\
                 <!-- Project: {} -->",
                self.project_name
            )));
        }
        let mut lines = vec![
            "# Directory Moved Successfully".to_owned(),
            String::new(),
            format!("**Source:** `{identifier}`"),
            format!("**Destination:** `{destination}`"),
            String::new(),
            "## Summary".to_owned(),
            format!("- Total files: {}", files.len()),
            format!("- Successfully moved: {}", moved.len()),
            format!("- Failed: {}", failed.len()),
        ];
        if !moved.is_empty() {
            lines.push(String::new());
            lines.push("## Moved Files".to_owned());
            for file in moved.iter().take(10) {
                lines.push(format!("- `{file}`"));
            }
            if moved.len() > 10 {
                lines.push(format!("- ... and {} more", moved.len() - 10));
            }
        }
        if !failed.is_empty() {
            lines.push(String::new());
            lines.push("## Errors".to_owned());
            for file in failed.iter().take(5) {
                lines.push(format!("- `{file}`"));
            }
        }
        lines.push(String::new());
        lines.push(format!("<!-- Project: {} -->", self.project_name));
        Ok(text_result(lines.join("\n")))
    }

    fn delete_note(&mut self, arguments: &Value) -> Result<Value> {
        let identifier = required_str(arguments, "identifier")?.to_owned();
        let is_directory = arguments["is_directory"].as_bool().unwrap_or(false);
        let output_format = OutputFormat::from_arguments(arguments, OutputFormat::Text);
        if is_directory {
            return self.delete_directory(&identifier, output_format);
        }

        // Resolve first so the result can name what was removed.
        let entity = crate::graph::resolve_entity_path(self.store, self.project_id, &identifier)?;
        let Some(entity) = entity else {
            return if output_format.is_json() {
                json_result(json!({
                    "deleted": false,
                    "title": Value::Null,
                    "permalink": Value::Null,
                    "file_path": Value::Null,
                }))
            } else {
                Ok(text_result("false"))
            };
        };
        {
            let mut notes = self.notes();
            notes.delete_note(&entity.file_path)?;
        }
        if output_format.is_json() {
            return json_result(json!({
                "deleted": true,
                "title": entity.title,
                "permalink": entity.permalink,
                "file_path": entity.file_path,
            }));
        }
        Ok(text_result("true"))
    }

    /// Delete every note under a directory path.
    ///
    /// The directory itself is left in place (the reference removes the files, not the
    /// folder), and the summary is the captured markdown block.
    fn delete_directory(&mut self, identifier: &str, output_format: OutputFormat) -> Result<Value> {
        let prefix = identifier
            .trim_matches('/')
            .trim_end_matches('/')
            .to_owned();
        let prefix_with_slash = format!("{prefix}/");
        let mut files: Vec<String> = crate::indexing::document::markdown_files(&self.vault)
            .into_iter()
            .map(|(relative, _)| relative)
            .filter(|path| path.starts_with(&prefix_with_slash))
            .collect();
        files.sort();

        let mut deleted: Vec<String> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for file in &files {
            let mut notes = self.notes();
            match notes.delete_note(file) {
                Ok(_) => deleted.push(file.clone()),
                Err(_) => failed.push(file.clone()),
            }
        }

        if output_format.is_json() {
            let mut payload = json!({
                "deleted": !deleted.is_empty() && failed.is_empty(),
                "is_directory": true,
                "identifier": identifier,
                "total_files": files.len(),
                "successful_deletes": deleted.len(),
                "failed_deletes": failed.len(),
                "deleted_files": deleted,
                "errors": failed,
            });
            if files.is_empty() {
                payload["error"] = json!("Directory not found or empty: no files matched");
            }
            return json_result(payload);
        }

        if files.is_empty() {
            return Ok(text_result(format!(
                "# Directory Delete Failed - No Files Found\n\n\
                 No files found for directory `{identifier}`.\n\n\
                 <!-- Project: {} -->",
                self.project_name
            )));
        }
        let mut lines = vec![
            "# Directory Deleted Successfully".to_owned(),
            String::new(),
            format!("**Directory:** `{identifier}`"),
            String::new(),
            "## Summary".to_owned(),
            format!("- Total files: {}", files.len()),
            format!("- Successfully deleted: {}", deleted.len()),
            format!("- Failed: {}", failed.len()),
        ];
        if !deleted.is_empty() {
            lines.push(String::new());
            lines.push("## Deleted Files".to_owned());
            for file in deleted.iter().take(10) {
                lines.push(format!("- `{file}`"));
            }
            if deleted.len() > 10 {
                lines.push(format!("- ... and {} more", deleted.len() - 10));
            }
        }
        if !failed.is_empty() {
            lines.push(String::new());
            lines.push("## Errors".to_owned());
            for file in failed.iter().take(5) {
                lines.push(format!("- `{file}`"));
            }
            if failed.len() > 5 {
                lines.push(format!("- ... and {} more errors", failed.len() - 5));
            }
        }
        lines.push(String::new());
        lines.push(format!("<!-- Project: {} -->", self.project_name));
        Ok(text_result(lines.join("\n")))
    }

    fn search_notes(&mut self, arguments: &Value) -> Result<Value> {
        let query = arguments["query"].as_str().map(str::to_owned);
        let output_format = OutputFormat::from_arguments(arguments, OutputFormat::Text);
        if arguments["search_all_projects"].as_bool().unwrap_or(false) {
            return self.search_all_projects(arguments, output_format);
        }
        match self.search_outcome(arguments)? {
            SearchOutcome::Payload(payload) => {
                if output_format.is_json() {
                    return json_result(payload);
                }
                Ok(text_result(render_search_markdown(
                    &payload,
                    &self.project_name,
                    query.as_deref(),
                    Some(&self.project_external_id),
                )))
            }
            // Guidance is returned as text whatever `output_format` asked for: the
            // reference only consults it on the success path.
            SearchOutcome::Guidance(text) => Ok(text_result(text)),
        }
    }

    /// Classify one `search_notes` request.
    ///
    /// Mirrors the reference tool: `search_type` maps the query onto a field (text /
    /// title / permalink) or onto a retrieval mode (vector / semantic / hybrid); an
    /// unknown type and a request with no criteria both produce guidance rather than a
    /// payload; and the semantic modes need an embedding runtime, so without one they
    /// say so instead of quietly running a text search.
    fn search_outcome(&mut self, arguments: &Value) -> Result<SearchOutcome> {
        self.search_outcome_for(self.project_id, arguments)
    }

    /// Search every registered project and merge the pages.
    ///
    /// Ports `_search_all_projects`: each project runs the same request with
    /// `page=1, page_size = page * page_size`, the totals add up, `total_is_exact` is the
    /// AND of the parts and `has_more` the OR, and the merged rows are sorted by score
    /// descending before the requested page is sliced out. The text surface labels the
    /// page `all projects`.
    fn search_all_projects(
        &mut self,
        arguments: &Value,
        output_format: OutputFormat,
    ) -> Result<Value> {
        let page = arguments["page"].as_u64().unwrap_or(1).max(1) as u32;
        let page_size = arguments["page_size"].as_u64().unwrap_or(10).max(1) as u32;
        let per_project_page_size = page * page_size;
        let projects = self.store.projects()?;

        let mut merged: Vec<Value> = Vec::new();
        let mut total = 0usize;
        let mut total_is_exact = true;
        let mut any_project_has_more = false;
        for project in &projects {
            let mut request = arguments.clone();
            if let Some(map) = request.as_object_mut() {
                map.insert("search_all_projects".to_owned(), json!(false));
                map.insert("page".to_owned(), json!(1));
                map.insert("page_size".to_owned(), json!(per_project_page_size));
                // Each project answers with its JSON payload; the merged page is the
                // one that gets rendered in the caller's format.
                map.insert(
                    "output_format".to_owned(),
                    json!(<&str>::from(OutputFormat::Json)),
                );
            }
            let payload = match self.search_outcome_for(project.id, &request)? {
                SearchOutcome::Payload(payload) => payload,
                // A project that answered with guidance contributes nothing, and the
                // merged total can no longer claim to be exact.
                SearchOutcome::Guidance(_) => {
                    total_is_exact = false;
                    continue;
                }
            };
            let rows = payload["results"].as_array().cloned().unwrap_or_default();
            let reported = payload["total"].as_u64().unwrap_or(0) as usize;
            total += if reported > 0 {
                reported
            } else {
                rows.len() + usize::from(payload["has_more"] == json!(true))
            };
            total_is_exact &= payload["total_is_exact"] == json!(true);
            any_project_has_more |= payload["has_more"] == json!(true);
            merged.extend(rows);
        }

        // Stable descending sort: ties keep project order, then result order.
        merged.sort_by(|left, right| {
            score_of(right)
                .partial_cmp(&score_of(left))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let start = ((page - 1) * page_size) as usize;
        let end = start + page_size as usize;
        let paged_results: Vec<Value> = merged
            .iter()
            .skip(start)
            .take(page_size as usize)
            .cloned()
            .collect();
        let payload = json!({
            "results": paged_results,
            "current_page": page,
            "page_size": page_size,
            "total": total,
            "total_is_exact": total_is_exact,
            "has_more": any_project_has_more || total > end || merged.len() > end,
        });
        if output_format.is_json() {
            return json_result(payload);
        }
        Ok(text_result(render_search_markdown(
            &payload,
            "all projects",
            arguments["query"].as_str(),
            None,
        )))
    }

    /// Classify one `search_notes` request against a specific project.
    ///
    /// Split out so `search_all_projects` can run the same logic per project.
    fn search_outcome_for(&mut self, project_id: i64, arguments: &Value) -> Result<SearchOutcome> {
        let query = arguments["query"].as_str().map(str::to_owned);
        let effective_query = query
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_owned();
        let raw_search_type = arguments["search_type"]
            .as_str()
            .unwrap_or("text")
            .to_owned();

        let supplied_entity_types = strings(&arguments["entity_types"]);
        let mut options = self.search_options(arguments)?;
        let mut mode = SearchMode::Text;

        // The mode only applies when there is an actual query string; a filter-only
        // request ignores `search_type` entirely, which is why the reference never
        // validates it in that case either.
        if !effective_query.is_empty() {
            let search_type = match raw_search_type.parse::<SearchType>() {
                Ok(search_type) => search_type,
                Err(_) => {
                    return Ok(SearchOutcome::Guidance(search_failed_guidance(
                        &self.project_name,
                        &effective_query,
                        &format!(
                            "Invalid search_type '{raw_search_type}'. Valid options: {}",
                            SearchType::valid_options()
                        ),
                    )));
                }
            };
            match search_type {
                SearchType::Text => options.query = Some(effective_query.clone()),
                SearchType::Vector | SearchType::Semantic => {
                    options.query = Some(effective_query.clone());
                    mode = SearchMode::Vector;
                }
                SearchType::Hybrid => {
                    options.query = Some(effective_query.clone());
                    mode = SearchMode::Hybrid;
                }
                // The reference maps the query onto *one* field: a `title`/`permalink`
                // search leaves `text` empty, so this port does the same. (The two legs
                // are combinable — `search::text` folds several MATCH predicates into
                // one table-level MATCH — but `search_type` keeps the reference's
                // one-field mapping, which the captures pin.)
                SearchType::Title => {
                    options.query = None;
                    options.title = Some(effective_query.clone());
                }
                SearchType::Permalink => {
                    options.query = None;
                    if effective_query.contains('*') {
                        options.permalink_match = Some(effective_query.clone());
                    } else {
                        options.permalink = Some(effective_query.clone());
                    }
                }
            }
        }

        if !has_search_criteria(&options, &supplied_entity_types) {
            return Ok(SearchOutcome::Guidance(no_criteria_guidance().to_owned()));
        }

        let payload = match mode {
            SearchMode::Text => page_payload(&self.store.search_text(project_id, &options)?),
            SearchMode::Vector | SearchMode::Hybrid => {
                let Some(provider) = self.provider.as_ref() else {
                    return Ok(SearchOutcome::Guidance(semantic_disabled_guidance(
                        &self.project_name,
                        &effective_query,
                        &raw_search_type,
                    )));
                };
                let query_vector = provider.embed_query(&effective_query)?;
                let rerank_request = self.reranker.as_ref().map(|reranker| RerankRequest {
                    query: &effective_query,
                    provider: reranker.as_ref(),
                    candidates: self.reranker_candidates,
                    max_document_chars: self.reranker_max_document_chars,
                });
                let mut vector_options = vector_options(&options);
                // A per-query `min_similarity` overrides the configured default.
                if let Some(min_similarity) = arguments["min_similarity"].as_f64() {
                    vector_options.min_similarity = min_similarity as f32;
                }
                let page = if matches!(mode, SearchMode::Hybrid) {
                    search_hybrid(
                        self.store,
                        project_id,
                        &effective_query,
                        &query_vector,
                        provider.model_name(),
                        &vector_options,
                        rerank_request.as_ref(),
                    )?
                } else {
                    search_vector(
                        self.store,
                        project_id,
                        &query_vector,
                        provider.model_name(),
                        &vector_options,
                        rerank_request.as_ref(),
                    )?
                };
                page_payload(&page)
            }
        };
        Ok(SearchOutcome::Payload(payload))
    }

    /// The `search_notes` JSON payload, shared with the ChatGPT `search` adapter.
    fn search_payload(&mut self, arguments: &Value) -> Result<Value> {
        match self.search_outcome(arguments)? {
            SearchOutcome::Payload(payload) => Ok(payload),
            SearchOutcome::Guidance(_) => Ok(json!({
                "results": [],
                "error": "No search criteria",
            })),
        }
    }

    /// Build the filter options one request asks for.
    fn search_options(&self, arguments: &Value) -> Result<TextSearchOptions> {
        let categories = strings(&arguments["categories"]);
        let mut options = TextSearchOptions {
            query: arguments["query"].as_str().map(str::to_owned),
            page: arguments["page"].as_u64().unwrap_or(1) as u32,
            page_size: arguments["page_size"].as_u64().unwrap_or(10) as u32,
            categories: categories.clone(),
            ..TextSearchOptions::default()
        };
        if let Some(title) = arguments["title"].as_str() {
            options.title = Some(title.to_owned());
        }
        if let Some(permalink) = arguments["permalink"].as_str() {
            options.permalink = Some(permalink.to_owned());
        }
        if let Some(permalink_match) = arguments["permalink_match"].as_str() {
            options.permalink_match = Some(permalink_match.to_owned());
        }
        for value in strings(&arguments["note_types"]) {
            options.note_types.push(value);
        }
        for value in strings(&arguments["tags"]) {
            options.tags.push(value);
        }
        // The reference's implicit default: a category filter scopes the search to
        // observation rows, because categories only exist there.
        let entity_types = strings(&arguments["entity_types"]);
        options.entity_types = if entity_types.is_empty() {
            crate::search::default_entity_types(&options.categories)
        } else {
            entity_types
                .iter()
                .filter_map(|value| value.parse::<SearchItemType>().ok())
                .collect()
        };
        if let Some(status) = arguments["status"].as_str() {
            options.status = Some(status.to_owned());
        }
        if let Some(after) =
            string_argument(arguments, &["after_date", "since", "after", "from_date"])
        {
            // An unparsable bound means "no date filter", as in the reference.
            options.after_date = crate::domain::dateparser::parse_after_date(after);
        }
        if let Some(filters) = arguments["metadata_filters"].as_object() {
            for (key, value) in filters {
                let key = if key == "note_type" { "type" } else { key };
                let value = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                options.metadata_filters.insert(key.to_owned(), value);
            }
        }
        Ok(options)
    }

    fn build_context(&mut self, arguments: &Value) -> Result<Value> {
        let url = required_str(arguments, "url")?.to_owned();
        let timeframe = arguments["timeframe"].as_str().unwrap_or("7d");
        // `build_context` is the one tool that answers with its JSON payload by
        // default; `output_format="text"` opts into the markdown artifact.
        let output_format = OutputFormat::from_arguments(arguments, OutputFormat::Json);
        let options = ContextOptions {
            depth: arguments["depth"].as_u64().unwrap_or(1) as u32,
            max_related: arguments["max_related"].as_u64().unwrap_or(10) as u32,
            page: arguments["page"].as_u64().unwrap_or(1) as u32,
            page_size: arguments["page_size"].as_u64().unwrap_or(10) as u32,
            since: Some(timeframe::parse_timeframe(timeframe)?),
        };
        let graph = build_context(self.store, self.project_id, &url, &options)?;
        if output_format == OutputFormat::Text {
            let response = serde_json::to_value(&graph)?;
            return Ok(text_result(crate::application::context::render_markdown(
                &response,
                &self.project_name,
            )));
        }
        json_result(serde_json::to_value(graph)?)
    }

    fn list_directory(&mut self, arguments: &Value) -> Result<Value> {
        let dir_name = string_argument(
            arguments,
            &["dir_name", "directory", "folder", "path", "dir"],
        )
        .unwrap_or("/")
        .to_owned();
        let page_size = arguments["page_size"]
            .as_u64()
            .or_else(|| arguments["limit"].as_u64())
            .or_else(|| arguments["per_page"].as_u64())
            .unwrap_or(DEFAULT_DIRECTORY_PAGE_SIZE as u64) as u32;
        let sort = string_argument(arguments, &["sort"])
            .map(DirectorySortOrder::parse)
            .transpose()?;
        let options = DirectoryOptions {
            dir_name: dir_name.clone(),
            depth: arguments["depth"].as_u64().unwrap_or(1) as u32,
            file_name_glob: string_argument(
                arguments,
                &["file_name_glob", "glob", "pattern", "filter"],
            )
            .map(str::to_owned),
            sort,
            page: arguments["page"].as_u64().unwrap_or(1) as u32,
            page_size,
        };
        let listing = list_directory(self.store, self.project_id, &options)?;
        if OutputFormat::from_arguments(arguments, OutputFormat::Text).is_json() {
            return json_result(serde_json::to_value(&listing)?);
        }
        Ok(text_result(render_directory_text(
            &listing, &options, &dir_name,
        )))
    }

    fn read_content(&mut self, arguments: &Value) -> Result<Value> {
        let raw_path = string_argument(arguments, &["path", "file_path", "filepath", "file"])
            .ok_or_else(|| Error::InvalidArgument {
                message: "path is required".to_owned(),
            })?
            .to_owned();
        let relative = self.notes().resolve(&raw_path)?;
        let bytes = std::fs::read(self.vault.join(&relative))?;
        let content_type = guess_content_type(&relative);

        if content_type.starts_with("text/") || content_type == "application/json" {
            // Text responses carry the transport's charset suffix, so the media type
            // is `text/markdown; charset=utf-8` rather than a bare `text/markdown`.
            let declared = if content_type.starts_with("text/") {
                format!("{content_type}; charset=utf-8")
            } else {
                content_type
            };
            let payload = json!({
                "type": "text",
                "text": String::from_utf8_lossy(&bytes),
                "content_type": declared,
                "encoding": "utf-8",
            });
            // `read_content` is the one tool the reference does not wrap: its
            // declared return type is a plain `dict`.
            return Ok(json!({
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&payload)? }],
                "isError": false,
                "structuredContent": payload,
            }));
        }

        // The reference resizes and re-encodes images with Pillow before returning
        // them. Re-encoding cannot be made byte-compatible from Rust, so the local
        // core returns the original bytes and labels the true media type instead.
        if content_type.starts_with("image/") {
            return unwrapped_result(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": content_type,
                    "data": encode_base64(&bytes),
                },
            }));
        }

        if bytes.len() > MAX_BINARY_CONTENT_BYTES {
            return unwrapped_result(json!({
                "type": "error",
                "error": format!(
                    "Document size {} bytes exceeds maximum allowed size",
                    bytes.len()
                ),
            }));
        }

        unwrapped_result(json!({
            "type": "document",
            "source": {
                "type": "base64",
                "media_type": content_type,
                "data": encode_base64(&bytes),
            },
        }))
    }

    fn recent_activity(&mut self, arguments: &Value) -> Result<Value> {
        let page_size = arguments["page_size"]
            .as_u64()
            .or_else(|| arguments["limit"].as_u64())
            .or_else(|| arguments["per_page"].as_u64())
            .unwrap_or(10);
        if page_size > 100 {
            return Err(Error::InvalidArgument {
                message: format!("page_size must be <= 100, got {page_size}"),
            });
        }
        let types = parse_activity_types(&arguments["type"])?;
        let type_filter_applied = !types.is_empty();
        let timeframe =
            string_argument(arguments, &["timeframe", "since", "time_range", "lookback"])
                .unwrap_or("7d")
                .to_owned();
        let options = ActivityOptions {
            types: if type_filter_applied {
                types
            } else {
                vec![SearchItemType::Entity]
            },
            depth: arguments["depth"].as_u64().unwrap_or(1) as u32,
            page: arguments["page"]
                .as_u64()
                .or_else(|| arguments["page_number"].as_u64())
                .unwrap_or(1) as u32,
            page_size: page_size as u32,
            max_related: 10,
            since: Some(timeframe::parse_timeframe(&timeframe)?),
            type_filter_applied,
        };
        let activity = recent_context(self.store, self.project_id, &options)?;
        if OutputFormat::from_arguments(arguments, OutputFormat::Text).is_json() {
            return json_result(serde_json::to_value(recent_rows(&activity))?);
        }
        Ok(text_result(render_activity_text(
            &self.project_name,
            &activity,
            &timeframe,
            Some(&self.project_external_id),
            type_filter_applied,
        )))
    }

    fn list_memory_projects(&mut self, arguments: &Value) -> Result<Value> {
        let projects = self.store.projects()?;
        let merged = projects
            .iter()
            .map(|project| {
                let is_default = project.id == self.project_id;
                json!({
                    "name": project.name,
                    "external_id": project.external_id,
                    "path": project.path,
                    "local_path": project.path,
                    "cloud_path": Value::Null,
                    "source": "local",
                    "is_default": is_default,
                    "is_private": false,
                    "display_name": Value::Null,
                    "workspace_name": Value::Null,
                    "workspace_type": Value::Null,
                    "workspace_tenant_id": Value::Null,
                    "workspace_slug": Value::Null,
                    "workspace_is_default": false,
                    // `_sync_support_metadata(None)`.
                    "sync_supported": true,
                    "sync_reason": Value::Null,
                    "local_usage": "sync-supported",
                    "qualified_name": Value::Null,
                })
            })
            .collect::<Vec<_>>();
        let default_project = projects
            .iter()
            .find(|project| project.id == self.project_id)
            .map(|project| project.name.clone());
        if OutputFormat::from_arguments(arguments, OutputFormat::Text).is_json() {
            return json_result(json!({
                "projects": merged,
                "default_project": default_project,
                "constrained_project": self.project_name,
            }));
        }
        Ok(text_result(format!(
            "Project: {}\n\nNote: This MCP server is constrained to a single project.\nAll operations will automatically use this project.",
            self.project_name
        )))
    }

    /// Project creation through a `--project`-constrained server is refused.
    ///
    /// The reference short-circuits on `BASIC_MEMORY_MCP_PROJECT` before opening any
    /// routed client; `auto-memory mcp` is always constrained to one project, so this is
    /// the whole surface rather than an error path. Project lifecycle belongs to the CLI.
    fn create_memory_project(&mut self, arguments: &Value) -> Result<Value> {
        let project_name = required_str(arguments, "project_name")?;
        if OutputFormat::from_arguments(arguments, OutputFormat::Text).is_json() {
            return json_result(json!({
                "name": project_name,
                "path": arguments["project_path"].as_str().unwrap_or_default(),
                "is_default": false,
                "created": false,
                "already_exists": false,
                "error": "PROJECT_CONSTRAINED",
                "message": format!(
                    "Project creation disabled - MCP server is constrained to project '{}'.",
                    self.project_name
                ),
            }));
        }
        Ok(text_result(format!(
            "# Error\n\nProject creation disabled - MCP server is constrained to project '{}'.\nIndex the project with the CLI instead: `auto-memory reindex --vault \"{}\" --index <index> --project \"{project_name}\"`, or run the server without `--project`.",
            self.project_name,
            arguments["project_path"].as_str().unwrap_or_default()
        )))
    }

    /// Project deletion through a `--project`-constrained server is refused.
    fn delete_project(&mut self, arguments: &Value) -> Result<Value> {
        // The reference requires the argument even though this refusal does not echo it.
        required_str(arguments, "project_name")?;
        Ok(text_result(format!(
            "# Error\n\nProject deletion disabled - MCP server is constrained to project '{}'.\nThe CLI cannot remove projects; run the server without `--project` to enable deletion.",
            self.project_name
        )))
    }

    fn view_note(&mut self, arguments: &Value) -> Result<Value> {
        let identifier = required_str(arguments, "identifier")?.to_owned();
        // `view_note` is a thin wrapper over `read_note`'s text surface: the raw
        // markdown is embedded in an artifact instruction block, and a miss returns
        // the "Note Not Found" guidance verbatim rather than wrapping it.
        let content = self.note_text(&identifier, 1, 10)?;
        if content.contains("# Note Not Found") {
            return Ok(text_result(content));
        }
        // `dedent` leaves the template's own indentation in place whenever the
        // inserted markdown contains a column-zero line, which is the normal case;
        // the trailing `.strip()` then eats the leading newline. Reproduced from
        // `mcp/tools/view_note.py`.
        let template = format!(
            "\n        Note retrieved: \"{identifier}\"\n        \n        Display this note as a markdown artifact for the user.\n    \n        Content:\n        ---\n        {content}\n        ---\n        "
        );
        Ok(text_result(dedent(&template).trim().to_owned()))
    }

    fn diagnostics(&mut self) -> Result<Value> {
        let counts = self.store.counts(self.project_id)?;
        // The reference declares this tool with `output_schema=None`, so its payload is a
        // markdown report and the frame carries no `structuredContent`; the report's
        // machine-specific values are replaced by this port's own.
        let config = json!({
            "server": SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "protocol_version": MCP_PROTOCOL_VERSION,
            "project": { "name": self.project_name, "permalink": self.permalink },
            "vault": self.vault.to_string_lossy(),
            "counts": {
                "entities": counts.entities,
                "observations": counts.observations,
                "relations": counts.relations,
            },
            "tools": ToolName::iter().map(<&str>::from).collect::<Vec<_>>(),
        });
        let report = [
            "# Auto Memory Diagnostics".to_owned(),
            String::new(),
            "## Version".to_owned(),
            format!("- auto-memory-rs: {}", env!("CARGO_PKG_VERSION")),
            format!("- Protocol: {MCP_PROTOCOL_VERSION}"),
            String::new(),
            "## System".to_owned(),
            format!("- OS: {}", std::env::consts::OS),
            format!("- Architecture: {}", std::env::consts::ARCH),
            String::new(),
            "## Configuration".to_owned(),
            format!("- Project: {} ({})", self.project_name, self.permalink),
            format!("- Vault: {}", self.vault.display()),
            String::new(),
            "```json".to_owned(),
            serde_json::to_string_pretty(&config)?,
            "```".to_owned(),
        ]
        .join("\n");
        Ok(plain_text_result(report))
    }

    fn notes(&mut self) -> NoteService<'_> {
        NoteService::new(
            self.store,
            self.project_id,
            &self.vault,
            IndexOptions::new(&self.permalink),
        )
    }

    /// Whether the connected client identified itself as OpenAI's MCP client.
    fn is_openai_mcp_client(&self) -> bool {
        self.client_info
            .as_ref()
            .is_some_and(ClientInfo::is_openai_mcp)
    }

    /// `search` — the ChatGPT/OpenAI compatibility adapter.
    ///
    /// A non-OpenAI client is not an error: the reference answers with a well-formed
    /// payload telling the caller to use `search_notes`, and an OpenAI client gets the
    /// same ten results as `search_notes(page=1, page_size=10)` reshaped into
    /// `{id, title, url}` rows.
    fn chatgpt_search(&mut self, arguments: &Value) -> Result<Value> {
        let query = required_str(arguments, "query")?.to_owned();
        if !self.is_openai_mcp_client() {
            return content_items_result(vec![text_item(json!({
                "results": [],
                "error": "Unsupported MCP client",
                "error_message": "The search compatibility tool is only available to \
                                   OpenAI MCP clients. Use search_notes instead.",
            }))]);
        }

        let payload = self.search_payload(&json!({
            "query": query,
            "page": 1,
            "page_size": 10,
            "output_format": "json",
        }))?;
        let raw = payload["results"].as_array().cloned().unwrap_or_default();
        let formatted = raw
            .iter()
            .enumerate()
            .map(|(index, result)| {
                let permalink = result["permalink"].as_str().unwrap_or_default().to_owned();
                let title = result["title"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                json!({
                    "id": if permalink.is_empty() {
                        format!("doc-{index}")
                    } else {
                        permalink.clone()
                    },
                    "title": if title.is_empty() { "Untitled".to_owned() } else { title },
                    "url": permalink,
                })
            })
            .collect::<Vec<_>>();

        content_items_result(vec![text_item(json!({
            "results": formatted,
            "total_count": raw.len(),
            "query": query,
        }))])
    }

    /// `fetch` — the ChatGPT/OpenAI compatibility adapter.
    ///
    /// An OpenAI client gets the raw markdown of the note plus a derived title; a miss
    /// still returns a document, flagged through `metadata.error`.
    fn chatgpt_fetch(&mut self, arguments: &Value) -> Result<Value> {
        let id = required_str(arguments, "id")?.to_owned();
        if !self.is_openai_mcp_client() {
            return content_items_result(vec![text_item(json!({
                "id": id,
                "title": "Unsupported MCP Client",
                "text": "The fetch compatibility tool is only available to OpenAI MCP \
                         clients. Use read_note instead.",
                "url": id,
                "metadata": { "error": "Unsupported MCP client" },
            }))]);
        }

        // `_identifier_for_read_note`: a path-like id is routed as a memory URL, while
        // a bare title or an existing memory URL is passed through untouched.
        let identifier = if id.starts_with("memory://") || !id.contains('/') {
            id.clone()
        } else {
            format!("memory://{id}")
        };
        let content = self.note_text(&identifier, 1, 10)?;
        let metadata = if content.trim_start().starts_with("# Note Not Found") {
            json!({ "error": "Document not found" })
        } else {
            json!({ "format": "markdown" })
        };
        content_items_result(vec![text_item(json!({
            "id": id,
            "title": document_title(&content, &id),
            "text": content,
            "url": id,
            "metadata": metadata,
        }))])
    }

    /// `schema_validate`: one note, one type, or every schema-covered type.
    fn schema_validate(&mut self, arguments: &Value) -> Result<Value> {
        let note_type = arguments["note_type"].as_str().map(str::to_owned);
        let identifier = arguments["identifier"].as_str().map(str::to_owned);
        let output_format = OutputFormat::from_arguments(arguments, OutputFormat::Text);

        let service = SchemaService::new(self.store, self.project_id, &self.vault);
        let outcome = schema_tools::validate(&service, note_type.as_deref(), identifier.as_deref());
        if output_format.is_json() {
            return json_result(outcome.payload());
        }
        Ok(text_result(outcome.text()))
    }

    /// `schema_infer`: frequency analysis plus a suggested Picoschema block.
    fn schema_infer(&mut self, arguments: &Value) -> Result<Value> {
        let note_type = required_str(arguments, "note_type")?.to_owned();
        let threshold = arguments["threshold"].as_f64().unwrap_or(0.25);
        let output_format = OutputFormat::from_arguments(arguments, OutputFormat::Text);

        let service = SchemaService::new(self.store, self.project_id, &self.vault);
        let outcome = schema_tools::infer(&service, &note_type, threshold);
        if output_format.is_json() {
            return json_result(outcome.payload());
        }
        Ok(text_result(outcome.text()))
    }

    /// `schema_diff`: declared fields versus observed usage.
    fn schema_diff(&mut self, arguments: &Value) -> Result<Value> {
        let note_type = required_str(arguments, "note_type")?.to_owned();
        let output_format = OutputFormat::from_arguments(arguments, OutputFormat::Text);

        let service = SchemaService::new(self.store, self.project_id, &self.vault);
        let outcome = schema_tools::diff(&service, &note_type);
        if output_format.is_json() {
            return json_result(outcome.payload());
        }
        Ok(text_result(outcome.text()))
    }
}

/// Tool definitions for `tools/list`, one per [`ToolName`] in declaration order.
pub fn tool_definitions() -> Vec<Value> {
    ToolName::iter().map(ToolName::definition).collect()
}

/// Tool definitions as a specific session advertises them.
///
/// A read-only session omits the mutating tools; the HTTP transport uses this to
/// build rmcp's `Tool` list without opening a [`McpServer`] (which borrows a `Store`).
pub(crate) fn advertised_tools(read_only: bool) -> Vec<Value> {
    ToolName::iter()
        .filter(|tool| !read_only || !tool.is_mutating())
        .map(ToolName::definition)
        .collect()
}

/// Register (or refresh) the project row for a vault before serving.
pub fn ensure_project(
    store: &mut Store,
    name: &str,
    permalink: &str,
    vault: &std::path::Path,
) -> Result<i64> {
    let project_id = store.upsert_project(name, permalink, &vault.to_string_lossy())?;
    let mut service = IndexService::new(store, project_id, vault, IndexOptions::new(permalink));
    service.reconcile()?;
    Ok(project_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tools/list`, `tools/call`, and the diagnostics report share one name set.
    ///
    /// `call_tool` matches on `ToolName` exhaustively, so this covers the remaining
    /// surface: every variant is advertised exactly once with a description and an
    /// object input schema, and the wire names still spell the compatibility contract
    /// (`docs/mcp-spec.md` §3–5) that clients and the reference golden depend on. The
    /// list below is the contract, deliberately not derived from the enum, so a
    /// renamed variant cannot quietly rename a tool on the wire.
    #[test]
    fn tool_names_cover_the_advertised_definitions() {
        let names: Vec<&str> = ToolName::iter().map(<&str>::from).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "duplicate tool names: {names:?}");
        assert_eq!(
            names,
            [
                "write_note",
                "read_note",
                "edit_note",
                "move_note",
                "delete_note",
                "search_notes",
                "search",
                "fetch",
                "build_context",
                "schema_validate",
                "schema_infer",
                "schema_diff",
                "auto_memory_diagnostics",
                "list_directory",
                "read_content",
                "view_note",
                "recent_activity",
                "list_memory_projects",
                "create_memory_project",
                "delete_project",
            ],
            "the wire names moved"
        );

        for tool in ToolName::iter() {
            assert_eq!(tool.to_string().parse::<ToolName>().ok(), Some(tool));
        }
        assert_eq!(
            "nope".parse::<ToolName>(),
            Err(strum::ParseError::VariantNotFound)
        );

        let definitions = tool_definitions();
        let advertised: Vec<&str> = definitions
            .iter()
            .map(|definition| definition["name"].as_str().expect("tool name"))
            .collect();
        assert_eq!(
            advertised, names,
            "tools/list must follow declaration order"
        );

        for definition in &definitions {
            let name = definition["name"].as_str().expect("tool name");
            assert!(
                definition["description"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
                "{name} has no description"
            );
            assert_eq!(
                definition["inputSchema"]["type"], "object",
                "{name} input schema"
            );
            assert!(
                definition["inputSchema"]["properties"].is_object(),
                "{name} properties"
            );
            assert!(
                definition["inputSchema"]["required"].is_array(),
                "{name} required list"
            );
        }
    }
}
