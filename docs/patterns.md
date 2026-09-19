# Design Patterns — what this port uses, and what it deliberately avoids

The port has to stay byte-compatible with the reference while being idiomatic Rust, so
patterns are adopted only where they buy something. This note records the decisions, the
reasons, and the trigger that would make a rejected pattern worth revisiting.

## In use

| Pattern | Where | Why it fits |
|---|---|---|
| Strategy (trait object) | `search::embedding::EmbeddingProvider`, `runtime::rerank::RerankProvider` | Two real implementations each: the ONNX runtime and the fixture provider used by tests. Callers take `&dyn …`, so adding a provider is a new type, not a new branch. |
| Sum-type dispatch (closed vocabulary) | `ToolName`, `SearchType`, `OutputFormat`, `EditOperation`, `DirectorySortOrder`, `SearchItemType`, `ValidationMode` | The reference's vocabularies are closed sets. An enum + exhaustive `match` makes "every surface handles every case" a compile-time property, and `strum` keeps the wire spelling in one place. Adding a case is a compile error until `tools/list`, dispatch and the golden coverage test agree. |
| Options struct | `TextSearchOptions`, `VectorSearchOptions`, `ContextOptions`, `ActivityOptions`, `DirectoryOptions`, `EditOptions`, `IndexOptions`, `RebuildOptions` | `Default` + `..Default::default()` means a new optional setting does not touch existing call sites — the Rust way to avoid a telescoping constructor or a mutable builder. |
| Newtype | `Permalink`, `RelationType`, `ProjectId`, `EntityId`, `DocumentId`, `Scratch` | Validation at construction and no accidental mix-ups between ids that are all strings on the wire. |
| RAII guard | `Store` (connection + prepared statements), `Scratch` (test temp dirs), `Debouncer` (its own window state) | Resource lifetime is tied to scope; no manual cleanup to forget. |
| Adapter + layered modules | `adapters/{cli,mcp,filesystem}` → `application/` → `domain/` (+ `storage/`, `search/`, `indexing/` as infrastructure) | The domain is unaware of MCP, SQLite and the CLI; a new front end is a new adapter. `src/lib.rs` states the dependency rule. |
| Builder | `tests/common::Session` | One scripted MCP session needs a vault, an index and a few server flags; the builder names the defaults (`--project oracle`) and keeps the optional parts (`--embedding-fixture`) explicit. Tests only — no runtime cost. |
| Fixture / provider-from-capture | `tests/common::{Scratch, fixture, indexed_store}`, `FixtureEmbeddingProvider`, `FixtureRerankProvider` | Tests replay captured reference data instead of re-deriving it, which is what makes the golden comparisons meaningful. |

## Considered and rejected

| Pattern | Why not (now) | Revisit when |
|---|---|---|
| Repository trait over `Store` | There is exactly one backend (SQLite) and tests already run against in-memory SQLite, so a trait would add dynamic dispatch and a second source of truth for the schema. | A second backend (Postgres/cloud) is implemented — it is explicitly out of scope today. |
| `Tool` trait + runtime registry for MCP tools | The tool set is closed and enumerated; the exhaustive `match` in `call_tool` plus the `tools/list` coverage test already guarantee no tool can be half-registered. A registry would trade that for runtime lookups. | Tools become plugin-provided or the count grows enough that one file per tool group is unwieldy — splitting `server.rs` into tool-group modules is the cheaper step first. |
| Abstract factory / DI container | Collaborators are concrete (`Store`, providers) and injected by constructor; there is no runtime choice of implementation graph. | Several environments (local, server, cloud) need different wiring. |
| Observer / event bus for the watcher | `notify` + the debouncer already are the extension point, and the reference exposes no hook to mirror. | The port grows non-reference consumers (e.g. LSP-style notifications). |
| Visitor over the markdown AST | The parser is a single pass into a fixed structure, and its output is byte-compared against the reference; indirection would obscure the parity. | A second consumer needs to rewrite documents structurally (e.g. a formatter that preserves all trivia). |
| Interpreter/AST for Picoschema | `schema/parser.rs` is a deliberate line-by-line port of the reference's algorithm with golden tests; an AST + evaluator would be a second implementation to keep in sync. | The schema language is extended beyond the reference. |
| Singleton / global state | Would break per-test isolation (each test case owns a `Store`, a vault and a project id). | Never — it conflicts with the test strategy. |

## Extension points that already exist

- **New tool**: add a `ToolName` variant (its wire name comes from `strum`), a `definition()`
  arm, and a `call_tool` arm — the coverage test in `adapters::mcp::server` fails until all
  three agree.
- **New search type / output format / edit operation**: add the enum variant; every `match`
  that has to care stops compiling.
- **New embedding or reranking backend**: implement the provider trait and pass it to
  `McpServer::with_provider` / `with_reranker`.
- **New front end**: write an adapter under `src/adapters/` against the application services;
  the domain and storage layers stay untouched.
- **New test fixture**: `common::Scratch` + `copy_dir`/`copy_dir_with_mtimes` + `Session`
  keep the temp-dir lifecycle and the MCP handshake out of the test body.
