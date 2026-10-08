//! `auto-memory` command-line entry point.
//!
//! Phase 6 surface:
//! - `parse <path>` — serialize the parse layer (compared with the reference parser).
//! - `reindex --vault <dir> --index <db> [--project <name>] [--full]` — rebuild or
//!   reconcile the derived index.
//! - `status --index <db> --project <permalink>` — print index counts.
//! - `context <memory://url>` — build graph context (JSON, or the reference's
//!   undecorated `--plain` outline).

use std::future::Future;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use auto_memory::adapters::mcp::http::{
    DEFAULT_HTTP_HOST, DEFAULT_HTTP_PATH, DEFAULT_HTTP_PORT, HttpServer, serve,
};
use auto_memory::adapters::mcp::{McpServer, ensure_project};
use auto_memory::application::context::{ContextOptions, build_context, render_plain};
use auto_memory::application::schema::SchemaService;
use auto_memory::application::schema_tools;
use auto_memory::config::{UserConfig, load_user_config, resolve_index, resolve_project};
use auto_memory::domain::permalink::generate_permalink;
use auto_memory::domain::search::SearchItemType;
use auto_memory::domain::timeframe;
use auto_memory::hooks::{
    Harness, HookEvent, build_session_brief, checkpoint_prompt, load_harness_settings, mapping_dir,
    normalize,
};
use auto_memory::indexing::{
    DEFAULT_WATCH_WINDOW, IndexOptions, IndexService, VaultWatcher, watch_once, watch_vault,
};
use auto_memory::runtime::{
    DEFAULT_RERANKER_CANDIDATES, DEFAULT_RERANKER_MAX_DOCUMENT_CHARS, MODEL_CACHE_ENV,
    OnnxEmbeddingProvider, OnnxRerankProvider, REFERENCE_MODEL_REPO, RerankProvider, RerankRequest,
    default_model_cache, model_cache_search_paths, onnx_runtime_search_paths, reference_model_dir,
    resolve_onnx_runtime,
};
use auto_memory::search::embedding::{EmbeddingProvider, FixtureEmbeddingProvider};
use auto_memory::search::rerank::FixtureRerankProvider;
use auto_memory::search::text::TextSearchOptions;
use auto_memory::search::vector::{VectorSearchOptions, search_hybrid, search_vector};
use auto_memory::storage::Store;
use tracing_subscriber::EnvFilter;

// One subcommand per verb. `clap` owns parsing, required arguments, `--help`, and the
// exit-2 usage error; the dispatched functions receive typed values instead of the
// untyped `--key value` bag this binary used to carry.
#[derive(Parser)]
#[command(
    name = "auto-memory",
    version,
    about = "Local-first Rust knowledge base with markdown notes, a derived SQLite index, and an MCP server"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Every verb this binary implements.
#[derive(Subcommand)]
enum Command {
    /// Serialize the parse layer for one markdown file.
    Parse {
        /// Markdown file to parse.
        path: PathBuf,
    },
    /// Rebuild or reconcile the derived index.
    Reindex(ReindexArgs),
    /// Keep the markdown index in sync with the vault.
    Watch(WatchArgs),
    /// Serve the MCP tools on stdio or Streamable HTTP.
    Mcp(McpArgs),
    /// Print index counts for one project.
    Status(StatusArgs),
    /// Report what is usable on this machine.
    Doctor(DoctorArgs),
    /// Register, list, or remove vaults.
    #[command(subcommand)]
    Project(ProjectCommand),
    /// Resolve a `memory://` URL and print its graph context.
    Context(ContextArgs),
    /// Validate, infer, or diff note schemas.
    #[command(subcommand)]
    Schema(SchemaCommand),
    /// Harness lifecycle hook entry point.
    Hook(HookArgs),
    /// Search the index (text, `--vector`, or `--hybrid`).
    Search(SearchArgs),
}

/// Flags selecting the embedding runtime: fixture, model cache, ONNX library.
#[derive(Args, Clone)]
struct EmbeddingArgs {
    /// Replay captured reference vectors instead of loading ONNX Runtime.
    #[arg(long, value_name = "FILE")]
    embedding_fixture: Option<PathBuf>,
    /// Directory holding the fastembed model cache.
    #[arg(long, value_name = "DIR")]
    model_cache: Option<PathBuf>,
    /// Path to the ONNX Runtime shared library or its containing directory.
    #[arg(long, value_name = "PATH")]
    onnx_runtime: Option<PathBuf>,
}

/// Cross-encoder reranking flags, opt-in like the reference's `reranker_enabled`.
#[derive(Args, Clone)]
struct RerankArgs {
    /// Rerank vector/hybrid results with the cross-encoder.
    #[arg(long)]
    reranker: bool,
    /// Deterministic reranker scores instead of the ONNX model.
    #[arg(long, value_name = "FILE")]
    reranker_fixture: Option<PathBuf>,
    /// Number of candidates fed to the reranker.
    #[arg(long, value_name = "N")]
    reranker_candidates: Option<usize>,
    /// Maximum document length handed to the reranker.
    #[arg(long, value_name = "N")]
    reranker_max_chars: Option<usize>,
}

/// `auto-memory reindex --vault <dir> --index <db>`.
#[derive(Args)]
struct ReindexArgs {
    /// Vault directory to index (defaults to the registered project's vault).
    #[arg(long, value_name = "DIR")]
    vault: Option<PathBuf>,
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Project name (defaults to `default_project`, else the vault directory name).
    #[arg(long, value_name = "NAME")]
    project: Option<String>,
    /// Rebuild every note instead of reconciling.
    #[arg(long)]
    full: bool,
    /// Refresh the vector index instead of the markdown index.
    #[arg(long)]
    embeddings: bool,
    #[command(flatten)]
    embed: EmbeddingArgs,
}

/// `auto-memory watch --vault <dir> --index <db>`.
#[derive(Args)]
struct WatchArgs {
    /// Vault directory to watch (defaults to the registered project's vault).
    #[arg(long, value_name = "DIR")]
    vault: Option<PathBuf>,
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Project name (defaults to `default_project`, else the vault directory name).
    #[arg(long, value_name = "NAME")]
    project: Option<String>,
    /// Debounce window in milliseconds.
    #[arg(long, value_name = "N")]
    window_ms: Option<u64>,
    /// Apply a single batch and exit.
    #[arg(long)]
    once: bool,
    /// Not a `watch` flag. Declared only so the command can refuse it with the
    /// `reindex --embeddings` alternative instead of clap's generic error.
    #[arg(long, hide = true)]
    embeddings: bool,
}

/// `auto-memory mcp --vault <dir> --index <db>`.
#[derive(Args)]
struct McpArgs {
    /// Vault directory to serve (defaults to the registered project's vault).
    #[arg(long, value_name = "DIR")]
    vault: Option<PathBuf>,
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Project name (defaults to `default_project`, else the vault directory name).
    #[arg(long, value_name = "NAME")]
    project: Option<String>,
    /// Serve the Streamable HTTP transport instead of stdio.
    #[arg(long)]
    http: bool,
    /// HTTP bind host.
    #[arg(long, value_name = "HOST")]
    host: Option<String>,
    /// HTTP bind port.
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,
    /// HTTP path of the MCP endpoint.
    #[arg(long, value_name = "PATH")]
    path: Option<String>,
    /// Refuse the tools that write.
    #[arg(long)]
    read_only: bool,
    #[command(flatten)]
    embed: EmbeddingArgs,
    #[command(flatten)]
    rerank: RerankArgs,
}

/// `auto-memory status --index <db> --project <permalink>`.
#[derive(Args)]
struct StatusArgs {
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Project permalink.
    #[arg(long, value_name = "NAME")]
    project: String,
}

/// `auto-memory doctor`.
#[derive(Args)]
struct DoctorArgs {
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard per-user path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Vault to inspect.
    #[arg(long, value_name = "DIR")]
    vault: Option<PathBuf>,
    /// Project permalink to report counts for.
    #[arg(long, value_name = "NAME")]
    project: Option<String>,
    /// Model cache directory to report on.
    #[arg(long, value_name = "DIR")]
    model_cache: Option<PathBuf>,
    /// ONNX Runtime shared library or containing directory to report on.
    #[arg(long, value_name = "PATH")]
    onnx_runtime: Option<PathBuf>,
    /// Emit the report as JSON.
    #[arg(long)]
    json: bool,
}

/// `auto-memory project <add|list|remove>`.
#[derive(Subcommand)]
enum ProjectCommand {
    /// Register a vault and index it.
    Add(ProjectAddArgs),
    /// List every registered project.
    List(ProjectListArgs),
    /// Unregister a project; the vault is left alone.
    Remove(ProjectRemoveArgs),
}

/// `project add <name> <path>`.
#[derive(Args)]
struct ProjectAddArgs {
    /// Display name.
    name: String,
    /// Vault directory.
    path: PathBuf,
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard per-user path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Permalink slug (defaults to one generated from the name).
    #[arg(long, value_name = "SLUG")]
    permalink: Option<String>,
    /// Register without indexing.
    #[arg(long)]
    no_index: bool,
    /// Emit the result as JSON.
    #[arg(long)]
    json: bool,
}

/// `project list`.
#[derive(Args)]
struct ProjectListArgs {
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard per-user path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Emit the result as JSON.
    #[arg(long)]
    json: bool,
}

/// `project remove <name|permalink>`.
#[derive(Args)]
struct ProjectRemoveArgs {
    /// Project name or permalink.
    identifier: String,
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard per-user path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Emit the result as JSON.
    #[arg(long)]
    json: bool,
}

/// `auto-memory context <url> --index <db> --project <permalink>`.
#[derive(Args)]
struct ContextArgs {
    /// `memory://` URL to resolve.
    url: String,
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Project permalink.
    #[arg(long, value_name = "NAME")]
    project: String,
    /// Traversal depth.
    #[arg(long, value_name = "N")]
    depth: Option<u32>,
    /// Timeframe window (default `7d`).
    #[arg(long, value_name = "WINDOW")]
    timeframe: Option<String>,
    /// Page number.
    #[arg(long, value_name = "N")]
    page: Option<u32>,
    /// Results per page.
    #[arg(long, value_name = "N")]
    page_size: Option<u32>,
    /// Related rows per entity.
    #[arg(long, value_name = "N")]
    max_related: Option<u32>,
    /// Print the reference's undecorated outline instead of JSON.
    #[arg(long)]
    plain: bool,
    /// JSON is already the default; accepted for symmetry with `--plain`.
    #[arg(long)]
    json: bool,
}

/// `auto-memory schema <validate|infer|diff>`.
#[derive(Subcommand)]
enum SchemaCommand {
    /// Validate a note type or a note file.
    Validate(SchemaArgs),
    /// Infer a schema definition from existing notes.
    Infer(SchemaInferArgs),
    /// Detect drift between a schema and actual note usage.
    Diff(SchemaArgs),
}

/// Index/project/vault selection shared by the schema verbs.
#[derive(Args)]
struct SchemaCommonArgs {
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Project permalink.
    #[arg(long, value_name = "NAME")]
    project: String,
    /// Vault override (defaults to the project's indexed path).
    #[arg(long, value_name = "DIR")]
    vault: Option<PathBuf>,
}

/// `schema validate` / `schema diff`.
#[derive(Args)]
struct SchemaArgs {
    /// Note type, or a note path (contains `/` or `.`).
    target: Option<String>,
    #[command(flatten)]
    common: SchemaCommonArgs,
    /// Exit non-zero when validation reports errors.
    #[arg(long)]
    strict: bool,
    /// Print the MCP text surface instead of the reference JSON.
    #[arg(long)]
    text: bool,
}

/// `schema infer <note_type>`.
#[derive(Args)]
struct SchemaInferArgs {
    /// Note type to infer from.
    note_type: String,
    #[command(flatten)]
    common: SchemaCommonArgs,
    /// Optional-field threshold.
    #[arg(long, value_name = "F")]
    threshold: Option<f64>,
    /// Print the MCP text surface instead of the reference JSON.
    #[arg(long)]
    text: bool,
}

/// `auto-memory hook <session-start|pre-compact>`.
#[derive(Args)]
struct HookArgs {
    /// Hook verb: `session-start` or `pre-compact`.
    verb: String,
    /// Harness emitting the hook (`claude`, `codex`, `pi`, or `tact`).
    #[arg(long, value_name = "NAME")]
    harness: Option<String>,
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Project permalink (overrides the harness mapping).
    #[arg(long, value_name = "NAME")]
    project: Option<String>,
    /// Project directory the mapping is resolved against.
    #[arg(long, value_name = "DIR")]
    project_dir: Option<PathBuf>,
}

/// `auto-memory search --index <db> --project <permalink> [filters] <query>`.
#[derive(Args)]
struct SearchArgs {
    /// Query terms; omit them for a filter-only search.
    #[arg(value_name = "QUERY", num_args = 0..)]
    query: Vec<String>,
    /// Index database (defaults to `AUTO_MEMORY_INDEX`, the user config file, then
    /// the standard path).
    #[arg(long, value_name = "DB")]
    index: Option<PathBuf>,
    /// Project permalink.
    #[arg(long, value_name = "NAME")]
    project: String,
    /// Title filter.
    #[arg(long, value_name = "T")]
    title: Option<String>,
    /// Note type filter.
    #[arg(long = "type", value_name = "T")]
    note_type: Option<String>,
    /// Tag filter.
    #[arg(long, value_name = "TAG")]
    tag: Option<String>,
    /// Status filter.
    #[arg(long, value_name = "S")]
    status: Option<String>,
    /// Category filter.
    #[arg(long, value_name = "C")]
    category: Option<String>,
    /// Entity type filter (`entity`, `observation`, or `relation`).
    #[arg(long, value_name = "TYPE")]
    entity_type: Option<String>,
    /// Permalink or permalink glob.
    #[arg(long, value_name = "P")]
    permalink: Option<String>,
    /// Metadata filter, `key=value`.
    #[arg(long, value_name = "KEY=VALUE")]
    meta: Option<String>,
    /// Date or window bound (also accepted as `--after_date`).
    #[arg(long = "after-date", alias = "after_date", value_name = "WINDOW")]
    after_date: Option<String>,
    /// Page number.
    #[arg(long, value_name = "N")]
    page: Option<u32>,
    /// Results per page.
    #[arg(long, value_name = "N")]
    page_size: Option<u32>,
    /// Rank against the vector index only.
    #[arg(long)]
    vector: bool,
    /// Fuse text and vector retrieval.
    #[arg(long)]
    hybrid: bool,
    /// Minimum semantic score.
    #[arg(long, value_name = "F")]
    min_similarity: Option<f32>,
    #[command(flatten)]
    embed: EmbeddingArgs,
    #[command(flatten)]
    rerank: RerankArgs,
}

fn main() -> ExitCode {
    init_tracing();
    // `clap` owns `--version` (and `-V`); keep the historical `-v` spelling working.
    if std::env::args().nth(1).is_some_and(|arg| arg == "-v") {
        println!("auto-memory {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    match Cli::parse().command {
        Command::Parse { path } => parse_command(&path),
        Command::Reindex(args) => async_command(reindex_command(args)),
        Command::Status(args) => async_command(status_command(args)),
        Command::Doctor(args) => async_command(doctor_command(args)),
        Command::Project(command) => async_command(project_command(command)),
        Command::Search(args) => async_command(search_command(args)),
        Command::Context(args) => async_command(context_command(args)),
        Command::Schema(command) => async_command(schema_command(command)),
        Command::Hook(args) => async_command(hook_command(args)),
        Command::Watch(args) => async_command(watch_command(args)),
        Command::Mcp(args) => async_command(mcp_command(args)),
    }
}

fn usage(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(2)
}

/// The environment half of the index chain.
///
/// Read here rather than inside the resolver so the chain stays pure and testable
/// (`specs/config-discovery-spec.md` §8).
fn index_from_environment() -> Option<PathBuf> {
    std::env::var_os("AUTO_MEMORY_INDEX").map(PathBuf::from)
}

/// Resolve `--index` for a CLI command: flag → environment → config file → default.
///
/// Returns the loaded config alongside it, because the commands that need an index
/// usually need `default_project` too. An unusable config file is an error here (the
/// user asked for a specific index and something is wrong with the file that might have
/// supplied one) while the hook warns and carries on — the same chain, two policies.
fn cli_index(explicit: Option<PathBuf>) -> Option<(PathBuf, UserConfig)> {
    let user = load_user_config();
    if let UserConfig::Malformed { path, error } = &user {
        eprintln!("Error: unusable config {}: {error}", path.display());
        return None;
    }
    let index = resolve_index(explicit, index_from_environment(), &user).value;
    Some((index, user))
}

/// Resolve the `(name, permalink, vault)` a vault-reading command runs against.
///
/// Shared by `reindex`, `watch`, and `mcp` so the precedence cannot drift between them
/// (`specs/config-discovery-spec.md` §8). Highest precedence first:
///
/// - project: `--project <name>` (a display name, normalized to a permalink) → the user
///   config's `default_project` (already a permalink, like the mapping files'
///   `primaryProject`) → the vault's directory name;
/// - vault: `--vault <dir>` → the registered project row's `path`.
///
/// Deriving the vault from the row is *safer* than accepting it: a typo used to point
/// `mcp`/`watch` reconcile at the wrong directory, which prunes that project's index rows
/// (`docs/integration-guide.md` §2.1). Either way the directory is checked before any
/// scan, because a missing vault otherwise looks exactly like an empty one.
async fn resolve_project_target(
    store: &Store,
    user: &UserConfig,
    project: Option<String>,
    vault: Option<PathBuf>,
) -> std::result::Result<(String, String, PathBuf), String> {
    let (name, permalink, registered) = match project {
        Some(name) => {
            let permalink = generate_permalink(&name);
            let row = store
                .project_by_permalink(&permalink)
                .await
                .map_err(|error| format!("failed to read project {permalink}: {error}"))?;
            (name, permalink, row)
        }
        None => match resolve_project(None, None, user) {
            Some(resolved) => {
                let permalink = resolved.value;
                let row = store
                    .project_by_permalink(&permalink)
                    .await
                    .map_err(|error| format!("failed to read project {permalink}: {error}"))?
                    .ok_or_else(|| format!("project not found: {permalink}"))?;
                (row.name.clone(), permalink, Some(row))
            }
            None => match &vault {
                Some(vault) => {
                    // A vault that is already registered names its own project.
                    // Minting a name from the directory instead would create a
                    // *second* project row for the same vault and split the
                    // permalinks — `--vault ~/agent-memory` would index the notes
                    // again under `agent-memory` while `1m` already owned them.
                    let existing = store
                        .projects()
                        .await
                        .map_err(|error| format!("failed to read projects: {error}"))?
                        .into_iter()
                        .find(|row| same_path(&row.path, vault));
                    if let Some(row) = existing {
                        (row.name.clone(), row.permalink.clone(), Some(row))
                    } else {
                        let name = vault.file_name().map_or_else(
                            || "default".to_owned(),
                            |name| name.to_string_lossy().into_owned(),
                        );
                        let permalink = generate_permalink(&name);
                        (name, permalink, None)
                    }
                }
                None => {
                    return Err(
                        "no project: pass `--project <name>`, or set `default_project` in \
                         ~/.config/auto-memory/config.json, or pass `--vault <dir>`"
                            .to_owned(),
                    );
                }
            },
        },
    };

    let vault = match vault {
        Some(vault) => vault,
        None => match &registered {
            Some(row) => PathBuf::from(&row.path),
            None => {
                return Err(format!(
                    "no vault for project {permalink}: pass `--vault <dir>`, or register it \
                     with `auto-memory project add {name} <vault>`"
                ));
            }
        },
    };
    if !vault.is_dir() {
        return Err(format!("vault directory not found: {}", vault.display()));
    }
    Ok((name, permalink, vault))
}

/// Whether a registered vault path and a given directory name the same place.
///
/// The stored path is whatever the user typed at `project add` time, so a trailing
/// slash or a symlinked home would miss an exact comparison; both sides fall back
/// to `canonicalize` before giving up. A path that cannot be canonicalised (it does
/// not exist yet) is simply not a match.
fn same_path(registered: &str, vault: &Path) -> bool {
    let registered = Path::new(registered);
    if registered == vault {
        return true;
    }
    match (registered.canonicalize(), vault.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Install the diagnostics subscriber.
///
/// stdout is reserved for machine-readable output — the `watch --once` report, the
/// `search` payload, MCP protocol frames — so every log line goes to stderr, where a
/// daemon manager (systemd, `nohup`, a pipe) picks it up without corrupting the
/// stream a caller parses.
///
/// `RUST_LOG` overrides the default, which keeps this crate at `info` (startup,
/// reconcile summaries, one line per applied watch batch) and everything else at
/// `warn`, so a default run stays readable and `RUST_LOG=auto_memory=debug` adds the
/// per-event detail. Filtering is by target, so the environment variable also works
/// on a per-module basis (`RUST_LOG=auto_memory::indexing=debug`).
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG));
    // A second initialisation (tests, or a future re-entry) must not abort the
    // process: losing the subscriber is not worth failing a memory index over.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        // Colour only a real terminal: a log file, `journalctl`, and a client that
        // captures stderr should not carry escape codes.
        .with_ansi(std::io::stderr().is_terminal())
        // The CLI is one binary; the module path adds noise, the message does not.
        .with_target(false)
        .try_init();
}

/// Default `RUST_LOG` filter: this crate at `info`, dependencies at `warn`.
const DEFAULT_LOG: &str = "auto_memory=info,warn";

/// Run an event-loop subcommand (`mcp`, `watch`) on a fresh tokio runtime.
///
/// The runtime is built here rather than around `main` so the one-shot commands keep
/// their synchronous startup: a worker pool costs threads, and `auto-memory search` or
/// `status` has nothing to overlap.
fn async_command(future: impl Future<Output = ExitCode>) -> ExitCode {
    match auto_memory::runtime::executor::runtime() {
        Ok(runtime) => runtime.block_on(future),
        Err(error) => {
            eprintln!("failed to start the async runtime: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `auto-memory schema <validate|infer|diff>`.
///
/// Mirrors the reference's `bm tool schema-*` commands, which call the MCP tool with
/// `output_format="json"` and print the result through
/// `json.dumps(..., indent=2, ensure_ascii=True, default=str)`. `--text` prints the
/// MCP text surface instead (the markdown report or the guidance block).
async fn schema_command(command: SchemaCommand) -> ExitCode {
    match command {
        SchemaCommand::Validate(args) => schema_validate_command(args).await,
        SchemaCommand::Infer(args) => schema_infer_command(args).await,
        SchemaCommand::Diff(args) => schema_diff_command(args).await,
    }
}

/// Open the index and resolve the project (and vault) a schema command runs against.
async fn schema_context(
    index: Option<&Path>,
    permalink: &str,
    vault: Option<&Path>,
) -> Result<(Store, i64, PathBuf), String> {
    let user = load_user_config();
    if let UserConfig::Malformed { path, error } = &user {
        return Err(format!("unusable config {}: {error}", path.display()));
    }
    let index = resolve_index(
        index.map(Path::to_path_buf),
        index_from_environment(),
        &user,
    )
    .value;
    let store = Store::open(&index)
        .await
        .map_err(|error| format!("failed to open index {}: {error}", index.display()))?;
    let project = store
        .project_by_permalink(permalink)
        .await
        .map_err(|error| format!("failed to read project: {error}"))?
        .ok_or_else(|| format!("project not found: {permalink}"))?;
    // Schema definitions are read from their files, so the vault matters: it defaults
    // to the path the project was indexed from.
    let vault = vault.map_or_else(|| PathBuf::from(&project.path), Path::to_path_buf);
    Ok((store, project.id, vault))
}

async fn schema_validate_command(args: SchemaArgs) -> ExitCode {
    let (store, project_id, vault) = match schema_context(
        args.common.index.as_deref(),
        &args.common.project,
        args.common.vault.as_deref(),
    )
    .await
    {
        Ok(context) => context,
        Err(message) => return usage(&message),
    };

    // Reference heuristic: a target containing `/` or `.` is an identifier, anything
    // else is a note type.
    let (note_type, identifier) = match args.target.as_deref() {
        Some(target) if target.contains('/') || target.contains('.') => (None, Some(target)),
        Some(target) => (Some(target), None),
        None => (None, None),
    };

    let service = SchemaService::new(&store, project_id, &vault);
    let outcome = schema_tools::validate(&service, note_type, identifier).await;
    let payload = outcome.payload();
    let text = outcome.text();
    let status = if args.strict && payload["error_count"].as_u64().unwrap_or(0) > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    print_schema_outcome(&payload, &text, args.text);
    status
}

async fn schema_infer_command(args: SchemaInferArgs) -> ExitCode {
    let threshold = args
        .threshold
        .unwrap_or(auto_memory::application::schema::OPTIONAL_THRESHOLD);
    let (store, project_id, vault) = match schema_context(
        args.common.index.as_deref(),
        &args.common.project,
        args.common.vault.as_deref(),
    )
    .await
    {
        Ok(context) => context,
        Err(message) => return usage(&message),
    };

    let service = SchemaService::new(&store, project_id, &vault);
    let outcome = schema_tools::infer(&service, &args.note_type, threshold).await;
    let payload = outcome.payload();
    let text = outcome.text();
    print_schema_outcome(&payload, &text, args.text);
    ExitCode::SUCCESS
}

async fn schema_diff_command(args: SchemaArgs) -> ExitCode {
    let Some(note_type) = args.target.clone() else {
        return usage("schema diff requires a note type");
    };
    let (store, project_id, vault) = match schema_context(
        args.common.index.as_deref(),
        &args.common.project,
        args.common.vault.as_deref(),
    )
    .await
    {
        Ok(context) => context,
        Err(message) => return usage(&message),
    };

    let service = SchemaService::new(&store, project_id, &vault);
    let outcome = schema_tools::diff(&service, &note_type).await;
    let payload = outcome.payload();
    let text = outcome.text();
    print_schema_outcome(&payload, &text, args.text);
    ExitCode::SUCCESS
}

/// Print a schema outcome as the reference CLI does, or as the MCP text surface.
fn print_schema_outcome(payload: &serde_json::Value, text: &str, text_surface: bool) {
    if text_surface {
        println!("{text}");
        return;
    }
    println!("{}", auto_memory::pycompat::python_json_dumps(payload));
}

/// `auto-memory hook <session-start|pre-compact> --harness <name>`.
///
/// The harness lifecycle entry point. It reads one JSON object on stdin and
/// prints context on stdout; stdout stays clean because the verb prints exactly
/// once. Every failure is **fail-open** (exit 0): a hook is advisory and must
/// never disrupt an agent session.
async fn hook_command(args: HookArgs) -> ExitCode {
    let harness = match args.harness.as_deref() {
        None => Harness::Claude,
        Some(value) => match Harness::parse(value) {
            Some(harness) => harness,
            None => {
                // A hook is invoked by a host agent, so a bad `--harness` is a
                // configuration mistake in *that* host — not a usage error the
                // caller can act on. Failing open keeps the documented contract
                // ("every failure path exits 0") true for the whole command;
                // `usage()` here used to be the one path that broke it.
                //
                // One line, not two: the subscriber already writes to stderr
                // (see `init_tracing`), so an extra `eprintln!` would print the
                // same sentence twice.
                tracing::warn!(
                    "hook {}: unknown harness: {value} (expected claude, codex, pi, or tact)",
                    args.verb
                );
                return ExitCode::SUCCESS;
            }
        },
    };
    // A hook verb never fails the caller; diagnostics go to stderr and the
    // process still exits 0.
    if let Err(message) = run_hook(&args.verb, harness, &args).await {
        tracing::warn!("hook {} failed: {message}", args.verb);
        eprintln!("auto-memory hook {}: {message}", args.verb);
    }
    ExitCode::SUCCESS
}

/// Run one hook verb. `Err` is a diagnostic, not a failure.
async fn run_hook(verb: &str, harness: Harness, args: &HookArgs) -> Result<(), String> {
    let hook_event = match verb {
        "session-start" => HookEvent::SessionStarted,
        "pre-compact" => HookEvent::CompactionImminent,
        other => return Err(format!("unknown hook verb: {other}")),
    };
    let payload = read_hook_payload();
    let event = normalize(harness, hook_event, &payload);

    let mapping = mapping_dir(args.project_dir.as_deref(), &event.cwd);
    let (settings, configured) = load_harness_settings(harness, &mapping);

    // Codex and Tact both ignore PreCompact stdout and ask for the checkpoint
    // from the post-compaction SessionStart instead, so there is nothing to
    // print here.
    if matches!(hook_event, HookEvent::CompactionImminent) {
        return Ok(());
    }

    // The user config file supplies both values below when nothing more specific
    // does. A broken file must not break a session: warn and continue down the
    // chain (the CLI errors instead — same chain, two policies).
    let user = load_user_config();
    if let UserConfig::Malformed { path, error } = &user {
        tracing::warn!("ignoring unusable config {}: {error}", path.display());
    }
    let Some(permalink) = resolve_project(
        args.project.clone(),
        settings.primary_project.clone(),
        &user,
    ) else {
        // No project resolved, so there is no brief to give. Say why, and name the
        // projects this index actually has — a nudge that does not name them makes the
        // user run another command to find out what to write in the mapping file.
        //
        // Still no guessing: an unnamed project is never used, because a brief from the
        // wrong knowledge graph is worse than no brief.
        let index = resolve_index(args.index.clone(), index_from_environment(), &user).value;
        let candidates = registered_permalinks(&index).await;
        let hint = if configured {
            harness.profile().pin_tip
        } else {
            harness.profile().setup_nudge
        };
        println!(
            "# Auto Memory\n\n{}",
            nudge_with_projects(hint, &candidates)
        );
        return Ok(());
    };
    let permalink = permalink.value;
    // An explicit `--project` overrides the mapping; reflect it in the brief so
    // the header and placement guidance name the project actually queried.
    let mut settings = settings;
    settings.primary_project = Some(permalink.clone());

    let index = resolve_index(args.index.clone(), index_from_environment(), &user).value;
    let store = Store::open(&index)
        .await
        .map_err(|error| format!("failed to open index {}: {error}", index.display()))?;
    let Some(project) = store
        .project_by_permalink(&permalink)
        .await
        .map_err(|error| format!("failed to read project {permalink}: {error}"))?
    else {
        return Err(format!("project not found: {permalink}"));
    };

    // Both harnesses deliver the checkpoint request from the post-compaction
    // SessionStart; Claude and Pi have no checkpoint path.
    let checkpoint = if matches!(harness, Harness::Codex | Harness::Tact)
        && event.trigger.as_deref() == Some("compact")
        && settings.checkpoint_on_compact
    {
        Some(checkpoint_prompt(&event))
    } else {
        None
    };
    let brief = build_session_brief(
        &store,
        project.id,
        harness.profile(),
        &settings,
        configured,
        checkpoint.as_deref(),
    )
    .await;
    let brief: String = brief
        .chars()
        .take(auto_memory::hooks::profiles::MAX_BRIEF_CHARS)
        .collect();
    println!("{brief}");
    Ok(())
}

/// Read the harness hook payload from stdin; junk or an interactive shell
/// normalizes to an empty object (never block waiting for input).
fn read_hook_payload() -> serde_json::Value {
    use std::io::Read;
    let empty = serde_json::Value::Object(serde_json::Map::new());
    if std::io::stdin().is_terminal() {
        return empty;
    }
    let mut text = String::new();
    if std::io::stdin().read_to_string(&mut text).is_err() {
        return empty;
    }
    serde_json::from_str(&text).unwrap_or(empty)
}

/// Maximum permalinks the first-run nudge names before trailing off.
const MAX_NUDGE_PROJECTS: usize = 10;

/// The permalinks registered in the index, for the first-run nudge.
///
/// Best-effort and strictly read-only: a missing or unreadable index yields an empty list
/// and the nudge keeps its generic wording. It must not *create* the index — `Store::open`
/// does, and a diagnostic that writes is a diagnostic you cannot trust.
async fn registered_permalinks(index: &Path) -> Vec<String> {
    if !index.exists() {
        return Vec::new();
    }
    let Ok(store) = Store::open(index).await else {
        return Vec::new();
    };
    store
        .projects()
        .await
        .map(|rows| rows.into_iter().map(|row| row.permalink).collect())
        .unwrap_or_default()
}

/// A profile's nudge or tip, with the permalinks this index actually has.
///
/// Naming the candidates is what makes the message actionable: the user learns the exact
/// value to put in the mapping file without running `auto-memory project list` first.
fn nudge_with_projects(hint: &str, candidates: &[String]) -> String {
    if candidates.is_empty() {
        return hint.to_owned();
    }
    let mut listed: Vec<&str> = candidates
        .iter()
        .take(MAX_NUDGE_PROJECTS)
        .map(String::as_str)
        .collect();
    if candidates.len() > MAX_NUDGE_PROJECTS {
        listed.push("…");
    }
    format!(
        "{hint}\n\n_Registered projects in this index: {}._",
        listed.join(", ")
    )
}

/// `auto-memory project <add|list|remove>`.
///
/// Project lifecycle was the one gap the MCP surface pointed at but the CLI did not
/// have: `create_memory_project` and `delete_project` refuse on a `--project`-constrained
/// server and tell the caller to use the CLI, and until now that meant hand-editing the
/// index. The argument shape is the reference's (`project add <name> <path>`) so the
/// refusal text only has to differ by the binary name.
async fn project_command(command: ProjectCommand) -> ExitCode {
    match command {
        ProjectCommand::Add(args) => project_add_command(args).await,
        ProjectCommand::List(args) => project_list_command(args).await,
        ProjectCommand::Remove(args) => project_remove_command(args).await,
    }
}

/// `project add <name> <path>` — register a vault, then index it.
async fn project_add_command(args: ProjectAddArgs) -> ExitCode {
    let ProjectAddArgs {
        name,
        path,
        index,
        permalink,
        no_index,
        json,
    } = args;
    if !path.is_dir() {
        return usage(&format!(
            "project path is not a directory: {}",
            path.display()
        ));
    }
    let permalink = permalink.unwrap_or_else(|| generate_permalink(&name));
    let Some((index, _user)) = cli_index(index) else {
        return ExitCode::FAILURE;
    };

    let mut store = match Store::open(&index).await {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let existed = matches!(store.project_by_permalink(&permalink).await, Ok(Some(_)));

    // Registering without indexing leaves a project that every query reports as empty,
    // which reads as a bug; index by default and let `--no-index` opt out.
    let indexed = if no_index {
        store
            .upsert_project(&name, &permalink, &path.to_string_lossy())
            .await
            .map(|_| None::<Value>)
    } else {
        match ensure_project(&mut store, &name, &permalink, &path).await {
            Ok(project_id) => store
                .counts(project_id)
                .await
                .map(|counts| Some(serde_json::to_value(counts).unwrap_or(Value::Null))),
            Err(error) => Err(error),
        }
    };
    let indexed = match indexed {
        Ok(indexed) => indexed,
        Err(error) => {
            eprintln!("failed to register project: {error}");
            return ExitCode::FAILURE;
        }
    };

    if json {
        print_json(&json!({
            "name": name,
            "permalink": permalink,
            "path": path.to_string_lossy(),
            "index": index.to_string_lossy(),
            "action": if existed { "updated" } else { "added" },
            "indexed": indexed,
        }))
    } else {
        println!(
            "{} project '{name}' ({permalink}) -> {}",
            if existed { "updated" } else { "added" },
            path.display()
        );
        match indexed {
            Some(counts) => println!(
                "indexed {} entities / {} observations / {} relations into {}",
                counts["entities"],
                counts["observations"],
                counts["relations"],
                index.display()
            ),
            None => println!("not indexed (--no-index)"),
        }
        ExitCode::SUCCESS
    }
}

/// `project list` — every registered project with its index counts.
async fn project_list_command(args: ProjectListArgs) -> ExitCode {
    let ProjectListArgs { index, json } = args;
    let Some((index, _user)) = cli_index(index) else {
        return ExitCode::FAILURE;
    };
    let store = match Store::open(&index).await {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let projects = match store.projects().await {
        Ok(projects) => projects,
        Err(error) => {
            eprintln!("failed to read projects: {error}");
            return ExitCode::FAILURE;
        }
    };
    // Counts come per project; a failure there should not hide the project list.
    let mut rows: Vec<Value> = Vec::with_capacity(projects.len());
    for project in &projects {
        rows.push(match store.counts(project.id).await {
            Ok(counts) => json!({
                "name": project.name,
                "permalink": project.permalink,
                "path": project.path,
                "entities": counts.entities,
                "observations": counts.observations,
                "relations": counts.relations,
            }),
            Err(error) => json!({
                "name": project.name,
                "permalink": project.permalink,
                "path": project.path,
                "error": error.to_string(),
            }),
        });
    }

    if json {
        print_json(&json!({ "index": index.to_string_lossy(), "projects": rows }))
    } else {
        if rows.is_empty() {
            println!("no projects registered in {}", index.display());
            return ExitCode::SUCCESS;
        }
        let width = rows
            .iter()
            .filter_map(|row| row["name"].as_str())
            .map(str::len)
            .max()
            .unwrap_or(4)
            .max(4);
        println!(
            "{:<width$}  {:>8}  {:<24}  path",
            "name", "entities", "permalink"
        );
        for row in &rows {
            println!(
                "{:<width$}  {:>8}  {:<24}  {}",
                row["name"].as_str().unwrap_or_default(),
                row["entities"].as_i64().unwrap_or_default(),
                row["permalink"].as_str().unwrap_or_default(),
                row["path"].as_str().unwrap_or_default(),
            );
        }
        ExitCode::SUCCESS
    }
}

/// `project remove <name>` — unregister a project and drop its derived rows.
///
/// The vault is left alone: it is the source of truth, and deleting a user's notes is
/// not something a project-lifecycle command should ever do.
async fn project_remove_command(args: ProjectRemoveArgs) -> ExitCode {
    let ProjectRemoveArgs {
        identifier,
        index,
        json,
    } = args;
    let Some((index, _user)) = cli_index(index) else {
        return ExitCode::FAILURE;
    };
    let mut store = match Store::open(&index).await {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    // Accept either the name or the permalink: the reference's CLI takes the name, but
    // the permalink is what every other command here uses to identify a project.
    let permalink = match store.project_by_permalink(&identifier).await {
        Ok(Some(_)) => identifier.clone(),
        _ => generate_permalink(&identifier),
    };
    match store.delete_project(&permalink).await {
        Ok(true) => {
            if json {
                print_json(&json!({ "name": identifier, "permalink": permalink, "deleted": true }))
            } else {
                println!("removed project '{identifier}' ({permalink}); the vault was not touched");
                ExitCode::SUCCESS
            }
        }
        Ok(false) => {
            eprintln!("project not found: {identifier}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("failed to remove project: {error}");
            ExitCode::FAILURE
        }
    }
}

/// One line of the `doctor` report.
struct Check {
    /// Stable machine-readable name (`index`, `vault`, `onnx_runtime`, …).
    name: &'static str,
    /// `ok`, `warn`, or `fail`.
    status: &'static str,
    /// What was found, or what to do about it.
    detail: String,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: "ok",
            detail: detail.into(),
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: "warn",
            detail: detail.into(),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: "fail",
            detail: detail.into(),
        }
    }
}

/// `auto-memory doctor` — report what is usable on this machine.
///
/// The pieces that make semantic search work are the two that are *not* in the box: an
/// ONNX Runtime shared library and a fastembed model cache. Both are optional, both are
/// discovered at run time, and before this command existed there was no way to see what
/// the discovery had actually chosen — a missing model surfaced as a one-line error
/// buried in whichever command needed it.
///
/// Exit code is `1` when a check *failed* (an index or vault that was named and is
/// unusable, a named project that is not registered). Missing semantic search is a
/// warning, not a failure: text search, context, schema, and the MCP server do not need
/// it, and treating an optional capability as a broken installation would make the
/// command useless in exactly the setups it exists for.
async fn doctor_command(args: DoctorArgs) -> ExitCode {
    let DoctorArgs {
        index,
        vault,
        project,
        model_cache: cache_dir,
        onnx_runtime,
        json,
    } = args;
    let mut checks = Vec::new();

    // --- configuration ---------------------------------------------------------
    //
    // First, because every later check depends on the values it supplies: "which
    // index is it even looking at" is the question a wrong-path report usually
    // answers, and the origin is the part the user cannot see from the flags.
    let user = load_user_config();
    checks.push(match &user {
        UserConfig::Malformed { path, error } => {
            Check::fail("config", format!("{} is unusable: {error}", path.display()))
        }
        other => Check::ok("config", other.describe()),
    });
    let resolved = resolve_index(index, index_from_environment(), &user);

    // --- index -----------------------------------------------------------------
    //
    // `Store::open` creates a missing index, so `doctor` must not call it on a path that
    // does not exist: a diagnostic that writes is a diagnostic you cannot trust.
    let index = resolved.value;
    let index_label = format!("{} (from {})", index.display(), resolved.origin);
    let store = if index.exists() {
        match Store::open(&index).await {
            Ok(store) => {
                checks.push(Check::ok("index", index_label));
                Some(store)
            }
            Err(error) => {
                checks.push(Check::fail(
                    "index",
                    format!("{}: {error}", index.display()),
                ));
                None
            }
        }
    } else {
        checks.push(Check::warn(
            "index",
            format!(
                "{index_label} does not exist yet — run `auto-memory project add <name> <path>` \
                 or `auto-memory reindex --vault <dir>`"
            ),
        ));
        None
    };
    if let Some(store) = &store {
        match store.schema_version().await {
            Ok(version) => checks.push(Check::ok(
                "schema",
                format!(
                    "version {version} (this build writes {})",
                    auto_memory::storage::schema::SCHEMA_VERSION
                ),
            )),
            Err(error) => checks.push(Check::fail("schema", error.to_string())),
        }
        match store.projects().await {
            Ok(projects) if projects.is_empty() => checks.push(Check::warn(
                "projects",
                "none registered — run `auto-memory project add <name> <path>`",
            )),
            Ok(projects) => checks.push(Check::ok(
                "projects",
                format!(
                    "{}: {}",
                    projects.len(),
                    projects
                        .iter()
                        .map(|project| project.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
            Err(error) => checks.push(Check::fail("projects", error.to_string())),
        }
    }

    let suggested_project = if project.is_some() {
        project.clone()
    } else if let Some(resolved) = resolve_project(None, None, &user) {
        Some(resolved.value)
    } else if let Some(store) = &store {
        match store.projects().await {
            Ok(projects) if projects.len() == 1 => projects.first().map(|row| row.name.clone()),
            _ => None,
        }
    } else {
        None
    };
    checks.push(codex_mcp_check(
        &index,
        vault.as_deref(),
        suggested_project.as_deref(),
    ));
    checks.push(codex_hooks_check());

    // --- vault and project (only when named) -----------------------------------
    if let Some(vault) = vault {
        if vault.is_dir() {
            let mut files = Vec::new();
            collect_files(&vault, &mut files);
            let markdown = files
                .iter()
                .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
                .count();
            if markdown == 0 {
                checks.push(Check::warn(
                    "vault",
                    format!("{} holds no markdown files", vault.display()),
                ));
            } else {
                checks.push(Check::ok(
                    "vault",
                    format!("{}: {markdown} markdown files", vault.display()),
                ));
            }
        } else {
            checks.push(Check::fail(
                "vault",
                format!("{} is not a directory", vault.display()),
            ));
        }
    }
    if let (Some(permalink), Some(store)) = (project.as_deref(), store.as_ref()) {
        match store
            .project_by_permalink(&generate_permalink(permalink))
            .await
        {
            Ok(Some(project)) => match store.counts(project.id).await {
                Ok(counts) if counts.entities == 0 => checks.push(Check::warn(
                    "project",
                    format!("'{}' is registered but its index is empty", project.name),
                )),
                Ok(counts) => checks.push(Check::ok(
                    "project",
                    format!(
                        "'{}': {} entities / {} observations / {} relations",
                        project.name, counts.entities, counts.observations, counts.relations
                    ),
                )),
                Err(error) => checks.push(Check::fail("project", error.to_string())),
            },
            Ok(None) => checks.push(Check::fail(
                "project",
                format!("'{permalink}' is not registered in {}", index.display()),
            )),
            Err(error) => checks.push(Check::fail("project", error.to_string())),
        }
    }

    // --- semantic search --------------------------------------------------------
    match resolve_onnx_runtime(onnx_runtime.as_deref()) {
        Some(path) => checks.push(Check::ok("onnx_runtime", format!("{}", path.display()))),
        None => {
            let searched: Vec<String> = onnx_runtime_search_paths()
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            checks.push(Check::warn(
                "onnx_runtime",
                format!(
                    "not found — semantic search is unavailable. Set {} to the library, or \
                     searched: {}",
                    auto_memory::runtime::ONNX_RUNTIME_ENV,
                    searched.join(", ")
                ),
            ));
        }
    }

    let cache = model_cache(cache_dir.as_deref());
    match reference_model_dir(&cache) {
        Some(model_dir) => checks.push(Check::ok(
            "model_cache",
            format!("{} ({})", cache.display(), model_dir.display()),
        )),
        None => {
            let roots: Vec<String> = model_cache_search_paths()
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            checks.push(Check::warn(
                "model_cache",
                format!(
                    "no {REFERENCE_MODEL_REPO} snapshot in {} — point --model-cache (or {}) at \
                     one, or searched: {}",
                    cache.display(),
                    auto_memory::runtime::MODEL_CACHE_ENV,
                    roots.join(", ")
                ),
            ));
        }
    }
    // The reranker is off by default, so a missing model is information, not a warning
    // worth acting on.
    if auto_memory::runtime::reference_rerank_dir(&cache).is_some() {
        checks.push(Check::ok("reranker_model", format!("{}", cache.display())));
    } else {
        checks.push(Check::warn(
            "reranker_model",
            "not installed (only needed for --reranker)",
        ));
    }

    // --- report -----------------------------------------------------------------
    let failed = checks.iter().any(|check| check.status == "fail");
    if json {
        let payload = json!({
            "ok": !failed,
            "checks": checks
                .iter()
                .map(|check| json!({
                    "name": check.name,
                    "status": check.status,
                    "detail": check.detail,
                }))
                .collect::<Vec<_>>(),
        });
        // The exit code has to survive the JSON surface too: `--json` is what a script
        // consumes, and it reads the status before it reads the body.
        let printed = print_json(&payload);
        return if failed {
            eprintln!("auto-memory doctor: at least one check failed");
            ExitCode::FAILURE
        } else {
            printed
        };
    }
    let width = checks
        .iter()
        .map(|check| check.name.len())
        .max()
        .unwrap_or(4);
    for check in &checks {
        let marker = match check.status {
            "ok" => "ok  ",
            "warn" => "warn",
            _ => "FAIL",
        };
        println!("{marker}  {:<width$}  {}", check.name, check.detail);
    }
    if failed {
        eprintln!("auto-memory doctor: at least one check failed");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Report whether Codex has the Auto Memory MCP server registered.
///
/// This is advisory: Auto Memory can be used by other MCP clients, and doctor must
/// never modify a user's Codex configuration. The executable is intentionally checked
/// separately from the server name: `auto-memory-rs` is the server/project identity,
/// while the installed executable is named `auto-memory`.
fn codex_mcp_check(index: &Path, vault: Option<&Path>, project: Option<&str>) -> Check {
    let config = codex_config_path();
    if !config.is_file() {
        return Check::warn(
            "codex_mcp",
            format!(
                "not configured at {}; run `{}`",
                config.display(),
                codex_mcp_add_command(index, vault, project)
            ),
        );
    }

    let text = match std::fs::read_to_string(&config) {
        Ok(text) => text,
        Err(error) => {
            return Check::warn(
                "codex_mcp",
                format!("cannot read {}: {error}", config.display()),
            );
        }
    };
    let document = match text.parse::<toml::Value>() {
        Ok(document) => document,
        Err(error) => {
            return Check::warn(
                "codex_mcp",
                format!("cannot parse {}: {error}", config.display()),
            );
        }
    };
    let servers = document.get("mcp_servers").and_then(toml::Value::as_table);
    let server = servers.and_then(|servers| servers.get("auto-memory-rs"));
    let Some(server) = server.and_then(toml::Value::as_table) else {
        let alternate = servers
            .and_then(|servers| servers.get("auto-memory"))
            .is_some();
        let hint = if alternate {
            "found `mcp_servers.auto-memory`; use the canonical server name `auto-memory-rs`"
        } else {
            "add `[mcp_servers.auto-memory-rs]`"
        };
        return Check::warn(
            "codex_mcp",
            format!(
                "Auto Memory MCP is not registered in {}; {hint}. Run `{}`",
                config.display(),
                codex_mcp_add_command(index, vault, project)
            ),
        );
    };

    let Some(command) = server.get("command").and_then(toml::Value::as_str) else {
        return Check::warn(
            "codex_mcp",
            format!(
                "`mcp_servers.auto-memory-rs` has no command in {}",
                config.display()
            ),
        );
    };
    let executable = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    if executable != "auto-memory" {
        return Check::warn(
            "codex_mcp",
            format!(
                "`mcp_servers.auto-memory-rs.command` is `{command}`; use the `auto-memory` binary"
            ),
        );
    }
    Check::ok(
        "codex_mcp",
        format!(
            "mcp_servers.auto-memory-rs in {} (command: {command})",
            config.display()
        ),
    )
}

fn codex_config_path() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
        .join("config.toml")
}

fn codex_hooks_check() -> Check {
    let codex_home = codex_config_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".codex"));
    let config = codex_home.join("config.toml");
    let plugin_entries = codex_plugin_entries(&config);
    let mut configured = Vec::new();
    let mut scopes = vec![codex_home.clone()];
    if let Ok(cwd) = std::env::current_dir() {
        scopes.extend(cwd.ancestors().map(|path| path.join(".codex")));
    }
    for scope in scopes {
        for path in [scope.join("hooks.json"), scope.join("config.toml")] {
            if file_configures_auto_memory_hook(&path) {
                configured.push(path);
            }
        }
    }

    let mut plugin_hooks = Vec::new();
    let cache = codex_home.join("plugins/cache");
    for (entry_name, enabled) in &plugin_entries {
        if !enabled {
            continue;
        }
        let Some((plugin_name, marketplace_name)) = entry_name.split_once('@') else {
            continue;
        };
        if plugin_name != "auto-memory-rs" {
            continue;
        }
        let plugin_cache = cache.join(marketplace_name).join(plugin_name);
        if let Ok(versions) = std::fs::read_dir(plugin_cache) {
            for version in versions.flatten() {
                let hooks = version.path().join("hooks/hooks.json");
                if hooks.is_file() {
                    plugin_hooks.push(hooks);
                }
            }
        }
    }

    if !plugin_entries.is_empty() {
        let enabled_entries = plugin_entries
            .iter()
            .filter(|(_, enabled)| *enabled)
            .collect::<Vec<_>>();
        let entry_details = plugin_entries
            .iter()
            .map(|(name, enabled)| format!("{name} (enabled = {enabled})"))
            .collect::<Vec<_>>()
            .join(", ");
        if enabled_entries.is_empty() && configured.is_empty() {
            return Check::warn(
                "codex_hooks",
                format!(
                    "Auto Memory Codex plugin is registered but disabled in {}: {entry_details}. Enable it from Codex `/plugins`, or set `[plugins.\"auto-memory-rs@auto-memory\"]` to `enabled = true`.",
                    config.display()
                ),
            );
        }
        if !enabled_entries.is_empty() && plugin_hooks.is_empty() && configured.is_empty() {
            return Check::warn(
                "codex_hooks",
                format!(
                    "Auto Memory Codex plugin is enabled in {} ({entry_details}), but its installed hooks/hooks.json was not found under {}. Reinstall it with: {}",
                    config.display(),
                    cache.display(),
                    codex_plugin_install_hint()
                ),
            );
        }
    } else if plugin_hooks.is_empty() && configured.is_empty() {
        return Check::warn(
            "codex_hooks",
            format!(
                "Auto Memory Codex plugin is not registered in {}; install it with: {}. Then enable `[plugins.\"auto-memory-rs@auto-memory\"]` with `enabled = true` (or install it from Codex `/plugins`). User/project hook files and plugin cache were also checked.",
                config.display(),
                codex_plugin_install_hint()
            ),
        );
    }

    if !configured.is_empty() || !plugin_hooks.is_empty() {
        let paths = configured
            .iter()
            .chain(plugin_hooks.iter())
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Check::ok(
            "codex_hooks",
            format!(
                "Auto Memory hook definitions found: {paths}. Plugin entry: {}. Declares SessionStart briefing/checkpoint prompt and PreCompact; Codex hook trust must still be approved.",
                if plugin_entries.is_empty() {
                    "not registered (hooks found from another source)".to_owned()
                } else {
                    plugin_entries
                        .iter()
                        .map(|(name, enabled)| format!("{name} enabled={enabled}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        );
    }

    Check::warn(
        "codex_hooks",
        format!(
            "no Auto Memory hooks found in user/project hooks.json or config.toml, or the installed plugin cache under {}. Install/enable the `auto-memory-rs` Codex plugin and review its hook trust; SessionStart supplies the brief/checkpoint prompt. Codex ignores PreCompact stdout.",
            cache.display()
        ),
    )
}

fn codex_plugin_entries(config: &Path) -> Vec<(String, bool)> {
    let Ok(contents) = std::fs::read_to_string(config) else {
        return Vec::new();
    };
    let Ok(document) = contents.parse::<toml::Value>() else {
        return Vec::new();
    };
    document
        .get("plugins")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flat_map(|plugins| plugins.iter())
        .filter(|(name, _)| name.split('@').next() == Some("auto-memory-rs"))
        .map(|(name, config)| {
            let enabled = config
                .get("enabled")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            (name.clone(), enabled)
        })
        .collect()
}

fn codex_plugin_install_hint() -> String {
    let root = std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| ".".to_owned());
    format!(
        "`codex plugin marketplace add {root}`; then run `/plugins` in Codex and install `auto-memory-rs` from the `auto-memory` marketplace"
    )
}

fn file_configures_auto_memory_hook(path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    contents.contains("auto-memory hook")
        || contents.contains("session_start.sh")
        || contents.contains("pre_compact.sh")
}

fn codex_mcp_add_command(index: &Path, vault: Option<&Path>, project: Option<&str>) -> String {
    let mut command = format!(
        "codex mcp add auto-memory-rs -- auto-memory mcp --index {}",
        index.display()
    );
    if let Some(vault) = vault {
        command.push_str(&format!(" --vault {}", vault.display()));
    }
    if let Some(project) = project {
        command.push_str(&format!(" --project {project}"));
    }
    command
}

/// Every file under `directory`, recursively.
fn collect_files(directory: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn parse_command(path: &Path) -> ExitCode {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            eprintln!("failed to read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    match auto_memory::markdown::parse_document(path, &content) {
        Ok(document) => print_json(&document),
        Err(error) => {
            eprintln!("failed to parse {}: {error}", path.display());
            ExitCode::FAILURE
        }
    }
}

async fn reindex_command(args: ReindexArgs) -> ExitCode {
    let ReindexArgs {
        vault,
        index,
        project,
        full,
        embeddings,
        embed,
    } = args;
    let Some((index, user)) = cli_index(index) else {
        return ExitCode::FAILURE;
    };
    let mut store = match Store::open(&index).await {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let (name, permalink, vault) = match resolve_project_target(&store, &user, project, vault).await
    {
        Ok(target) => target,
        Err(message) => return usage(&message),
    };
    let project_id = match store
        .upsert_project(&name, &permalink, &vault.to_string_lossy())
        .await
    {
        Ok(id) => id,
        Err(error) => {
            eprintln!("failed to register project: {error}");
            return ExitCode::FAILURE;
        }
    };
    let index_options = IndexOptions::new(&permalink);
    let mut service = IndexService::new(&mut store, project_id, &vault, index_options);

    // `reindex --embeddings` refreshes only the vector index; the markdown index
    // is reconciled first so an embeddings-only run still sees the vault (the
    // reference runs `reindex --full --search` followed by `--embeddings`).
    if embeddings {
        if let Err(error) = service.reconcile().await {
            eprintln!("incremental reindex failed: {error}");
            return ExitCode::FAILURE;
        }
        let provider = match embedding_provider(&embed) {
            Ok(provider) => provider,
            Err(message) => {
                eprintln!("Error: {message}");
                return ExitCode::FAILURE;
            }
        };
        return match service.reindex_embeddings(provider.as_ref()).await {
            Ok(report) => print_json(&report),
            Err(error) => {
                eprintln!("embedding reindex failed: {error}");
                ExitCode::FAILURE
            }
        };
    }

    if full {
        match service.full_rebuild().await {
            Ok(report) => print_json(&report),
            Err(error) => {
                eprintln!("full reindex failed: {error}");
                ExitCode::FAILURE
            }
        }
    } else {
        match service.reconcile().await {
            Ok(report) => print_json(&report),
            Err(error) => {
                eprintln!("incremental reindex failed: {error}");
                ExitCode::FAILURE
            }
        }
    }
}

/// `mcp --vault <dir> --index <db>`: serve the MCP tools on stdio or Streamable HTTP.
///
/// stdout carries protocol frames only; diagnostics go to stderr, so a client can
/// pipe the process directly. The stdio transport is the async one (`serve_async`),
/// so the signal handler can stop the loop between frames instead of killing a
/// request mid-write. `--http` swaps in the Streamable HTTP transport (served by
/// `rmcp` + `axum`), which stays up until the same shutdown signal arrives.
async fn mcp_command(args: McpArgs) -> ExitCode {
    let McpArgs {
        vault,
        index,
        project,
        http,
        host,
        port,
        path,
        read_only,
        embed,
        rerank,
    } = args;
    let Some((index, user)) = cli_index(index) else {
        return ExitCode::FAILURE;
    };
    let mut store = match Store::open(&index).await {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let (name, permalink, vault) = match resolve_project_target(&store, &user, project, vault).await
    {
        Ok(target) => target,
        Err(message) => return usage(&message),
    };
    let project_id = match ensure_project(&mut store, &name, &permalink, &vault).await {
        Ok(project_id) => project_id,
        Err(error) => {
            eprintln!("failed to prepare the project: {error}");
            return ExitCode::FAILURE;
        }
    };

    let external_id = match store.project_by_permalink(&permalink).await {
        Ok(Some(project)) => project.external_id,
        Ok(None) => {
            eprintln!("project not registered: {permalink}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("failed to read the project row: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        project = %name,
        permalink = %permalink,
        vault = %vault.display(),
        index = %index.display(),
        read_only,
        transport = if http { "http" } else { "stdio" },
        "mcp server starting"
    );
    // The providers are built once and handed to whichever transport runs, so the two
    // transports share one ONNX runtime instead of loading the model twice.
    // Semantic `search_type`s need the embedding runtime; without these flags the tool
    // reports that semantic search is unavailable instead of falling back to text.
    let provider = if embed.embedding_fixture.is_some()
        || embed.model_cache.is_some()
        || embed.onnx_runtime.is_some()
    {
        match embedding_provider(&embed) {
            Ok(provider) => Some(provider),
            Err(message) => {
                eprintln!("Error: {message}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    // The reranker is opt-in, like the reference's `reranker_enabled=False` default.
    let reranker = if rerank.reranker || rerank.reranker_fixture.is_some() {
        match rerank_provider(&rerank, &embed) {
            Ok(provider) => Some(provider),
            Err(message) => {
                eprintln!("Error: {message}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let reranker_candidates = rerank.reranker_candidates;
    let reranker_max_chars = rerank.reranker_max_chars;

    if http {
        let host = host.as_deref().unwrap_or(DEFAULT_HTTP_HOST);
        let port = port.unwrap_or(DEFAULT_HTTP_PORT);
        let path = path.as_deref().unwrap_or(DEFAULT_HTTP_PATH);
        let mut server = HttpServer::new(
            Arc::new(Mutex::new(store)),
            project_id,
            &name,
            &external_id,
            &permalink,
            &vault,
        );
        if read_only {
            server = server.with_read_only(true);
        }
        if let Some(provider) = provider {
            server = server.with_provider(provider);
        }
        if let Some(reranker) = reranker {
            server = server.with_reranker(reranker);
            if let Some(candidates) = reranker_candidates {
                server = server.with_reranker_candidates(candidates);
            }
            if let Some(max_chars) = reranker_max_chars {
                server = server.with_reranker_max_document_chars(max_chars);
            }
        }
        return match serve(server, host, port, path, shutdown_signal()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("mcp http server failed: {error}");
                ExitCode::FAILURE
            }
        };
    }

    let mut server = McpServer::new(
        &mut store,
        project_id,
        &name,
        &external_id,
        &permalink,
        &vault,
    );
    if read_only {
        server = server.with_read_only(true);
    }
    if let Some(provider) = provider {
        server = server.with_provider(provider);
    }
    if let Some(reranker) = reranker {
        server = server.with_reranker(reranker);
        if let Some(candidates) = reranker_candidates {
            server = server.with_reranker_candidates(candidates);
        }
        if let Some(max_chars) = reranker_max_chars {
            server = server.with_reranker_max_document_chars(max_chars);
        }
    }
    // `tokio::io::Stdin` is `AsyncRead`; the line framing needs a buffer on top.
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    match server.serve_async(stdin, stdout, shutdown_signal()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mcp server failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve on Ctrl-C or SIGTERM.
///
/// Used as the cooperative stop for both event loops: `mcp` finishes the request in
/// flight (or stops between frames) and exits cleanly, and `watch` breaks the loop,
/// applies the pending debounce window, and reports what it wrote. A signal that
/// cannot be installed must not take the server down, so the error is only reported.
///
/// SIGTERM matters for the daemon case: it is what `systemctl stop`, `docker stop`
/// and a plain `kill` send, and without a handler the process dies mid-window —
/// pending paths are not flushed and the batch count never reaches stdout.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        if let Err(error) = result {
                            eprintln!("auto-memory: could not listen for Ctrl-C: {error}");
                            // Without a signal there is nothing to wait for; park so the
                            // caller's loop keeps running and the default disposition applies.
                            std::future::pending::<()>().await;
                        }
                        tracing::info!("Ctrl-C received, stopping");
                    }
                    _ = terminate.recv() => tracing::info!("SIGTERM received, stopping"),
                }
                return;
            }
            Err(error) => tracing::warn!(%error, "could not listen for SIGTERM"),
        }
    }
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("auto-memory: could not listen for Ctrl-C: {error}");
        std::future::pending::<()>().await;
    }
}

/// `watch --vault <dir> --index <db>`: keep the index in sync with the vault.
///
/// Mirrors the reference watch service: an initial reconcile, then a debounced loop
/// (`--window-ms`, default 1000) over `notify` events. `--once` applies a single
/// batch and exits, which is handy for scripts and tests. The loop runs on the async
/// runtime, so Ctrl-C stops it *and* flushes the pending window before the process
/// exits, instead of dropping whatever was still debouncing.
async fn watch_command(args: WatchArgs) -> ExitCode {
    let WatchArgs {
        vault,
        index,
        project,
        window_ms,
        once,
        embeddings,
    } = args;
    // `--embeddings` is declared (hidden) only to reach this refusal: an operator would
    // pass it expecting vectors to follow the vault, and a markdown-only run that looks
    // like a semantic one is worse than an error naming the command that does the work.
    if embeddings {
        return usage(
            "watch does not take --embeddings: it only keeps the markdown index current — \
             run `auto-memory reindex --vault <dir> --index <db> --embeddings` for the vectors",
        );
    }
    let Some((index, user)) = cli_index(index) else {
        return ExitCode::FAILURE;
    };
    let mut store = match Store::open(&index).await {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let (name, permalink, vault) = match resolve_project_target(&store, &user, project, vault).await
    {
        Ok(target) => target,
        Err(message) => return usage(&message),
    };
    let project_id = match store
        .upsert_project(&name, &permalink, &vault.to_string_lossy())
        .await
    {
        Ok(id) => id,
        Err(error) => {
            eprintln!("failed to register project: {error}");
            return ExitCode::FAILURE;
        }
    };
    let window = window_ms.map_or(DEFAULT_WATCH_WINDOW, Duration::from_millis);

    let mut service = IndexService::new(
        &mut store,
        project_id,
        &vault,
        IndexOptions::new(&permalink),
    );
    tracing::info!(
        vault = %vault.display(),
        index = %index.display(),
        project = %permalink,
        window_ms = window.as_millis() as u64,
        once,
        "watching the vault"
    );
    // The reconcile is a full vault pass (read, parse, write); like the per-batch
    // indexing below it runs off the reactor.
    let reconciled = match service.reconcile().await {
        Ok(report) => report,
        Err(error) => {
            eprintln!("initial reconcile failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        added = reconciled.added,
        updated = reconciled.updated,
        unchanged = reconciled.unchanged,
        skipped = reconciled.skipped,
        removed = reconciled.removed,
        relations_resolved = reconciled.relations_resolved,
        "initial reconcile finished"
    );
    let watcher = VaultWatcher::new(service, &vault).with_window(window);
    if once {
        // The one-shot path is a bounded blocking collect; keep it off the reactor.
        return match watch_once(watcher, window).await {
            Ok(applied) => {
                log_batch("one-shot batch", &applied);
                print_json(&serde_json::json!({
                    "reconciled": reconciled,
                    "applied": applied,
                }))
            }
            Err(error) => {
                eprintln!("watch failed: {error}");
                ExitCode::FAILURE
            }
        };
    }
    match watch_vault(watcher, shutdown_signal()).await {
        Ok(batches) => {
            tracing::info!(batches, "watch stopped");
            print_json(&serde_json::json!({ "batches": batches }))
        }
        Err(error) => {
            eprintln!("watch failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Log one applied batch, using the library's formatter so `watch --once` and the
/// daemon report identically.
fn log_batch(label: &str, report: &auto_memory::indexing::WatchReport) {
    auto_memory::indexing::log_watch_report(label, report);
}
async fn status_command(args: StatusArgs) -> ExitCode {
    let StatusArgs {
        index,
        project: permalink,
    } = args;
    let Some((index, _user)) = cli_index(index) else {
        return ExitCode::FAILURE;
    };
    let store = match Store::open(&index).await {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let project = match store.project_by_permalink(&permalink).await {
        Ok(Some(project)) => project,
        Ok(None) => {
            eprintln!("project not found: {permalink}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("failed to read project: {error}");
            return ExitCode::FAILURE;
        }
    };
    match store.counts(project.id).await {
        Ok(counts) => print_json(&counts),
        Err(error) => {
            eprintln!("failed to read counts: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `context <url>`: resolve a `memory://` URL and print its graph context.
///
/// Defaults mirror the reference CLI (`--depth 1`, `--timeframe 7d`, page 1,
/// page size 10, max related 10) and errors print as `Error: …` with exit code 1.
async fn context_command(args: ContextArgs) -> ExitCode {
    let ContextArgs {
        url,
        index,
        project: permalink,
        depth,
        timeframe: window,
        page,
        page_size,
        max_related,
        plain,
        json: _json,
    } = args;
    let Some((index, _user)) = cli_index(index) else {
        return ExitCode::FAILURE;
    };

    let context = match (|| {
        let timeframe = window.as_deref().unwrap_or("7d");
        let since = timeframe::parse_timeframe(timeframe)?;
        let options = ContextOptions {
            depth: depth.unwrap_or(1),
            max_related: max_related.unwrap_or(10),
            page: page.unwrap_or(1),
            page_size: page_size.unwrap_or(10),
            since: Some(since),
        };
        options.validate()?;
        Ok::<_, Box<dyn std::error::Error>>(options)
    })() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("Error: {error}");
            return ExitCode::FAILURE;
        }
    };

    let store = match Store::open(&index).await {
        Ok(store) => store,
        Err(error) => {
            eprintln!("Error: failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let project = match store.project_by_permalink(&permalink).await {
        Ok(Some(project)) => project,
        Ok(None) => {
            eprintln!("Error: project not found: {permalink}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("Error: failed to read project: {error}");
            return ExitCode::FAILURE;
        }
    };

    let context_options = context;
    let graph = match build_context(&store, project.id, &url, &context_options).await {
        Ok(graph) => graph,
        Err(error) => {
            eprintln!("Error: {error}");
            return ExitCode::FAILURE;
        }
    };

    let response = match serde_json::to_value(&graph) {
        Ok(response) => response,
        Err(error) => {
            eprintln!("Error: failed to serialize output: {error}");
            return ExitCode::FAILURE;
        }
    };
    if plain {
        println!("{}", render_plain(&response));
        return ExitCode::SUCCESS;
    }
    match serde_json::to_string_pretty(&response) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Error: failed to serialize output: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn search_command(args: SearchArgs) -> ExitCode {
    let SearchArgs {
        query,
        index,
        project: permalink,
        title,
        note_type,
        tag,
        status,
        category,
        entity_type,
        permalink: permalink_filter,
        meta,
        after_date,
        page,
        page_size,
        vector,
        hybrid,
        min_similarity,
        embed,
        rerank,
    } = args;
    let Some((index, _user)) = cli_index(index) else {
        return ExitCode::FAILURE;
    };
    let store = match Store::open(&index).await {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let project = match store.project_by_permalink(&permalink).await {
        Ok(Some(project)) => project,
        Ok(None) => {
            eprintln!("project not found: {permalink}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("failed to read project: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut search = TextSearchOptions {
        query: (!query.is_empty()).then(|| query.join(" ")),
        page: page.unwrap_or(1),
        page_size: page_size.unwrap_or(10),
        ..TextSearchOptions::default()
    };
    if let Some(value) = title {
        search.title = Some(value);
    }
    if let Some(value) = note_type {
        search.note_types.push(value);
    }
    if let Some(value) = tag {
        search.tags.push(value);
    }
    if let Some(value) = category {
        search.categories.push(value);
    }
    if let Some(value) = status {
        search.status = Some(value);
    }
    if let Some(value) = entity_type {
        match value.parse::<SearchItemType>() {
            Ok(item_type) => search.entity_types = vec![item_type],
            Err(_) => return usage(&format!("unknown entity type: {value}")),
        }
    } else {
        // The reference's implicit default: a category filter scopes the search to
        // observation rows, because categories only exist there.
        search.entity_types = auto_memory::search::default_entity_types(&search.categories);
    }
    if let Some(value) = permalink_filter {
        if value.contains('*') {
            search.permalink_match = Some(value);
        } else {
            search.permalink = Some(value);
        }
    }
    if let Some(value) = meta {
        let Some((key, value)) = value.split_once('=') else {
            return usage("--meta expects key=value");
        };
        // `note_type` is the model column name; the frontmatter key is `type` (the
        // reference aliases it the same way on the MCP path).
        let key = if key == "note_type" { "type" } else { key };
        search
            .metadata_filters
            .insert(key.to_owned(), value.to_owned());
    }
    if let Some(value) = after_date {
        // Search bounds go through `dateparser`, not the timeframe parser the context
        // tools use; see `domain::dateparser` for why the two disagree.
        let Some(bound) = auto_memory::domain::dateparser::parse_after_date(&value) else {
            return usage(&format!("--after-date is not a date or window: {value}"));
        };
        search.after_date = Some(bound);
    }

    // Semantic modes reuse the filter set and embed the query locally.
    if vector || hybrid {
        let provider = match embedding_provider(&embed) {
            Ok(provider) => provider,
            Err(message) => {
                eprintln!("Error: {message}");
                return ExitCode::FAILURE;
            }
        };
        let Some(query_text) = search.query.clone() else {
            return usage("semantic search requires a query");
        };
        let query_vector = match provider.embed_query(&query_text) {
            Ok(vector) => vector,
            Err(error) => {
                eprintln!("Error: failed to embed the query: {error}");
                return ExitCode::FAILURE;
            }
        };
        // The reranker is opt-in (`--reranker`, or a fixture), like the reference's
        // `reranker_enabled=False` default.
        let reranker = if rerank.reranker || rerank.reranker_fixture.is_some() {
            match rerank_provider(&rerank, &embed) {
                Ok(provider) => Some(provider),
                Err(message) => {
                    eprintln!("Error: {message}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            None
        };
        let rerank_request = reranker.as_ref().map(|provider| RerankRequest {
            query: &query_text,
            provider: provider.as_ref(),
            candidates: rerank
                .reranker_candidates
                .unwrap_or(DEFAULT_RERANKER_CANDIDATES),
            max_document_chars: rerank
                .reranker_max_chars
                .unwrap_or(DEFAULT_RERANKER_MAX_DOCUMENT_CHARS),
        });

        let vector_options = VectorSearchOptions {
            min_similarity: min_similarity.unwrap_or(0.55),
            page: search.page,
            page_size: search.page_size,
            entity_types: search.entity_types.clone(),
            // The semantic legs reuse the text filters: the FTS half applies them
            // natively and the vector half intersects against a filter-only scan.
            permalink: search.permalink.clone(),
            permalink_match: search.permalink_match.clone(),
            title: search.title.clone(),
            note_types: search.note_types.clone(),
            categories: search.categories.clone(),
            tags: search.tags.clone(),
            status: search.status.clone(),
            metadata_filters: search.metadata_filters.clone(),
            after_date: search.after_date.clone(),
        };
        let page = if hybrid {
            search_hybrid(
                &store,
                project.id,
                &query_text,
                &query_vector,
                provider.model_name(),
                &vector_options,
                rerank_request.as_ref(),
            )
            .await
        } else {
            search_vector(
                &store,
                project.id,
                &query_vector,
                provider.model_name(),
                &vector_options,
                rerank_request.as_ref(),
            )
            .await
        };
        return match page {
            Ok(page) => print_json(&serde_json::json!({
                "results": page.results,
                "total": page.total,
                "total_is_exact": page.total_is_exact,
                "has_more": page.has_more,
                "current_page": page.current_page,
                "page_size": page.page_size,
            })),
            Err(error) => {
                eprintln!("Error: search failed: {error}");
                ExitCode::FAILURE
            }
        };
    }

    match store.search_text(project.id, &search).await {
        Ok(page) => print_json(&serde_json::json!({
            "results": page.results,
            "total": page.total,
            "total_is_exact": page.total_is_exact,
            "has_more": page.has_more,
            "current_page": page.current_page,
            "page_size": page.page_size,
        })),
        Err(error) => {
            eprintln!("search failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Build the embedding provider selected by the CLI flags.
///
/// `--embedding-fixture <json>` replays captured reference vectors (offline, no ONNX
/// runtime needed); otherwise the reference ONNX model is loaded from `--model-cache`.
fn embedding_provider(
    args: &EmbeddingArgs,
) -> Result<Box<dyn EmbeddingProvider + Send + Sync>, String> {
    if let Some(path) = &args.embedding_fixture {
        let json = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let provider = FixtureEmbeddingProvider::from_json(&json)
            .map_err(|error| format!("failed to load {}: {error}", path.display()))?;
        return Ok(Box::new(provider));
    }
    let cache = model_cache(args.model_cache.as_deref());
    let runtime = resolve_onnx_runtime(args.onnx_runtime.as_deref());
    let provider = OnnxEmbeddingProvider::load_from_cache(&cache, runtime.as_deref())
        .map_err(|error| format!("failed to load the embedding model: {error}"))?;
    Ok(Box::new(provider))
}

/// Build the cross-encoder reranker when the caller asked for one.
///
/// `--reranker-fixture FILE` supplies deterministic scores (a JSON map of query →
/// document → relevance) for tests and offline runs; otherwise the reference model is
/// loaded from the shared fastembed cache.
fn rerank_provider(
    args: &RerankArgs,
    embed: &EmbeddingArgs,
) -> Result<Box<dyn RerankProvider + Send + Sync>, String> {
    if let Some(path) = &args.reranker_fixture {
        let json = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let provider = FixtureRerankProvider::from_json(&json)
            .map_err(|error| format!("failed to load {}: {error}", path.display()))?;
        return Ok(Box::new(provider));
    }
    let cache = model_cache(embed.model_cache.as_deref());
    let runtime = resolve_onnx_runtime(embed.onnx_runtime.as_deref());
    let provider = OnnxRerankProvider::load_from_cache(&cache, runtime.as_deref())
        .map_err(|error| format!("failed to load the reranker: {error}"))?;
    Ok(Box::new(provider))
}

/// The model cache to read the embedding and reranker models from.
///
/// `--model-cache` wins, then `AUTO_MEMORY_MODEL_CACHE`, then the discovered default
/// (see `default_model_cache`) — which prefers the reference installation's cache so an
/// existing setup needs no download, and otherwise names this port's own directory.
fn model_cache(explicit: Option<&Path>) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os(MODEL_CACHE_ENV).map(PathBuf::from))
        .unwrap_or_else(default_model_cache)
}

fn print_json<T: serde::Serialize>(value: &T) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("failed to serialize output: {error}");
            ExitCode::FAILURE
        }
    }
}
