# auto-memory-rs Specification

- **Status:** Accepted
- **Date:** 2026-09-08
- **Scope:** Local-only Rust implementation of the Basic Memory core
- **Repository:** `auto-memory-rs`

## 1. Purpose

`auto-memory-rs` is a local-first Rust implementation of the Basic Memory core. It keeps the core data model, parsing semantics, indexing behavior, search behavior, knowledge-graph traversal, context construction, and MCP behavior compatible with the reference implementation while intentionally excluding Web and Cloud product layers.

The implementation must be idiomatic Rust internally. Compatibility applies to externally observable behavior and algorithm results, not to the source language's original module layout or class structure.

## 2. Scope

### 2.1 Included

- Markdown as the source of truth.
- Obsidian-compatible local file workflow.
- YAML frontmatter.
- Entities, observations, and relations.
- Wikilinks and permalink resolution.
- Project isolation and `memory://` URI resolution.
- Incremental and full indexing.
- SQLite-derived index.
- SQLite FTS5 text search.
- Local vector search.
- Hybrid search and deterministic ranking.
- Knowledge-graph traversal.
- Context construction.
- Schema inference, validation, and drift detection.
- CLI.
- Local stdio MCP server.
- Golden compatibility tests.
- Full recovery of derived state from Markdown files.

### 2.2 Explicitly excluded

- Web UI.
- Cloud API or hosted service.
- User accounts and authentication.
- Team workspaces and permissions.
- Billing and subscriptions.
- Remote MCP.
- Built-in cloud storage or backup.
- Built-in multi-device synchronization.
- Built-in conflict resolution.
- Mobile applications.

External tools such as Git, Syncthing, or Dropbox may synchronize the Markdown directory, but synchronization is outside the responsibility of `auto-memory-rs`.

## 3. Compatibility Definition

Compatibility is defined by behavior, not by internal implementation.

For the same project, Markdown corpus, configuration, and query, the Rust implementation should produce equivalent:

- Parsed entities.
- Observations and categories.
- Relations and relation types.
- Permalinks and project resolution.
- Search candidates.
- Search ranking and pagination.
- Graph traversal results.
- `build_context` output.
- MCP tool inputs, outputs, and errors.

Floating-point values may use an explicitly documented tolerance. Entity identity, relation identity, ordering, pagination, and context structure must not use fuzzy comparison.

## 4. Data Ownership Model

```text
Markdown files = source of truth
SQLite          = rebuildable derived index
Embeddings      = rebuildable derived data
```

Deleting the `.basic-memory/` directory must not delete user knowledge. A full reindex must recreate entities, observations, relations, text indexes, vectors, and graph data from the Markdown corpus.

Suggested project layout:

```text
memory-project/
├── notes/
├── projects/
├── people/
└── .basic-memory/
    ├── index.sqlite3
    ├── embeddings/
    ├── locks/
    └── state.json
```

The `.obsidian/` directory is not treated as a knowledge document directory and must not be indexed as note content.

## 5. Logical Architecture

```text
Obsidian
   │ local Markdown files
   ▼
File Watcher / CLI / MCP Adapters
   ▼
Application Services
   ▼
Domain Core
   ├── Markdown parsing
   ├── Indexing
   ├── Search and ranking
   ├── Knowledge graph
   ├── Context construction
   └── Schema operations
   ▼
Infrastructure
   ├── File system
   ├── SQLite
   ├── FTS5
   └── Local embedding runtime
```

Dependency direction must point inward:

```text
Adapters -> Application -> Domain
                    \-> Infrastructure
```

Domain code must not depend directly on MCP, CLI, SQLite, Tokio, or a concrete embedding implementation.

## 6. Rust Code Organization

The codebase uses idiomatic Rust organization rather than mechanically copying another language's file structure.

```text
src/
├── lib.rs
├── main.rs
├── config.rs
├── error.rs
├── domain/
├── markdown/
├── application/
├── search/
├── graph/
├── storage/
├── indexing/
├── schema/
├── adapters/
│   ├── mcp/
│   ├── cli/
│   └── filesystem/
└── runtime/
```

Rules:

- Use domain newtypes such as `ProjectId`, `EntityId`, `Permalink`, and `RelationType`.
- Use typed `Result` errors in library code.
- Use `thiserror`-style typed errors at module boundaries and add human-readable context in CLI code.
- Use `?` propagation; avoid unqualified `unwrap()`.
- Use `as_`, `to_`, and `into_` according to borrowing, allocation, and ownership semantics.
- Do not use `get_` prefixes for ordinary accessors.
- Keep parser, ranking, graph traversal, and context selection deterministic and independently testable.
- Use traits only at real external boundaries such as storage, filesystem, clock, and embedding providers.
- Keep CPU-bound algorithms synchronous; use async primarily for MCP transport, watcher/event loops, and background work.
- Do not hold locks across `.await`.
- Avoid global mutable state.
- Run `rustfmt`, `clippy`, unit tests, integration tests, and documentation checks in CI.

## 7. Domain Model

Core domain types:

```text
Project
Document
Entity
Observation
Relation
Permalink
SearchQuery
SearchResult
Context
SchemaDefinition
IndexState
```

A parsed document is an intermediate representation and must be independent of storage:

```text
ParsedDocument
├── metadata
├── observations
├── relations
├── wikilinks
└── body
```

The Markdown parser must not write SQLite or trigger MCP behavior.

## 8. Markdown Semantics

The parser must support:

- YAML frontmatter.
- Note title and type.
- Optional permalink.
- Tags.
- Observation lines such as `- [decision] ...`.
- Relation lines such as `- depends_on [[Target]]`.
- Quoted multi-word relation types.
- Bare wikilinks with a default relation type.
- Display labels such as `[[Target|Display Name]]`.
- Wikilinks embedded in prose.
- Unresolved targets that can later be resolved.
- UTF-8 content, including CJK text.

Parsing must not rewrite the source Markdown unless an explicit note-edit operation requests it.

## 9. Indexing Semantics

A single file update is transactional:

```text
BEGIN
  remove old derived records for the file
  insert parsed entity
  insert observations
  insert relations
  update FTS5 records
  update or invalidate embeddings
COMMIT
```

Required operations:

- Create.
- Modify.
- Delete.
- Rename.
- Full rebuild.
- Incremental rebuild.
- Periodic reconciliation.

Re-indexing the same file repeatedly must be idempotent. Incremental and full indexing of the same corpus must produce equivalent results.

## 10. Search Semantics

Search is divided into:

```text
Query parsing
  -> candidate retrieval
  -> text/vector fusion
  -> ranking
  -> filters
  -> pagination
  -> result hydration
```

Text search must support the reference behavior for ordinary terms, phrases, Boolean operators, prefix matching, project filters, tags, categories, types, dates, limits, and offsets.

The local implementation uses SQLite FTS5 for text search unless compatibility tests demonstrate that a different implementation is required.

Semantic search must pin:

- Embedding model and version.
- Tokenizer.
- Input normalization.
- Chunk size and overlap.
- Vector dimensions.
- Distance function.
- Candidate count.
- Similarity threshold.
- Score normalization.
- Tie-break rules.

Hybrid search must use the reference scoring and ordering specification. It must not silently substitute RRF, a weighted average, or another ranking algorithm.

## 11. Graph and Context Semantics

`build_context` is not equivalent to a simple text search.

```text
memory:// URI
  -> resolve project
  -> resolve root entity
  -> load observations
  -> load outgoing and incoming relations
  -> traverse to configured depth
  -> remove duplicates and cycles
  -> apply max-related limit
  -> render text or JSON context
```

Traversal order, depth, maximum related results, directionality, unresolved links, duplicate removal, and rendering shape are compatibility requirements.

## 12. Application Services

The shared application layer contains:

```text
NoteService
ProjectService
IndexService
SearchService
ContextService
SchemaService
DiagnosticsService
```

MCP and CLI must call these services instead of implementing separate business logic.

## 13. MCP and CLI

The first MCP transport is local stdio only:

```bash
auto-memory mcp --project /path/to/memory
```

MCP adapters are responsible for input deserialization, validation, error mapping, and output serialization. They must not contain SQL, Markdown parsing, search ranking, or graph traversal.

Initial tool groups:

```text
Content:    write_note, read_note, edit_note, move_note, delete_note,
            read_content, view_note
Search:     search, search_notes, fetch, recent_activity, list_directory
Graph:      build_context
Projects:   list_memory_projects, create_memory_project, delete_project
Schema:     schema_infer, schema_validate, schema_diff
Diagnostics: auto_memory_diagnostics
```

## 14. Reliability Requirements

- No network access is required for normal local operation.
- SQLite is rebuildable.
- File writes are atomic where possible.
- Index updates are transactional.
- Watcher events are debounced and reconciled.
- A malformed document must not corrupt the complete index.
- Cyclic graphs must terminate.
- Path traversal outside a project root must be rejected.
- MCP stdout must contain protocol output only; logs go to stderr or a log file.
- The system must provide `status`, `doctor`, and full-reindex diagnostics.

## 15. Compatibility Test Requirements

Every core behavior needs a fixture containing:

```text
input Markdown/config/query
reference output
Rust output
canonicalized diff
```

Required test groups:

- Parser.
- Frontmatter.
- Observations.
- Relations.
- Wikilinks.
- Project and URI resolution.
- Full and incremental indexing.
- Text search.
- Vector search.
- Hybrid ranking.
- Graph traversal.
- Context rendering.
- Note mutations.
- Schema operations.
- MCP request/response behavior.
- Error behavior.

## 16. Definition of Done

The implementation is complete only when:

- The local core works without Web or Cloud services.
- Obsidian can edit the Markdown corpus without breaking the index.
- Full reindex recreates all derived state.
- Core algorithm golden tests pass.
- MCP and CLI share the same application services.
- `cargo fmt --check` passes.
- `cargo clippy --all-targets --all-features -- -D warnings` passes.
- `cargo test --all` passes.
- Documentation tests pass.
- No untracked Cloud/Web dependency is required for local operation.
