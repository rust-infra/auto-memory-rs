# auto-memory-rs Execution Plan

- **Status:** Accepted plan
- **Date:** 2026-09-08
- **Repository:** `auto-memory-rs`
- **Target:** Local-only Rust core with Basic Memory behavior compatibility

## 0. Progress

- **Phase 0 — Lock the Reference Baseline:** done. Pinned Basic Memory 0.23.2 (see `reference.md`); contract docs `data-format.md`, `search-spec.md`, `context-spec.md`, `mcp-spec.md`, `compatibility-spec.md` written from the installed source.
- **Phase 1 — Build the Golden Corpus:** done. `tools/export_reference.py` oracle harness, `tests/fixtures/vault` fixtures, 34 golden artifacts in `tests/golden/`, `tests/common/` (shared canonicalization, fixture and MCP-session helpers).
- **Phase 2 — Bootstrap the Rust Project:** done. `lib.rs`/`main.rs`, `config`, `error`, `domain`, `markdown`, adapter/future-layer scaffolding; `serde`/`serde_json`/`serde_yaml_ng`/`thiserror`; `cargo fmt`/`clippy -D warnings`/`test` gates pass.
- **Phase 3 — Domain Model:** done (first slice). Newtypes (`ProjectId`, `EntityId`, `DocumentId`, `Permalink`, `RelationType`), `Frontmatter`, `Observation`, `Relation`, `ParsedDocument`, `Wikilink`, search enums/query/result.
- **Phase 4 — Markdown Parsing:** done for the golden corpus. Frontmatter normalization, observations, relations (explicit + inline `links_to`), wikilinks, permalink generation; `tests/parser_golden.rs` diffs Rust against the reference parser for all 16 fixtures and passes.
- **Phase 5 — SQLite Storage:** done (first slice). `src/storage/` (schema + `Store`), `src/indexing/rebuild.rs`, reference-mirrored tables (`project`/`entity`/`observation`/`relation` + FTS5 `search_index` created), transactional document replacement, cascade delete, deterministic `external_id`, SHA-256 checksums, relation resolution, full vault rebuild. `tests/storage_golden.rs` diffs the rebuilt projection against `tests/golden/parse/` and passes (15 entities / 16 observations / 16 relations, malformed file skipped).
- **Phase 6 — Full & Incremental Indexing:** done (first slice). `src/indexing/{document,service,debounce,rebuild}.rs`: `index_file` (checksum skip), `force_index_file`, `remove_file`, `move_file` (keeps permalink unless `update_permalinks_on_move`), `reconcile` (add/update/unchanged/removed/skipped + relation resolution), `full_rebuild` with stale-row pruning, and a path-coalescing `Debouncer` (1 s reference window). CLI adds `reindex --vault … --index … [--full]` and `status`. `tests/incremental_golden.rs` proves idempotency, no ghosts on rename, pruning on delete, malformed-file isolation, debounce→reconcile, and incremental/full convergence.
- **Phase 6b — OS filesystem watcher:** done. `src/indexing/watcher.rs` maps `notify` events to project-relative paths, drops ignored paths (`DEFAULT_IGNORE_PATTERNS`, `.bmignore`) and non-markdown files, coalesces them through the existing `Debouncer` (reference `index_delay` = 1000 ms), pairs delete/create events with a matching checksum into `IndexService::move_file` (so `update_permalinks_on_move=false` keeps the permalink), applies the rest as index/remove, and exposes `watch_vault`/`watch_once` plus CLI `watch [--window-ms N] [--once]`. `tests/watch_golden.rs` (5, incl. a real `notify` loop) + `tests/cli_golden.rs` cover it.
- **Phase 7 — Text Search (FTS5):** done (first slice). `search_index` is populated for entity/observation/relation rows; `SQLite` FTS5 search with the reference query shape (`title MATCH ? OR content_stems MATCH ? OR content_snippet MATCH ?`), `bm25()` ordering (negative scores), boolean/phrase/prefix preparation, title match, permalink/glob, note-type/category/tag/status/metadata filters, date filter, exact totals, pagination, and the OR-relaxation retry. CLI adds `search`. `tests/search_golden.rs` matches reference ordering **and scores** for text/CJK/phrase/boolean/prefix queries plus filters, category rows, pagination, and relaxation.
- **Phase 8 — Vector & Hybrid Search:** done for the local core. Reference chunking port (`search/chunking.rs`, verified against a 78-chunk corpus), `EmbeddingProvider` + ONNX runtime (`src/runtime/embedding.rs`: `fastembed` + `ort` on the reference `model_optimized.onnx`, offline, CLS pooling, 384 dims, drift ≤ ~2.3e-4 with identical ranking), `search_vector_chunks`/`search_vector_embeddings` storage with source-hash reuse, `IndexService::reindex_embeddings`, vector/hybrid retrieval (`aggregate_matches`, `fuse_hybrid`, probe pagination, reference `matched_chunk`), and CLI `reindex --embeddings` + `search --vector|--hybrid|--min-similarity`. End-to-end: `tests/vector_golden.rs` (6) reproduces `vector-local-index.json` and `hybrid-rust.json` through the stored index, `tests/cli_golden.rs` through the CLI, `tests/embedding_runtime.rs` against captured reference vectors. The default-off reranker landed in Phase 8c, the vector-leg filters in Phase 15b; `vec0` storage stays an accepted internal difference (see the completion audit).
- **Phase 9 — Graph & Context (`build_context`):** done. `Store::find_related` ports the reference recursive CTE verbatim (`ORDER BY depth, type, id LIMIT max_related`, relations at odd depths, `MIN(depth)` dedupe); `src/application/context.rs` mirrors the API hydration layer (primary/observations/related summaries, metadata counts, `timeframe` filter, page semantics, empty-graph behavior) and both text surfaces (`render_plain` = CLI `--plain`, `render_markdown` = MCP `output_format="text"`). CLI adds `context <memory://url>`. Supporting parity work: frontmatter `created`/`modified` → `entity.created_at`/`updated_at` (+ file-time fallback, copied into `search_index` rows), reference relation insertion order (`(relation_type, to_name)` sort), and `src/domain/timeframe.rs` (`validate_timeframe`/`parse_timeframe` + reference timestamp layouts). `tests/context_golden.rs` (12) + `tests/cli_golden.rs` (7) green, including a row-for-row replay of the reference traversal against the reference ids. See `reference.md` §6e for the run-dependent-id caveat.
- Note: the FTS5 table is created but **not populated**; stemming/CJK channels and `search_index` parity belong to Phase 7.
- **Phase 11 — stdio MCP Server (fourth slice):** the transport is unchanged (newline-delimited JSON-RPC on stdout, diagnostics on stderr), and the tool surface is now 15 tools: the note family (`write_note`/`read_note`/`view_note`/`read_content`/`edit_note`/`move_note`/`delete_note`), `search_notes`, `build_context`, `list_directory`, `recent_activity`, the project trio (`list_memory_projects`/`create_memory_project`/`delete_project`), and `auto_memory_diagnostics`. `src/application/directory.rs` ports `DirectoryService.list_directory` end to end (prefix query with the reference `LIKE 'prefix/%'` semantics, two-pass tree build, depth/glob collection where the glob gates inclusion but never recursion, the four sort orders with Python's stable `reverse=True` tie behavior, bounded pages whose nodes always carry `children: []`, and the block-character text renderer). `src/application/activity.rs` ports `build_context` with **no** `memory_url`: a recency search (`search_item_types` + `after_date`, `LIMIT page_size+1`, `ORDER BY updated_at DESC`, offset emulated through a wider first page), the shared `find_related` traversal and hydration, `uri: null` metadata, `_extract_recent_rows`, and the guide-laden `_format_project_output` text. `read_note` now follows the reference default `output_format="text"` and its miss chain (resolution → exact title → `format_related_results` → `format_not_found_message`), with the JSON surface `{title, permalink, file_path, content, frontmatter}`. `create_memory_project`/`delete_project` return the reference's constrained-server refusals (the `Use the CLI …` hint is re-pointed at this port's own binary — see `docs/compatibility-spec.md` §4). Tool results carry `structuredContent`, wrapped under `result` for every tool except `read_content` — matching FastMCP's rule for `str`/union return types. `tools/dump_reference_mcp.py` drives the reference server over stdio and captures `tests/golden/mcp/responses.json` (25 frames); `tests/mcp_golden.rs` replays every text surface character-for-character and every JSON payload field-for-field. Three parity bugs surfaced and were fixed: `entity.updated_at` was being inserted as `created_at`, file times were truncated to whole seconds when the reference keeps microseconds, and descending directory sorts reversed ties instead of using a stable reversed comparator. One divergence is deliberate and pinned by a test: this port's resolver also accepts `<path>.md`, so `memory://notes/frontmatter` resolves here but not in the reference (whose note carries an explicit permalink). The `fetch`/`search` ChatGPT adapters landed in Phase 11b; the `_meta.fastmcp.*` marker stays intentionally unmirrored.
- **Phase 13 — Schema Operations (first slice):** `src/schema/` ports the whole `basic_memory.picoschema` package as five modules: `parser` (field keys with `?`/`(array)`/`(enum)`/`(object)` modifiers plus descriptions that may themselves contain commas, parentheses, or brackets — the key splitter scans right-to-left for the paren that actually closes the final modifier; enum values from either a YAML list or a quoted `[a, b], description` string; `parse_schema_note` with the reference's exact `ValueError` texts), `resolver` (inline mapping → explicit `schema:` reference → implicit match on the note's own `type` → nothing), `validator` (field → observation category or relation type; `present`/`missing`/`enum_mismatch`; unmatched categories and relations reported but never failing; `warn` records warnings while `strict` records errors and clears `passed`; `settings.frontmatter` keys validated with the same field rules), `inference` (presence counted once per note, array-ness when more than half of the notes containing a field contain it more than once, `Counter.most_common` tie behaviour, fields classified at the 95%/25% thresholds into a suggested mapping) and `diff` (new/dropped fields plus cardinality mismatches, reusing the inference analysis). `tools/dump_reference_picoschema.py` runs the reference interpreter over the pure `basic_memory.picoschema` functions and writes `tests/golden/schema/picoschema.json` (38 cases across parse, schema-note, parse-error, validate, infer, diff, and resolve); `tests/schema_golden.rs` replays all of them, including explicit key-order assertions for the two ordered outputs (`suggested_schema`, `unmatched_observations`). Enabling `serde_json`'s `preserve_order` feature was required: a schema mapping *is* ordered data, and a sorted `Map` silently reorders fields (all 138 pre-existing tests still pass with it on).
- **Phase 12–13 — Schema tool surface (second slice):** the three tools are now wired end to end. `src/schema/report.rs` mirrors `basic_memory.schemas.schema` (`ValidationReport`/`NoteValidationResponse`/`FieldResultResponse`/`TypeValidationSummary`, `InferenceReport`/`FieldFrequencyResponse`, `DriftReport`/`DriftFieldResponse`) with `skip_serializing_if` standing in for pydantic's `exclude_none=True`; `src/application/schema.rs` ports the router's report assembly (`_find_by_note_type` through `normalize_note_type`, `_schema_covered_note_types` including inline/explicit coverage and the first-spelling-wins display label, `_find_schema_entities` entity-then-reference matching, `_schema_frontmatter_from_file` where the **file** beats the index, the identifier path's permalink → title → path → search-hit chain, and the raw-YAML frontmatter read that keeps `version: 1` a number); `src/domain/note_type.rs` adds `to_snake_case`/`normalize_note_type`; `src/application/schema_text.rs` renders the three reports and the three guidance blocks character for character, and `src/application/schema_tools.rs` holds the shared guard chain so the MCP tools and the CLI classify a request identically. CLI: `auto-memory schema validate|infer|diff` prints the payload through `pycompat::python_json_dumps` (`json.dumps(indent=2, ensure_ascii=True)` byte-for-byte) with `--text` for the MCP text surface and `--strict` for a non-zero exit on validation errors. `tools/dump_reference_schema_mcp.py` captures three vaults (schema / no-schema / broken-schema) into `tests/golden/mcp/schema.json` — 30 `tools/call` frames plus 14 `bm tool schema-*` CLI runs; `tests/schema_mcp_golden.rs` and `tests/schema_cli_golden.rs` replay them byte for byte. Two parity findings from that comparison: non-`str` tool results must be **compact** JSON (this port had emitted pretty JSON), and `analyze_observations`/`analyze_relations` must walk each note's categories in **first-appearance** order rather than `most_common` order — the earlier pure-function golden had missed that tie-order rule.
- **Phase 11b — ChatGPT adapters (`search`/`fetch`):** the last two MCP tools are in. Both are gated on the `initialize` request's `clientInfo` (`name`/`title`, exactly `openai-mcp` or prefixed, case-insensitive): a neutral client gets a *successful* payload explaining the tool is OpenAI-only, an OpenAI client gets real results. `search` reuses the `search_notes` payload (`page=1, page_size=10`) and reshapes rows to `{id, title, url}` with `total_count` counting the page; `fetch` normalizes a path-like id to a memory URL, returns the note's raw markdown, derives its title from a leading `# heading` or `str.title()` of the last path segment, and flags a miss through `metadata.error`. Their result frame is unique in the server: `content[0].text` is the compact JSON of the *content list*, each item's `text` is `json.dumps(payload, ensure_ascii=False)`, and `structuredContent.result` is the list. `tools/dump_reference_chatgpt_mcp.py` captures both client identities into `tests/golden/mcp/chatgpt.json` (9 calls); `tests/chatgpt_mcp_golden.rs` replays every frame byte for byte and pins the gate. Comparing that capture surfaced a **search-parity gap**: our relaxation retry ignored the reference's guards, so a query the reference answered with zero results (`zzz-nothing-matches-this`) matched two notes here. `src/search/relaxation.rs` now ports `relaxed_query_words` in full — quoted/boolean rejection, the three-token minimum, the numeric-token guard, the two-word CJK branch, `RELAXATION_STOPWORDS` pruning, format/mark-aware tokenization (`unicode-general-category`), term de-duplication, and quoted FTS rendering for apostrophes.
- **Phase 14 — Obsidian Compatibility and Recovery:** done. `tests/obsidian_compatibility.rs` covers the nine checklist items that the indexer tests did not: an atomic save (Obsidian's temp-file-plus-rename write) keeps the note's id/permalink/`created_at` and produces exactly one indexed path, both via `VaultWatcher` and via a real `notify` loop; `.obsidian/` and `.basic-memory/` are invisible to the watcher *and* to a reconcile; a wikilink authored before its target exists stays unresolved (with no relation permalink) and resolves once the target note is created, refreshing the relation search row; renaming in the file explorer keeps the permalink so incoming links keep resolving; deleting a note removes its observations, search rows, and (via the reference's own `ON DELETE CASCADE` on `relation.to_id`) its incoming link rows, and a full rebuild restores the stranded link as unresolved — the "the vault is the only durable state" promise. `docs/usage.md` now documents install, Obsidian setup, indexing, MCP setup, querying, and recovery, which is also Phase 15's documentation deliverable.
- **Phase 15 — Hardening and Release (first slice):** `tests/hardening.rs` covers the security and robustness items. Path containment: `NoteService::resolve` used to probe the filesystem for any `*.md` identifier, so `../outside/secret.md` was readable — the identifier now has to be a safe project-relative path (`is_safe_relative_path`: no absolute prefix, no `.`/`..`/empty segment) before the filesystem branch, and everything else falls through to the index, whose paths come from walking the vault. The test reads, edits, deletes, writes, and moves through the real MCP server against a secret file placed beside the vault, asserts nothing leaked and nothing outside changed, and was confirmed to fail against the unfixed resolver. Robustness: an undecodable (non-UTF-8) note is skipped by `reindex` without taking the run down, and a dependency-free property test writes 24 pseudo-randomly generated notes twice, asserting the bytes are a fixed point. `cargo doc --no-deps` passes, and the five release commands in §17 run clean.
- **Phase 15 — Hardening and Release (second slice):** benchmarks, an offline smoke gate, and the release checklist. `tests/benchmarks.rs` (three `#[ignore]`d tests, run with `-- --ignored --nocapture`) generates a 400-note vault and reports full rebuild vs checksum-only reconcile vs 10%-touched incremental rates, search latency percentiles over 200 queries, and chunking throughput; the assertions are loose sanity floors so a catastrophic regression still fails without pinning machine speed. `tools/smoke.py` is the clean-machine check: offline release build, index, status, search, context, schema, a scripted MCP session (20 tools), then delete-the-index-and-rebuild with a SHA-256 of the vault taken before and after to prove indexing never rewrites the user's files — it prints `SMOKE OK`. `docs/release-checklist.md` collects the five quality commands, the compatibility evidence to regenerate, the two smoke commands with their baseline numbers, the inputs that live outside the repo (reference 0.23.2 interpreter, fastembed cache + ONNX Runtime), and the parked work. Baseline numbers on the development machine (debug): full rebuild ~450 docs/s, 400-note checksum pass ~54 ms, search median ~0.6 ms / p95 ~3.4 ms, chunking ~78k rows/s.
- **Phase 15b — Remaining parity items (semantic-leg filters, search-bound semantics):** the vector leg is now filtered like the reference. `_dispatch_retrieval_mode` hands the full filter set to both legs; the FTS leg applies it natively, and the vector leg ranks candidates and then intersects them with a filter-only scan (`search_text=None` + filters, `limit=VECTOR_FILTER_SCAN_LIMIT`=50000) keyed on `(type, id)`. `VectorSearchOptions` gained the filter fields, `src/search/vector.rs` implements `apply_row_filters`, and the CLI passes its text filters into the semantic legs. Two further divergences surfaced while capturing the new cases: (1) a category filter without an explicit `entity_types` must default to **observation** rows, because categories only exist there — `search::default_entity_types`, applied by the CLI and the MCP tool, pinned by the new `category-decision-implicit` golden; (2) `search`'s `after_date` is parsed by `dateparser`, not by the timeframe parser the context tools use — it yields naive local time that SQLite reads as UTC, so a relative bound is shifted by the local UTC offset (`src/domain/dateparser.rs` ports the supported subset with unit tests; the corpus uses absolute dates because relative ones are wall-clock dependent). The MCP `search_notes` tool also gained `entity_types`, `status`, `metadata_filters` (with the reference's `note_type` → `type` alias) and `after_date`; `tests/mcp_search_filters.rs` checks those against the same captured queries. Finally, this work corrected a claim in the release checklist: re-running the oracle is **not** byte-identical. The reference's concurrent indexing changes id order (no-query search order, graph/context rows) and its runtime moves semantic scores by up to 1.21e-4 between runs, so the semantic envelope is 5e-4 with ranking and `matched_chunk` still asserted exactly.
- **Phase 11c — `search_notes` output format, search types, and the semantic floor:** the tool was returning a JSON payload for every request, which meant `output_format="text"` (the default) and every `search_type` other than text were silently wrong. `src/application/search_text.rs` ports `_format_search_markdown`, the no-results line, the `# No Search Criteria` reply, the semantic-disabled guidance, and the generic `# Search Failed` template; `search_outcome` maps `search_type` onto the reference's fields and modes (title/permalink modes clear the text field, because `(title MATCH … OR content_stems MATCH …) AND title MATCH …` is a SQLite "unable to use function MATCH in the requested context" error); an unknown type returns the troubleshooting guidance naming the valid options; and the semantic modes are refused with the `Semantic Search Disabled` text unless a runtime is attached. `auto-memory mcp` gained `--embedding-fixture` / `--model-cache` / `--onnx-runtime`, so `search_type="vector"` ranks against the stored vector index for real. `tools/dump_reference_search_mcp.py` captures the nine branches into `tests/golden/mcp/search-types.json`, and `tests/mcp_search_types_golden.rs` (4 tests) replays them plus a configured-runtime semantic search.
- **Phase 11d — Note-tool surfaces (`write_note`/`edit_note`/`delete_note`/`move_note`):** a parameter audit against the reference's captured `tools/list` found several parameters that were accepted and ignored or missing, and the four tools' response surfaces were JSON-only. Now: `write_note` honours `note_type` and `tags` (content frontmatter still wins, explicit tags beat `metadata["tags"]`), writes a plain body verbatim while trimming a body that carried its own frontmatter, normalizes `/` to the project root, refuses an out-of-project directory with the reference's `SECURITY_VALIDATION_ERROR` payload, blocks an overwrite with `action: conflict` / `NOTE_ALREADY_EXISTS`, and returns `{title, permalink, file_path, checksum, action}` or the `# Created note` summary plus the session footer. `edit_note` became an upsert (missing identifier ⇒ created, `fileCreated: true`) with its own JSON payload and `# Edited note (<op>)` summary. `delete_note` returns `true`/`false` or `{deleted, title, permalink, file_path}`, and with `is_directory: true` deletes a subtree and reports the directory summary (both surfaces). `move_note` keeps `destination_folder` for notes but refuses it for directories (`DESTINATION_FOLDER_NOT_FOR_DIRECTORIES`), reports `DESTINATION_SAME_AS_SOURCE` and `Entity not found`, moves whole directories, and rewrites the destination file with the entity's identity so a note without frontmatter keeps its title. `read_note`'s JSON miss payload now carries `{title, permalink, file_path}`. `tools/dump_reference_note_mcp.py` captures 36 frames plus the post-session vault files into `tests/golden/mcp/note-tools.json`; `tests/mcp_note_tools_golden.rs` replays them all. One deliberate divergence, pinned in that test: the reference's MCP write path leaves duplicate `search_index` rows (same type+id, different rowids) — this port indexes once.
- **Phase 8c — Cross-encoder reranker:** the reference ships `reranker_enabled=False`, so the default corpus never exercised it; with it on, the vector and hybrid legs rescore their top `reranker_candidates` (20) rows with a fastembed cross-encoder (`jinaai/jina-reranker-v1-tiny-en`), replace those scores with the squashed relevance, and demote the tail onto the same `[0, 1]` scale (`floor / (index + 2)`). `src/runtime/rerank.rs` loads the reference ONNX model (fastembed's user-defined rerank path) and applies the reference's clamped sigmoid; `src/search/rerank.rs` ports the pool/tail/pagination flow and the candidate budget (`max(semantic_vector_k, candidates * 4) + tail_size * 10`). Both search legs take an optional `RerankRequest`, and the CLI/MCP gained `--reranker` (plus `--reranker-candidates`, `--reranker-max-chars`, `--reranker-fixture`). `tools/dump_reference_rerank.py` runs the reference with both switches on and captures four reranked searches; `tests/rerank_golden.rs` replays them through a fixture provider (keys are the document texts, so the document format is verified too) and, where the model is installed, through the real ONNX reranker — the Rust run reproduced the reference's page exactly.
- **Phase 15 — Hardening and Release (third slice):** the last two checklist items. `tests/properties.rs` (4 tests, no new dependencies — a small LCG generates the inputs) asserts parser invariants over 200 generated notes (`parse → render → parse` is a fixed point for title/type/tags/body/observations/relations, rendering is stable, every bracketed line keeps its category, every wikilink line becomes a relation), permalink invariants (no spaces/underscores/backslashes, a known extension is stripped once, the reference's documented examples), and graph-traversal invariants over a generated cyclic vault (unique `(type, id, root_id)` rows because the reference groups by root, `depth > 0` so seeds are excluded, the `max_results` cap honoured, rows returned in `depth` order). `tests/storage_migration.rs` (3 tests) covers the index migration: a blank file gets every table and the current `schema_version`, reopening keeps rows while repairing a missing or stale version row, and migrating twice is a no-op. Both were written against the reference's own SQL (`GROUP BY … root_id`) rather than assumptions — an early version of the traversal property asserted global uniqueness and was corrected when the reference's query showed otherwise.
- **Completion audit:** all phases 0–15 are done. The whole golden corpus was regenerated from the reference after the last code change (27 files changed, every one in a known run-dependent category: index-id order, semantic float drift ≤1.21e-4, run metadata), and `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, `cargo doc --no-deps`, the benchmarks, and `tools/smoke.py` all pass against it. The last documented gap was closed too: `entity.created_at` now takes the row's insert time rather than the file ctime (measured against the reference; `tests/index_timestamps.rs`). Deliberately out of scope or accepted as an internal difference: `vec0` storage (BLOB tables + Rust scoring; exact KNN either way, results verified equal, only the reference's `bm inspect` reports the marker), the reference CLI's `bm inspect` retrieval diagnostics, and the Web/Cloud surfaces incl. `list_workspaces`.
- **Phase 10 — Note Mutation:** done for the local core. `src/markdown/serialize.rs` (frontmatter merge + PyYAML-compatible emitter + `write_atomic`) is verified byte-for-byte against the reference-normalized vault (`tests/note_golden.rs`, 16/16, including "skip malformed frontmatter" and "no update leaves bytes untouched"); `src/markdown/edit.rs` ports the reference edit operations and `_merge_metadata_into_markdown`, verified against `tests/golden/note/edit-operations.json` (22 cases captured from `note_preparation`, error messages included); `src/application/note.rs` adds `NoteService` (read/write/edit/move/delete, overwrite protection, automatic reindex, permalink preserved on move) covered by `tests/note_service.rs`. The CLI/MCP tool surfaces landed in Phase 11d, together with write-path goldens captured through the reference MCP tools.

## 1. Delivery Strategy

The project will be delivered in vertical slices. Every slice must produce a runnable, testable result. The implementation will not begin with MCP or UI; it will begin with the compatibility specification, test corpus, and deterministic core algorithms.

The order is:

```text
reference baseline
  -> compatibility corpus
  -> domain model
  -> parser
  -> storage
  -> indexing
  -> text search
  -> graph/context
  -> note mutation
  -> MCP/CLI
  -> semantic/hybrid search
  -> schema tools
  -> hardening/release
```

## 2. Phase 0: Lock the Reference Baseline

### Work

1. Record the upstream repository and documentation sources.
2. Pin an upstream commit or release.
3. Record the reference tool list.
4. Record the Markdown grammar.
5. Record project and `memory://` resolution behavior.
6. Record text, vector, and hybrid search parameters.
7. Record error, pagination, and output behavior.
8. Mark Web and Cloud features as explicitly out of scope.

### Deliverables

```text
docs/compatibility-spec.md
docs/data-format.md
docs/search-spec.md
docs/context-spec.md
docs/mcp-spec.md
```

### Exit criteria

- A fixed reference version exists.
- Every planned behavior has a written compatibility rule or an explicit open question.
- No implementation work depends on an unpinned upstream `main` branch.

## 3. Phase 1: Build the Golden Corpus

### Work

Create Markdown fixtures for:

- Plain notes.
- Frontmatter.
- Observations.
- Relations.
- Quoted relation types.
- Wikilinks and display labels.
- Unresolved links.
- Duplicate titles.
- Empty and malformed documents.
- Chinese, Japanese, Korean, and mixed-language content.
- Multiple projects.
- File creation, deletion, rename, and modification.
- Search filters, pagination, empty results, and invalid queries.
- One-hop, two-hop, and cyclic graph cases.

For every fixture, save the reference output for parsing, indexing, search, and context construction.

### Deliverables

```text
tests/fixtures/
tests/golden/
tests/common/
```

### Exit criteria

- The corpus covers every core algorithm.
- Expected output is versioned.
- A failing compatibility test gives a small, actionable diff.

## 4. Phase 2: Bootstrap the Rust Project

### Work

1. Convert the empty binary crate into a library plus binary.
2. Add the module structure from the specification.
3. Add `error.rs`, `config.rs`, and domain IDs.
4. Add format and lint configuration.
5. Add a minimal CI command set.
6. Add a `docs/` index and development instructions.

### Deliverables

```text
src/lib.rs
src/main.rs
src/domain/
src/error.rs
src/config.rs
rustfmt.toml
```

### Exit criteria

```bash
cargo check
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

all pass on the empty skeleton.

## 5. Phase 3: Implement the Domain Model

### Work

Implement:

- `ProjectId`;
- `EntityId`;
- `DocumentId`;
- `Permalink`;
- `RelationType`;
- `Project`;
- `Document`;
- `Entity`;
- `Observation`;
- `Relation`;
- `SearchQuery`;
- `SearchResult`;
- `ParsedDocument`.

Use newtypes for values with domain semantics. Keep storage row types separate from domain types.

### Exit criteria

- Domain types compile without storage dependencies.
- Serialization and equality behavior are tested.
- Invalid IDs, paths, and permalinks return typed errors.

## 6. Phase 4: Implement Markdown Parsing

### Work

1. Parse frontmatter.
2. Parse title, type, permalink, and tags.
3. Parse observations.
4. Parse relation lines.
5. Parse quoted multi-word relation types.
6. Parse bare and labeled wikilinks.
7. Parse wikilinks in prose.
8. Preserve unresolved targets.
9. Preserve original body content.
10. Compare every parser result with the golden corpus.

### Exit criteria

- Parser tests pass.
- Parser has no filesystem or SQLite dependency.
- Parse results are deterministic.
- CJK and mixed-language fixtures pass.

## 7. Phase 5: Implement SQLite Storage

### Work

1. Create migrations.
2. Create project, file, entity, observation, relation, and index tables.
3. Add repository modules.
4. Add transactions for single-document replacement.
5. Add file content hashes and parser/index versions.
6. Add schema migration tests.
7. Add database rebuild tests.

### Exit criteria

- Storage can save and load all domain objects.
- Re-running the same write is idempotent.
- A deleted database can be rebuilt from fixtures.
- Domain code does not depend on SQL row layout.

## 8. Phase 6: Implement Full and Incremental Indexing

### Work

1. Implement `index_file`.
2. Implement `remove_file`.
3. Implement rename handling.
4. Implement `reindex --full`.
5. Implement `reindex --incremental`.
6. Add content hashing.
7. Add event debounce.
8. Add `.obsidian/` and temporary-file exclusion.
9. Add periodic reconciliation.
10. Compare full and incremental index output.

### Exit criteria

- Full and incremental indexes are equivalent.
- Repeated saves do not duplicate data.
- Rename does not leave ghost entities.
- Delete removes derived records.
- A malformed file does not corrupt unrelated notes.

## 9. Phase 7: Implement Text Search

### Work

1. Create FTS5 tables.
2. Index titles, permalinks, body, observations, relations, and tags.
3. Implement query parsing.
4. Implement phrase search.
5. Implement Boolean operators.
6. Implement prefix matching.
7. Implement type, project, tag, category, and date filters.
8. Implement stable ordering and pagination.
9. Add CJK behavior tests.
10. Compare output and scores with the reference corpus.

### Exit criteria

- Text search fixtures pass.
- Empty and invalid queries behave correctly.
- Pagination is stable.
- CJK searches do not rely only on whitespace tokenization.

## 10. Phase 8: Implement Graph and Context

### Work

1. Resolve `memory://` URIs.
2. Resolve permalink to entity.
3. Load observations.
4. Load outgoing and incoming relations.
5. Implement bounded traversal.
6. Detect cycles.
7. Deduplicate nodes.
8. Apply depth and max-related limits.
9. Implement text and JSON rendering.
10. Compare context output with golden fixtures.

### Exit criteria

- One-hop, multi-hop, and cyclic graphs terminate correctly.
- Project boundaries are respected.
- Context output shape and ordering are compatible.

## 11. Phase 9: Implement Note Mutation Services

### Work

Implement one shared `NoteService` for CLI and MCP:

- `write_note`;
- `read_note`;
- `edit_note`;
- `move_note`;
- `delete_note`;
- `read_content`;
- `view_note`.

Support:

- append;
- prepend;
- find/replace;
- section replacement;
- insert before section;
- insert after section;
- overwrite protection;
- atomic file writes;
- automatic reindexing.

### Exit criteria

- Mutation fixtures pass.
- Failed mutations do not partially modify files.
- CLI and service results match.
- Index state is updated after successful writes.

## 12. Phase 10: Implement Local MCP

### Work

1. Add stdio transport.
2. Define input and output DTOs.
3. Register content tools.
4. Register search tools.
5. Register graph/context tools.
6. Register project tools.
7. Register diagnostics tools.
8. Map typed application errors to MCP errors.
9. Ensure stdout contains protocol data only.
10. Add request/response compatibility tests.

### Exit criteria

- An MCP client can initialize the server.
- Core read, write, search, and context tools work.
- MCP does not duplicate application logic.
- Stdio logs do not corrupt protocol output.

## 13. Phase 11: Implement CLI

### Work

Add:

```bash
auto-memory init <path>
auto-memory project list
auto-memory project add <name> <path>
auto-memory reindex
auto-memory reindex --full
auto-memory reindex --embeddings
auto-memory search <query>
auto-memory status
auto-memory doctor
auto-memory mcp
```

The CLI calls the same application services used by MCP.

### Exit criteria

- CLI supports text and JSON output.
- stdout is stable and scriptable.
- errors are readable and retain useful causes.
- no CLI command bypasses application services.

## 14. Phase 12: Implement Vector and Hybrid Search

### Work

1. Pin the embedding model and version.
2. Pin tokenizer and normalization.
3. Implement deterministic chunking.
4. Store model ID, dimension, and embedding hash.
5. Implement local vector retrieval.
6. Implement cosine similarity.
7. Implement threshold and candidate limits.
8. Implement reference hybrid scoring.
9. Implement stable tie-breaks.
10. Add model-change invalidation and re-embedding.

### Exit criteria

- Vector fixtures pass within documented floating-point tolerance.
- Hybrid result ordering matches the reference.
- Model changes trigger controlled re-embedding.
- Text search still works when embeddings are unavailable.

## 15. Phase 13: Implement Schema Operations

### Work

Implement:

- schema inference;
- schema validation;
- schema drift detection.

Rules:

- validation must not silently modify notes;
- inference must be deterministic;
- drift output must be stable;
- schema errors must identify the note and field.

### Exit criteria

- Schema fixtures pass.
- Invalid notes are reported without data loss.
- Inference and validation share the same domain representation.

## 16. Phase 14: Obsidian Compatibility and Recovery

### Work

1. Test editing from Obsidian.
2. Test atomic save behavior.
3. Test rename from the Obsidian file explorer.
4. Test deleting a note.
5. Test wikilink creation before target creation.
6. Test later target resolution.
7. Confirm `.obsidian/` is ignored.
8. Test full recovery after deleting `.basic-memory/`.
9. Test external file synchronization followed by reindex.

### Exit criteria

- Obsidian can be the only manual management interface.
- No Web page is needed for normal local operation.
- Reindex restores all derived state.

## 17. Phase 15: Hardening and Release

### Work

Run:

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo doc --no-deps
```

Also add:

- property tests for parser and graph traversal;
- regression tests for every fixed bug;
- database migration tests;
- path traversal tests;
- malformed UTF-8/error-path tests where applicable;
- performance benchmarks for indexing and search;
- offline installation test;
- clean-machine smoke test.

### Exit criteria

- All compatibility fixtures pass.
- All quality checks pass.
- Local operation does not require network access.
- Documentation describes installation, Obsidian setup, MCP setup, reindexing, and recovery.

## 18. Milestone Definition

### Milestone A: Parser Core

- Domain model.
- Frontmatter.
- Observations.
- Relations.
- Wikilinks.
- Golden parser tests.

### Milestone B: Rebuildable Local Index

- SQLite storage.
- Full reindex.
- Incremental index.
- Watcher.
- Reconciliation.

### Milestone C: Compatible Local Search

- FTS5 search.
- Filters.
- Pagination.
- Graph traversal.
- Context construction.

### Milestone D: Usable AI Integration

- Note mutation services.
- MCP stdio.
- CLI.
- Obsidian workflow.

### Milestone E: Algorithm Parity

- Vector search.
- Hybrid ranking.
- Schema operations.
- Full compatibility suite.

## 19. Risk Controls

| Risk | Control |
|---|---|
| Upstream behavior changes | Pin reference commit and update intentionally |
| Parser mismatch | Golden parser corpus |
| Search score mismatch | Fixed engine, model, and ranking parameters |
| Embedding drift | Pin model/version and store embedding metadata |
| Watcher event loss | Debounce plus periodic reconciliation |
| Index corruption | Transactional updates plus full rebuild |
| CLI/MCP divergence | Shared application services |
| Over-abstraction | Traits only at external boundaries |
| Unidiomatic Rust | rustfmt, clippy, review, and documented module rules |
| Scope expansion | Keep Web and Cloud explicitly out of scope |

## 20. Immediate Next Actions

1. Replace the Hello World binary with `lib.rs` plus `main.rs`.
2. Create the `domain` and `markdown` modules.
3. Add the first parser fixtures.
4. Define `ParsedDocument`, `Observation`, `Relation`, and `Permalink`.
5. Implement frontmatter parsing.
6. Implement observation and relation parsing.
7. Add golden parser tests.
8. Run formatting, clippy, and tests.
9. Only after parser behavior is stable, begin SQLite storage.
