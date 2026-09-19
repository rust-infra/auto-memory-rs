//! `auto-memory` command-line entry point.
//!
//! Phase 6 surface:
//! - `parse <path>` — serialize the parse layer (compared with the reference parser).
//! - `reindex --vault <dir> --index <db> [--project <name>] [--full]` — rebuild or
//!   reconcile the derived index.
//! - `status --index <db> --project <permalink>` — print index counts.
//! - `context <memory://url>` — build graph context (JSON, or the reference's
//!   undecorated `--plain` outline).

use std::collections::BTreeMap;
use std::future::Future;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use auto_memory::adapters::mcp::http::{
    DEFAULT_HTTP_HOST, DEFAULT_HTTP_PATH, DEFAULT_HTTP_PORT, HttpServer, serve,
};
use auto_memory::adapters::mcp::{McpServer, ensure_project};
use auto_memory::application::context::{ContextOptions, build_context, render_plain};
use auto_memory::application::schema::SchemaService;
use auto_memory::application::schema_tools;
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
    DEFAULT_RERANKER_CANDIDATES, DEFAULT_RERANKER_MAX_DOCUMENT_CHARS, OnnxEmbeddingProvider,
    OnnxRerankProvider, RerankProvider, RerankRequest, find_onnx_runtime,
};
use auto_memory::search::embedding::{EmbeddingProvider, FixtureEmbeddingProvider};
use auto_memory::search::rerank::FixtureRerankProvider;
use auto_memory::search::text::TextSearchOptions;
use auto_memory::search::vector::{VectorSearchOptions, search_hybrid, search_vector};
use auto_memory::storage::Store;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-v") => {
            println!("auto-memory {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("parse") => match args.get(1) {
            Some(path) => parse_command(path),
            None => usage("auto-memory parse <path>"),
        },
        Some("reindex") => reindex_command(&args[1..]),
        Some("status") => status_command(&args[1..]),
        Some("search") => search_command(&args[1..]),
        Some("context") => context_command(&args[1..]),
        Some("schema") => schema_command(&args[1..]),
        Some("hook") => hook_command(&args[1..]),
        // The two event-loop surfaces run on a tokio runtime; everything else is a
        // one-shot pass over the vault or the index and stays synchronous.
        Some("watch") => async_command(watch_command(&args[1..])),
        Some("mcp") => async_command(mcp_command(&args[1..])),
        _ => usage(&format!(
            "auto-memory {}\n\nusage:\n  auto-memory --version\n  auto-memory parse <path>\n  \
             auto-memory reindex --vault <dir> --index <db> [--project <name>] [--full]\n  \
             auto-memory reindex --vault <dir> --index <db> --embeddings \\\n               [--model-cache DIR] [--onnx-runtime PATH] [--embedding-fixture FILE]\n  \
             auto-memory watch --vault <dir> --index <db> [--project <name>] [--window-ms N] [--once]\n  \
             auto-memory mcp --vault <dir> --index <db> [--project <name>] \\\n               [--embedding-fixture FILE | --model-cache DIR] [--onnx-runtime PATH]\n  \
             auto-memory mcp --vault <dir> --index <db> [--project <name>] --http \\\n               [--host HOST] [--port PORT] [--path PATH] [--read-only]\n  \
             auto-memory status --index <db> --project <permalink>\n  \
             auto-memory context <memory://url> --index <db> --project <permalink> \\\n               [--depth N] [--timeframe T] [--page N] [--page-size N] [--max-related N] \\\n               [--json|--plain]\n  \
             auto-memory schema <validate|infer|diff> [target] --index <db> --project <permalink> \\\n               [--vault <dir>] [--threshold F] [--strict] [--text]\n  \
             auto-memory hook <session-start|pre-compact> --harness <claude|codex|pi> \\\n               [--index <db>] [--project <permalink>] [--project-dir <dir>]\n  \
             auto-memory search --index <db> --project <permalink> [--title T] [--type T] \\\n               [--tag TAG] [--status S] [--meta KEY=VALUE] [--after-date WINDOW] \\\n               [--entity-type TYPE] [--category C] [--permalink P] <query>\n  \
             auto-memory search --index <db> --project <permalink> [--vector|--hybrid] \\\n               [--min-similarity F] [--embedding-fixture FILE] [--reranker] \\\n               [--reranker-candidates N] [--reranker-fixture FILE] <query>",
            env!("CARGO_PKG_VERSION")
        )),
    }
}

fn usage(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(2)
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
fn schema_command(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("validate") => schema_validate_command(&args[1..]),
        Some("infer") => schema_infer_command(&args[1..]),
        Some("diff") => schema_diff_command(&args[1..]),
        Some(other) => usage(&format!("unknown schema subcommand: {other}")),
        None => usage("auto-memory schema <validate|infer|diff> ..."),
    }
}

/// Open the index and resolve the project (and vault) a schema command runs against.
fn schema_context(options: &Options) -> Result<(Store, i64, PathBuf), String> {
    let index = options
        .value("index")
        .map(PathBuf::from)
        .ok_or_else(|| "schema requires --index <db>".to_owned())?;
    let permalink = options
        .value("project")
        .ok_or_else(|| "schema requires --project <permalink>".to_owned())?
        .to_owned();
    let store = Store::open(&index)
        .map_err(|error| format!("failed to open index {}: {error}", index.display()))?;
    let project = store
        .project_by_permalink(&permalink)
        .map_err(|error| format!("failed to read project: {error}"))?
        .ok_or_else(|| format!("project not found: {permalink}"))?;
    // Schema definitions are read from their files, so the vault matters: it defaults
    // to the path the project was indexed from.
    let vault = options
        .value("vault")
        .map_or_else(|| PathBuf::from(&project.path), PathBuf::from);
    Ok((store, project.id, vault))
}

fn schema_validate_command(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let (store, project_id, vault) = match schema_context(&options) {
        Ok(context) => context,
        Err(message) => return usage(&message),
    };

    // Reference heuristic: a target containing `/` or `.` is an identifier, anything
    // else is a note type.
    let (note_type, identifier) = match options.positionals.first() {
        Some(target) if target.contains('/') || target.contains('.') => {
            (None, Some(target.as_str()))
        }
        Some(target) => (Some(target.as_str()), None),
        None => (None, None),
    };

    let service = SchemaService::new(&store, project_id, &vault);
    let outcome = schema_tools::validate(&service, note_type, identifier);
    let payload = outcome.payload();
    let status = if options.switch("strict") && payload["error_count"].as_u64().unwrap_or(0) > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    print_schema_outcome(&payload, &outcome.text(), &options);
    status
}

fn schema_infer_command(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let Some(note_type) = options.positionals.first().cloned() else {
        return usage("schema infer requires a note type");
    };
    let threshold = match options.value("threshold") {
        None => auto_memory::application::schema::OPTIONAL_THRESHOLD,
        Some(value) => match value.parse::<f64>() {
            Ok(threshold) => threshold,
            Err(_) => return usage(&format!("--threshold must be a number, got {value}")),
        },
    };
    let (store, project_id, vault) = match schema_context(&options) {
        Ok(context) => context,
        Err(message) => return usage(&message),
    };

    let service = SchemaService::new(&store, project_id, &vault);
    let outcome = schema_tools::infer(&service, &note_type, threshold);
    print_schema_outcome(&outcome.payload(), &outcome.text(), &options);
    ExitCode::SUCCESS
}

fn schema_diff_command(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let Some(note_type) = options.positionals.first().cloned() else {
        return usage("schema diff requires a note type");
    };
    let (store, project_id, vault) = match schema_context(&options) {
        Ok(context) => context,
        Err(message) => return usage(&message),
    };

    let service = SchemaService::new(&store, project_id, &vault);
    let outcome = schema_tools::diff(&service, &note_type);
    print_schema_outcome(&outcome.payload(), &outcome.text(), &options);
    ExitCode::SUCCESS
}

/// Print a schema outcome as the reference CLI does, or as the MCP text surface.
fn print_schema_outcome(payload: &serde_json::Value, text: &str, options: &Options) {
    if options.switch("text") {
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
fn hook_command(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let Some(verb) = options.positionals.first().map(String::as_str) else {
        return usage("auto-memory hook <session-start|pre-compact> --harness <claude|codex|pi>");
    };
    let harness = match options.value("harness") {
        None => Harness::Claude,
        Some(value) => match Harness::parse(value) {
            Some(harness) => harness,
            None => {
                return usage(&format!(
                    "unknown harness: {value} (expected claude, codex, or pi)"
                ));
            }
        },
    };
    // A hook verb never fails the caller; diagnostics go to stderr and the
    // process still exits 0.
    if let Err(message) = run_hook(verb, harness, &options) {
        tracing::warn!("hook {verb} failed: {message}");
        eprintln!("auto-memory hook {verb}: {message}");
    }
    ExitCode::SUCCESS
}

/// Run one hook verb. `Err` is a diagnostic, not a failure.
fn run_hook(verb: &str, harness: Harness, options: &Options) -> Result<(), String> {
    let hook_event = match verb {
        "session-start" => HookEvent::SessionStarted,
        "pre-compact" => HookEvent::CompactionImminent,
        other => return Err(format!("unknown hook verb: {other}")),
    };
    let payload = read_hook_payload();
    let event = normalize(harness, hook_event, &payload);

    let project_dir = options.value("project-dir").map(PathBuf::from);
    let mapping = mapping_dir(project_dir.as_deref(), &event.cwd);
    let (settings, configured) = load_harness_settings(harness, &mapping);

    // Codex ignores PreCompact stdout and asks for the checkpoint from the
    // post-compaction SessionStart instead, so there is nothing to print here.
    if matches!(hook_event, HookEvent::CompactionImminent) {
        return Ok(());
    }

    let Some(permalink) = options
        .value("project")
        .map(str::to_owned)
        .or_else(|| settings.primary_project.clone())
    else {
        // No mapping: emit the first-run nudge instead of guessing a project.
        if !configured {
            println!("# Auto Memory\n\n{}", harness.profile().setup_nudge);
        }
        return Ok(());
    };
    // An explicit `--project` overrides the mapping; reflect it in the brief so
    // the header and placement guidance name the project actually queried.
    let mut settings = settings;
    settings.primary_project = Some(permalink.clone());

    let index = options
        .value("index")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("AUTO_MEMORY_INDEX").map(PathBuf::from))
        .unwrap_or_else(default_index_path);
    let store = Store::open(&index)
        .map_err(|error| format!("failed to open index {}: {error}", index.display()))?;
    let Some(project) = store
        .project_by_permalink(&permalink)
        .map_err(|error| format!("failed to read project {permalink}: {error}"))?
    else {
        return Err(format!("project not found: {permalink}"));
    };

    let checkpoint = if harness == Harness::Codex
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
    );
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

/// Default index path, matching `tools/auto-memory-hook.py`.
fn default_index_path() -> PathBuf {
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from(".local/share/auto-memory/memory.db"),
        |home| PathBuf::from(home).join(".local/share/auto-memory/memory.db"),
    )
}

fn parse_command(path: &str) -> ExitCode {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            eprintln!("failed to read {path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    match auto_memory::markdown::parse_document(path, &content) {
        Ok(document) => print_json(&document),
        Err(error) => {
            eprintln!("failed to parse {path}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn reindex_command(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let Some(vault) = options.value("vault").map(PathBuf::from) else {
        return usage("reindex requires --vault <dir>");
    };
    let Some(index) = options.value("index").map(PathBuf::from) else {
        return usage("reindex requires --index <db>");
    };
    // A missing vault used to look like an empty one (`read_dir` failure is tolerated),
    // which is indistinguishable from "indexed nothing" in the summary output.
    if !vault.is_dir() {
        return usage(&format!("vault directory not found: {}", vault.display()));
    }
    let name = match options.value("project") {
        Some(name) => name.to_owned(),
        None => vault.file_name().map_or_else(
            || "default".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        ),
    };
    let permalink = generate_permalink(&name);

    let mut store = match Store::open(&index) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let project_id = match store.upsert_project(&name, &permalink, &vault.to_string_lossy()) {
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
    if options.switch("embeddings") {
        if let Err(error) = service.reconcile() {
            eprintln!("incremental reindex failed: {error}");
            return ExitCode::FAILURE;
        }
        let provider = match embedding_provider(&options) {
            Ok(provider) => provider,
            Err(message) => {
                eprintln!("Error: {message}");
                return ExitCode::FAILURE;
            }
        };
        return match service.reindex_embeddings(provider.as_ref()) {
            Ok(report) => print_json(&report),
            Err(error) => {
                eprintln!("embedding reindex failed: {error}");
                ExitCode::FAILURE
            }
        };
    }

    if options.switch("full") {
        match service.full_rebuild() {
            Ok(report) => print_json(&report),
            Err(error) => {
                eprintln!("full reindex failed: {error}");
                ExitCode::FAILURE
            }
        }
    } else {
        match service.reconcile() {
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
async fn mcp_command(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let Some(vault) = options.value("vault").map(PathBuf::from) else {
        return usage("mcp requires --vault <dir>");
    };
    let Some(index) = options.value("index").map(PathBuf::from) else {
        return usage("mcp requires --index <db>");
    };
    let name = match options.value("project") {
        Some(name) => name.to_owned(),
        None => vault.file_name().map_or_else(
            || "default".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        ),
    };
    let permalink = generate_permalink(&name);

    let mut store = match Store::open(&index) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let project_id = match ensure_project(&mut store, &name, &permalink, &vault) {
        Ok(project_id) => project_id,
        Err(error) => {
            eprintln!("failed to prepare the project: {error}");
            return ExitCode::FAILURE;
        }
    };

    let external_id = match store.project_by_permalink(&permalink) {
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
    let read_only = options.switch("read-only");
    tracing::info!(
        project = %name,
        permalink = %permalink,
        vault = %vault.display(),
        index = %index.display(),
        read_only,
        transport = if options.switch("http") { "http" } else { "stdio" },
        "mcp server starting"
    );
    // The providers are built once and handed to whichever transport runs, so the two
    // transports share one ONNX runtime instead of loading the model twice.
    // Semantic `search_type`s need the embedding runtime; without these flags the tool
    // reports that semantic search is unavailable instead of falling back to text.
    let provider = if options.value("embedding-fixture").is_some()
        || options.value("model-cache").is_some()
        || options.value("onnx-runtime").is_some()
    {
        match embedding_provider(&options) {
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
    let reranker = if options.switch("reranker") || options.value("reranker-fixture").is_some() {
        match rerank_provider(&options) {
            Ok(provider) => Some(provider),
            Err(message) => {
                eprintln!("Error: {message}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let reranker_candidates = options
        .value("reranker-candidates")
        .and_then(|value| value.parse().ok());
    let reranker_max_chars = options
        .value("reranker-max-chars")
        .and_then(|value| value.parse().ok());

    if options.switch("http") {
        let host = options.value("host").unwrap_or(DEFAULT_HTTP_HOST);
        let port = match options.value("port") {
            Some(value) => match value.parse::<u16>() {
                Ok(port) => port,
                Err(_) => return usage(&format!("--port must be 0..=65535 (got {value:?})")),
            },
            None => DEFAULT_HTTP_PORT,
        };
        let path = options.value("path").unwrap_or(DEFAULT_HTTP_PATH);
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
async fn watch_command(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    // `Options` recognises every switch this CLI defines, so `watch` parses flags it
    // never reads. `--embeddings` is the one an operator would pass expecting vectors
    // to follow the vault; accepting it quietly would make a markdown-only run look
    // like a semantic one, so refuse it and name the command that does the work.
    if options.switch("embeddings") {
        return usage(
            "watch does not take --embeddings: it only keeps the markdown index current — \
             run `auto-memory reindex --vault <dir> --index <db> --embeddings` for the vectors",
        );
    }
    let Some(vault) = options.value("vault").map(PathBuf::from) else {
        return usage("watch requires --vault <dir>");
    };
    let Some(index) = options.value("index").map(PathBuf::from) else {
        return usage("watch requires --index <db>");
    };
    let name = match options.value("project") {
        Some(name) => name.to_owned(),
        None => vault.file_name().map_or_else(
            || "default".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        ),
    };
    let permalink = generate_permalink(&name);
    let mut store = match Store::open(&index) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let project_id = match store.upsert_project(&name, &permalink, &vault.to_string_lossy()) {
        Ok(id) => id,
        Err(error) => {
            eprintln!("failed to register project: {error}");
            return ExitCode::FAILURE;
        }
    };
    let window = options
        .value("window-ms")
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(DEFAULT_WATCH_WINDOW, Duration::from_millis);

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
        once = options.switch("once"),
        "watching the vault"
    );
    // The reconcile is a full vault pass (read, parse, write); like the per-batch
    // indexing below it runs off the reactor.
    let reconciled = match tokio::task::block_in_place(|| service.reconcile()) {
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
    if options.switch("once") {
        // The one-shot path is a bounded blocking collect; keep it off the reactor.
        return match tokio::task::block_in_place(|| watch_once(watcher, window)) {
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
fn status_command(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let Some(index) = options.value("index").map(PathBuf::from) else {
        return usage("status requires --index <db>");
    };
    let Some(permalink) = options.value("project") else {
        return usage("status requires --project <permalink>");
    };
    let store = match Store::open(&index) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let project = match store.project_by_permalink(permalink) {
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
    match store.counts(project.id) {
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
fn context_command(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let Some(index) = options.value("index").map(PathBuf::from) else {
        return usage("context requires --index <db>");
    };
    let Some(permalink) = options.value("project") else {
        return usage("context requires --project <permalink>");
    };
    let Some(url) = options.positionals.first() else {
        return usage("context requires a memory:// url");
    };

    let number = |key: &str, default: u32| -> Result<u32, String> {
        match options.value(key) {
            None => Ok(default),
            Some(value) => value
                .parse::<u32>()
                .map_err(|_| format!("{key} must be a positive integer, got {value}")),
        }
    };
    let context = match (|| {
        let timeframe = options.value("timeframe").unwrap_or("7d");
        let since = timeframe::parse_timeframe(timeframe)?;
        let options = ContextOptions {
            depth: number("depth", 1)?,
            max_related: number("max-related", 10)?,
            page: number("page", 1)?,
            page_size: number("page-size", 10)?,
            since: Some(since),
        };
        options.validate()?;
        Ok::<_, Box<dyn std::error::Error>>((options, url.clone()))
    })() {
        Ok(value) => value,
        Err(error) => {
            eprintln!("Error: {error}");
            return ExitCode::FAILURE;
        }
    };

    let store = match Store::open(&index) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("Error: failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let project = match store.project_by_permalink(permalink) {
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

    let (context_options, url) = context;
    let graph = match build_context(&store, project.id, &url, &context_options) {
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
    if options.switch("plain") {
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

fn search_command(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let Some(index) = options.value("index").map(PathBuf::from) else {
        return usage("search requires --index <db>");
    };
    let Some(permalink) = options.value("project") else {
        return usage("search requires --project <permalink>");
    };
    let store = match Store::open(&index) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("failed to open index {}: {error}", index.display());
            return ExitCode::FAILURE;
        }
    };
    let project = match store.project_by_permalink(permalink) {
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
        query: (!options.positionals.is_empty()).then(|| options.positionals.join(" ")),
        page: options
            .value("page")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1),
        page_size: options
            .value("page-size")
            .and_then(|value| value.parse().ok())
            .unwrap_or(10),
        ..TextSearchOptions::default()
    };
    if let Some(value) = options.value("title") {
        search.title = Some(value.to_owned());
    }
    if let Some(value) = options.value("type") {
        search.note_types.push(value.to_owned());
    }
    if let Some(value) = options.value("tag") {
        search.tags.push(value.to_owned());
    }
    if let Some(value) = options.value("category") {
        search.categories.push(value.to_owned());
    }
    if let Some(value) = options.value("status") {
        search.status = Some(value.to_owned());
    }
    if let Some(value) = options.value("entity-type") {
        match value.parse::<SearchItemType>() {
            Ok(item_type) => search.entity_types = vec![item_type],
            Err(_) => return usage(&format!("unknown entity type: {value}")),
        }
    } else {
        // The reference's implicit default: a category filter scopes the search to
        // observation rows, because categories only exist there.
        search.entity_types = auto_memory::search::default_entity_types(&search.categories);
    }
    if let Some(value) = options.value("permalink") {
        if value.contains('*') {
            search.permalink_match = Some(value.to_owned());
        } else {
            search.permalink = Some(value.to_owned());
        }
    }
    if let Some(value) = options.value("meta") {
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
    if let Some(value) = options
        .value("after-date")
        .or_else(|| options.value("after_date"))
    {
        // Search bounds go through `dateparser`, not the timeframe parser the context
        // tools use; see `domain::dateparser` for why the two disagree.
        let Some(bound) = auto_memory::domain::dateparser::parse_after_date(value) else {
            return usage(&format!("--after-date is not a date or window: {value}"));
        };
        search.after_date = Some(bound);
    }

    // Semantic modes reuse the filter set and embed the query locally.
    if options.switch("vector") || options.switch("hybrid") {
        let provider = match embedding_provider(&options) {
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
        let reranker = if options.switch("reranker") || options.value("reranker-fixture").is_some()
        {
            match rerank_provider(&options) {
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
            candidates: options
                .value("reranker-candidates")
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_RERANKER_CANDIDATES),
            max_document_chars: options
                .value("reranker-max-chars")
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_RERANKER_MAX_DOCUMENT_CHARS),
        });

        let vector_options = VectorSearchOptions {
            min_similarity: options
                .value("min-similarity")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0.55),
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
        let page = if options.switch("hybrid") {
            search_hybrid(
                &store,
                project.id,
                &query_text,
                &query_vector,
                provider.model_name(),
                &vector_options,
                rerank_request.as_ref(),
            )
        } else {
            search_vector(
                &store,
                project.id,
                &query_vector,
                provider.model_name(),
                &vector_options,
                rerank_request.as_ref(),
            )
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

    match store.search_text(project.id, &search) {
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
/// runtime needed); otherwise the reference ONNX model is loaded from
/// `--model-cache` (default `~/.config/basic-memory/fastembed_cache`).
fn embedding_provider(
    options: &Options,
) -> Result<Box<dyn EmbeddingProvider + Send + Sync>, String> {
    if let Some(path) = options.value("embedding-fixture") {
        let json = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read {path}: {error}"))?;
        let provider = FixtureEmbeddingProvider::from_json(&json)
            .map_err(|error| format!("failed to load {path}: {error}"))?;
        return Ok(Box::new(provider));
    }
    let cache = options.value("model-cache").map_or_else(
        || {
            std::env::var_os("HOME").map_or_else(
                || PathBuf::from(".config/basic-memory/fastembed_cache"),
                |home| PathBuf::from(home).join(".config/basic-memory/fastembed_cache"),
            )
        },
        PathBuf::from,
    );
    let runtime = options
        .value("onnx-runtime")
        .map(PathBuf::from)
        .or_else(find_onnx_runtime);
    let provider = OnnxEmbeddingProvider::load_from_cache(&cache, runtime.as_deref())
        .map_err(|error| format!("failed to load the embedding model: {error}"))?;
    Ok(Box::new(provider))
}

/// Build the cross-encoder reranker when the caller asked for one.
///
/// `--reranker-fixture FILE` supplies deterministic scores (a JSON map of query →
/// document → relevance) for tests and offline runs; otherwise the reference model is
/// loaded from the shared fastembed cache.
fn rerank_provider(options: &Options) -> Result<Box<dyn RerankProvider + Send + Sync>, String> {
    if let Some(path) = options.value("reranker-fixture") {
        let json = std::fs::read_to_string(path)
            .map_err(|error| format!("failed to read {path}: {error}"))?;
        let provider = FixtureRerankProvider::from_json(&json)
            .map_err(|error| format!("failed to load {path}: {error}"))?;
        return Ok(Box::new(provider));
    }
    let cache = options.value("model-cache").map_or_else(
        || {
            std::env::var_os("HOME").map_or_else(
                || PathBuf::from(".config/basic-memory/fastembed_cache"),
                |home| PathBuf::from(home).join(".config/basic-memory/fastembed_cache"),
            )
        },
        PathBuf::from,
    );
    let runtime = options
        .value("onnx-runtime")
        .map(PathBuf::from)
        .or_else(find_onnx_runtime);
    let provider = OnnxRerankProvider::load_from_cache(&cache, runtime.as_deref())
        .map_err(|error| format!("failed to load the reranker: {error}"))?;
    Ok(Box::new(provider))
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

/// Minimal `--key value` / `--switch` parser for the CLI surface.
struct Options {
    values: BTreeMap<String, String>,
    switches: Vec<String>,
    positionals: Vec<String>,
}

const SWITCH_FLAGS: &[&str] = &[
    "json",
    "plain",
    "full",
    "verbose",
    "local",
    "embeddings",
    "vector",
    "hybrid",
    "once",
    "strict",
    "text",
    "read-only",
    "http",
];

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut values = BTreeMap::new();
        let mut switches = Vec::new();
        let mut positionals = Vec::new();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            let Some(key) = arg.strip_prefix("--") else {
                positionals.push(arg.clone());
                index += 1;
                continue;
            };
            if SWITCH_FLAGS.contains(&key) {
                switches.push(key.to_owned());
                index += 1;
                continue;
            }
            match args.get(index + 1) {
                Some(next) if !next.starts_with("--") => {
                    values.insert(key.to_owned(), next.clone());
                    index += 2;
                }
                _ => {
                    switches.push(key.to_owned());
                    index += 1;
                }
            }
        }
        Ok(Self {
            values,
            switches,
            positionals,
        })
    }

    fn value(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    fn switch(&self, key: &str) -> bool {
        self.switches.iter().any(|switch| switch == key)
    }
}
