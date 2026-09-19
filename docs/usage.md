# Using `auto-memory-rs`

Local-only core of Basic Memory, behavior-compatible with the reference 0.23.2 release. There is
no Web UI and no cloud sync: **Obsidian is the management interface**, the vault of markdown files
is the only durable state, and everything else (the SQLite index, the embeddings) is derived.

## 1. Build

```bash
cargo build --release            # add --offline when the registry cache is warm
./target/release/auto-memory --version
```

The embedding runtime (`search --vector` / `--hybrid`, `reindex --embeddings`) needs the fastembed
cache and the ONNX Runtime shared library; text search, context, schema, and the MCP server work
without either. See `docs/auto-memory-rs-execution-plan.md` Phase 8b for the model details.

## 2. The vault

Point `--vault` at any directory of markdown files — typically the folder Obsidian already opens.
Notes are ordinary markdown:

```markdown
---
title: Ada Lovelace
type: person
tags: [person, history]
---

# Ada Lovelace

- [fact] First programmer
- [role] Mathematician
- works_at [[organizations/analytical-engine]]

See also [[people/grace-hopper]].
```

`[category] content` lines become observations, `[[wikilinks]]` (and `relation_type [[target]]`
lines) become relations, and frontmatter is normalized the way the reference does.

## 3. Index

The index is a SQLite file **outside** the vault, so it never shows up in Obsidian:

```bash
auto-memory reindex --vault ~/vault --index ~/.local/share/auto-memory/memory.db --project oracle
auto-memory status  --index ~/.local/share/auto-memory/memory.db --project oracle
```

`reindex` is incremental (only changed files are rewritten); `--full` prunes stale rows and
rebuilds the whole project; `--embeddings` additionally refreshes the semantic chunks.

## 4. Keep it current while Obsidian is open

```bash
auto-memory watch --vault ~/vault --index ~/.local/share/auto-memory/memory.db --project oracle
```

The watcher applies the reference ignore rules (dot-directories such as `.obsidian/` and
`.basic-memory/`, `node_modules`, non-markdown files, plus `.bmignore`), waits out the 1000 ms
debounce window, and then indexes. Because it pairs a delete event with the matching create event
into a *move*, renaming a note in Obsidian's file explorer keeps the note's identity and permalink
(and therefore keeps every other note's links to it working). An atomic save — a temp file
renamed over the note, which is how Obsidian writes — is a single update, not a delete plus a
create. Running `watch --once` applies one batch and exits, which is useful from a cron job or a
sync hook.

The daemon maintains the **markdown index only**. It reads `--vault`, `--index`, `--project`,
`--window-ms` and `--once`, and `--embeddings` exits with code 2 pointing at
`reindex --embeddings` rather than being accepted and ignored: vector refresh is a separate pass,
and a run that silently skipped it would look semantic when it was not.

## 5. Query

```bash
auto-memory search --index ~/.local/share/auto-memory/memory.db --project oracle "rust"
auto-memory context memory://notes/simple --index ~/.local/share/auto-memory/memory.db --project oracle
auto-memory schema validate person --index ~/.local/share/auto-memory/memory.db --project oracle
```

`search` supports `--title`, `--permalink`, `--type`, `--tag`, `--category`, `--status`,
`--entity-type`, `--meta key=value`, `--after-date <window>`, and the vector legs `--vector` /
`--hybrid` / `--min-similarity`. A `--category` filter without `--entity-type` implicitly scopes
the search to observation rows, matching the reference.

`--after-date` accepts `1d`, `2 weeks ago`, `2026-09-01`, and friends. Search bounds go through the
reference's `dateparser` semantics, which are **not** the timeframe semantics the `context` and
`recent_activity` tools use: the value is read as naive local time and compared as UTC, so a
relative bound lands one UTC-offset *later* than you would expect from `context --timeframe`.

One deliberate difference from `bm tool search-notes`: there, `--title` and `--permalink` are
switches that reinterpret the positional query, while here they take the value inline
(`--title Alpha`, `--permalink 'projects/*'`). This port's flags mirror the MCP tool's parameter
names, where `title`/`permalink` are values. `context` walks the graph and prints
the reference's JSON, its `--plain` outline, or the MCP markdown (`--json`). `schema
validate|infer|diff` prints the same payloads as the reference's `bm tool schema-*` commands
(`--text` for the MCP text surface, `--strict` to exit non-zero on validation errors).

## 6. MCP clients

```bash
auto-memory mcp --vault ~/vault --index ~/.local/share/auto-memory/memory.db --project oracle
```

Newline-delimited JSON-RPC 2.0 on stdout, diagnostics on stderr. Example client configuration:

```json
{
  "mcpServers": {
    "basic-memory": {
      "command": "/path/to/auto-memory",
      "args": [
        "mcp",
        "--vault", "/home/me/vault",
        "--index", "/home/me/.local/share/auto-memory/memory.db",
        "--project", "oracle"
      ]
    }
  }
}
```

The same tools are served over the MCP **Streamable HTTP** transport:

```bash
auto-memory mcp --vault ~/vault --index ~/.local/share/auto-memory/memory.db --project oracle \
  --http --host 127.0.0.1 --port 8765 --path /mcp
```

`--host` defaults to `127.0.0.1`, `--port` to `8765`, `--path` to `/mcp`. The host defaults to
loopback because the endpoint is unauthenticated; pass `--host 0.0.0.0` to expose it. The port
is deliberately not the reference CLI's `8000`, to avoid colliding with a running reference
server. The endpoint is the
specification's Streamable HTTP transport (JSON-RPC `POST`, `Mcp-Session-Id`, SSE responses),
served by the official `rmcp` SDK. `--read-only` hides and refuses the mutating tools on both
transports. Diagnostics go to stderr either way.

The note tools are upsert-shaped, like the reference's: `write_note` fills `type` from
`note_type` and merges `tags` (content frontmatter still wins), refuses to clobber an existing
note unless `overwrite=True` (or `directory` escapes the project), and `edit_note` creates the note
its identifier names when it does not exist. `delete_note(..., is_directory=True)` and
`move_note(..., is_directory=True, destination_path=...)` operate on whole folders; the default
`output_format` is `text` for all the note tools, so pass `output_format="json"` for the structured
payload.

Twenty tools are exposed: the note family (`write_note`, `read_note`, `view_note`,
`read_content`, `edit_note`, `move_note`, `delete_note`), search (`search_notes`, plus the
`search`/`fetch` adapters that answer only OpenAI's MCP client), graph (`build_context`),
navigation (`list_directory`), activity (`recent_activity`), projects
(`list_memory_projects`, `create_memory_project`, `delete_project`), the schema tools
(`schema_validate`, `schema_infer`, `schema_diff`), and `basic_memory_diagnostics`. The server is
always constrained to one project; use the CLI for project lifecycle and for indexing.

Add `--reranker` to rescore the top candidates with a cross-encoder
(`jinaai/jina-reranker-v1-tiny-en`, loaded from the same model cache) before the page is
sliced — the reference ships this **off by default** ("adds latency and a first-run model
download"). `--reranker-candidates N` (default 20) sets the rescored window, and
`--reranker-fixture FILE` supplies deterministic scores for tests and offline runs. The
same flags work on `auto-memory mcp`.

Semantic search types (`search_type="vector"` / `"semantic"` / `"hybrid"`) need the embedding
runtime: start the server with `--embedding-fixture FILE` (deterministic test vectors) or
`--model-cache DIR` (the fastembed model, `~/.config/basic-memory/fastembed_cache` by default).
Without either, those requests answer with the reference's "Semantic Search Disabled" guidance
instead of falling back to text. The vector index itself is built by
`auto-memory reindex --embeddings`.

## 7. Recovery

There is nothing to recover beyond the markdown: delete the index file (or the whole state
directory) and rebuild.

```bash
rm ~/.local/share/auto-memory/memory.db
auto-memory reindex --vault ~/vault --index ~/.local/share/auto-memory/memory.db --project oracle --full --embeddings
```

A full rebuild reproduces the same projection the incremental index had, including links whose
target note has been deleted (they come back as unresolved links, exactly as they read in the
file). `tests/obsidian_compatibility.rs` pins the editing, renaming, deleting, and
link-before-target paths; `tests/incremental_golden.rs` pins incremental/full convergence.

## 8. Deliberate differences from the reference

- `auto-memory` creates the index file's parent directory when it is missing and
  refuses a `--vault` that is not a directory; the reference expects the first to exist and
  silently indexes nothing for the second.

- No Web UI, no cloud sync, no `list_workspaces` — this is the local core only.
- The vault is never rewritten during indexing. The reference injects `title`/`type`/`permalink`
  into files that lack frontmatter (`ensure_frontmatter_on_sync`); here Obsidian owns the files,
  so the index records what is on disk.
- `read_content` serves image bytes verbatim instead of resizing them through Pillow.
- Tool results do not carry the reference's `_meta.fastmcp.*` marker.
- Embeddings are stored as BLOBs and scored in Rust rather than in a `sqlite-vec` `vec0`
  virtual table. Both do exact KNN, and the vector, hybrid, and reranked results are verified
  equal to captured reference runs, so this is a storage-format choice (the `vector_index`
  marker reads `blob` instead of `sqlite-vec`); only the reference's `bm inspect` diagnostic
  reports it, and that subsystem is not part of this port.
- The reference CLI's retrieval-inspection diagnostics (`bm inspect query` / `bm inspect
  chunks`, and the `doctor`/`orphans` reports) are not ported: they trace and render internal
  retrieval stages, which this port's tests cover directly against captured reference output
  instead.

Everything else is pinned against captured reference behavior; see `docs/reference.md` for the
per-phase evidence and `tests/golden/README.md` for the corpus.
