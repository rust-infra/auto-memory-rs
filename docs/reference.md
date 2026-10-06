# Reference Baseline — 0.23.2

- **Status:** Verified (code-read) + partial golden capture pending
- **Date captured:** 2026-09-08 (UTC) / 2026-09-09 (local)
- **Version:** 0.23.2
- **Captured from:** the locally installed reference implementation (offline oracle)

## 1. Where the reference lives

| Item | Value |
|---|---|
| CLI binary | `~/.local/bin/basic-memory` (a symlink to `~/.local/share/uv/tools/basic-memory/bin/basic-memory`) |
| uv tool env | `~/.local/share/uv/tools/basic-memory/` |
| Package | `~/.local/share/uv/tools/basic-memory/lib/python3.13/site-packages/basic_memory` |
| Config dir (local) | `~/.config/basic-memory/` (`config.json`, `memory.db`(+WAL), `.bmignore`, `fastembed_cache/`, logs — created on first use, so a fresh install holds only the model cache) |
| Project layout | Markdown vault + `.basic-memory/config.json` (project config only) |
| Python | 3.13 (uv tool venv) |

These are machine-local paths. Re-read them before trusting the citations below: `basic-memory
--version` must say `0.23.2`, and the `site-packages` segment moves with the interpreter's minor
version whenever the tool is reinstalled — that is exactly how `src/runtime/embedding.rs` once
lost the ONNX Runtime (it hardcoded `python3.14`; see `docs/README.md`, productization pass).

The names in that table — and the `BASIC_MEMORY_*` variables and `.basic-memory/` directory used
below — are the upstream project's **own identifiers**, recorded verbatim. They are not this
project's naming and are renamed nowhere: the oracle harness sets `BASIC_MEMORY_CONFIG_DIR` and
reads the model cache under the reference config dir, `tests/common` maps our renamed surfaces
back onto the reference wording, and the file references in §3 are paths inside the installed
Python package. Renaming them here would break the trail this document exists to preserve; only
the prose around them is ours.

Local mode uses **SQLite** (aiosqlite) with FTS5; the app-level database `memory.db` lives in the
config directory, not inside the project. Projects registered in `config.json` under `projects`.
Cloud/Postgres paths are out of scope for `auto-memory-rs`.

## 2. Verification status legend

- **[V]** = verified by reading 0.23.2 source (file:line cited).
- **[G]** = needs Phase 1 golden capture from the running reference before Rust can assert parity.

## 3. Key source files (0.23.2)

| Concern | File (relative to `basic_memory/`) |
|---|---|
| Entity / observation / relation / note_content models | `models/knowledge.py` |
| Project model | `models/project.py` |
| Permalink generation, tag parsing, path utils | `utils.py` |
| Config models & defaults | `config_models.py`, `config.py` |
| Markdown frontmatter/content parsing | `markdown/entity_parser.py`, `markdown/markdown_processor.py` |
| Observation / relation grammar | `markdown/plugins.py` |
| Markdown schema DTOs | `markdown/schemas.py` |
| Search index DDL (FTS5, vector chunk tables) | `models/search.py` |
| SQLite search repository (FTS/vector/hybrid) | `repository/sqlite_search_repository.py` |
| Shared fusion/ranking logic | `repository/search_repository_base.py` |
| Search query relaxation rules | `repository/search_query.py` |
| Semantic chunk planning | `repository/semantic_chunking.py` |
| Embedding provider (fastembed) | `repository/fastembed_provider.py` |
| Search service | `services/search_service.py` |
| Context service (`build_context`) | `services/context_service.py` |
| Memory URL & context schemas | `schemas/memory.py`, `schemas/search.py` |
| MCP server & tools | `mcp/server.py`, `mcp/tools/*.py` |
| Picoschema | `picoschema/{parser,inference,validator,diff,resolver}.py` |
| CLI commands | `cli/commands/*.py`, `cli/main.py` |

## 4. Verified constants (V)

| Constant | Value | Source |
|---|---|---|
| Embedding model | `BAAI/bge-small-en-v1.5` (alias `bge-small-en-v1.5`) | `repository/fastembed_provider.py` |
| Embedding dimensions | 384 | `fastembed_provider.py`, `sqlite_search_repository.py:82` |
| Embedding provider | `fastembed` (ONNX, local) | `config_models.py` |
| semantic_vector_k | 100 | `config_models.py` |
| semantic_min_similarity | 0.55 | `config_models.py` |
| Hybrid fusion formula | `max(v, f) + 0.3 * min(v, f)` | `search_repository_base.py:90,2617-2624` |
| Fusion formula version | `max+0.3*min/v1` | `search_repository_base.py:91` |
| FTS gate threshold | 0.0 | `search_repository_base.py:92` |
| Reranker (default off) | `jinaai/jina-reranker-v1-tiny-en`, candidates 20, max chars 2000 | `config_models.py` |
| FTS5 tokenizer | `unicode61 tokenchars 0x2F`, `prefix '1,2,3,4'` | `models/search.py` (CREATE_SEARCH_INDEX) |
| FTS score | `bm25(search_index)`, `ORDER BY score ASC` | `sqlite_search_repository.py` |
| Vector distance→similarity | L2 from sqlite-vec `vec0`; `max(0, 1 - d²/2)` | `sqlite_search_repository.py:720-727` |
| Semantic chunk | max 900 chars, overlap 120 chars; chunk_key `type:id:idx`; sha256 source hash | `semantic_chunking.py:12-13,97-99` |
| build_context defaults | depth 1, page_size 10 (max 50), max_related 10 (max 100) | `schemas/memory.py` |
| permalinks_include_project | true (generated permalinks prefixed with project slug) | `config_models.py:625` + golden |
| ensure_frontmatter_on_sync | true (missing frontmatter is written back to the file) | `config_models.py:620` + golden |
| index_changes | true | `config_models.py:558` |
| update_permalinks_on_move | false | `config_models.py:553` |
| kebab_filenames / disable_permalinks | false / false | `config_models.py:588,593` |
| write_note_overwrite_default | false | `config_models.py:598` |
| format_on_save | false | `config_models.py:636` |
| Depth traversal scaling | each logical hop = 2 graph levels (relation→entity) | `services/context_service.py:327-333` |
| Memory URL | normalize to `memory://`; no `//`, no `://`, reject `< > " \| ?`, max 2028 | `schemas/memory.py` |
| Search item types | `entity`, `observation`, `relation` | `schemas/search.py` |
| Retrieval modes | `fts`, `vector`, `hybrid` | `schemas/search.py` |

## 5. Fact corrections vs. earlier drafts

1. **Index DB location**: reference stores the derived SQLite index in the **app config dir**
   (`~/.config/basic-memory/memory.db`), **not** inside `.basic-memory/index.sqlite3` in the vault.
   A project folder holds markdown plus `.basic-memory/config.json` (project config/logs only).
   `auto-memory-rs` must decide whether to mirror this or document a deliberate deviation.
2. **Entity per note file**: each markdown file is one entity; observations/relations are parsed
   from the file body (not separate files). `title` defaults to file stem; `type` defaults to `note`.
3. **Search modes**: exact permalink, glob permalink (`*`), title, and full-text are distinct query
   paths in addition to vector/hybrid (the older draft flattened them).

## 6. Phase 1 golden findings (verified by running the oracle)

Captured by `tools/export_reference.py` into `tests/golden/` (16 markdown fixtures,
15 indexed entities, 47 search rows, 78 vector chunks):

1. **Generated permalinks are project-prefixed** (`oracle/notes/simple`) because
   `permalinks_include_project` defaults to true. An explicit frontmatter `permalink`
   is kept verbatim (`notes/frontmatter-note`).
2. **Missing frontmatter is injected on sync** (`ensure_frontmatter_on_sync=true`):
   files without frontmatter are rewritten with `title` (file stem), `type: note`, and a
   generated permalink. The normalized vault is captured at `tests/golden/vault/`.
   This means the reference mutates user markdown during indexing — a decision point for
   `auto-memory-rs` (mirror vs. read-only default).
3. **Malformed YAML frontmatter files are skipped** (parser warns, batch normalization
   fails); they are absent from the entity table.
4. Observation `category` defaults to `note` when the bracket form is absent;
   task markers (`[ ] [x] [/]`), transcript timecodes (`[00:01:02]`), blockquote callouts,
   markdown links, and wikilink-only lines are excluded.
5. Prose/`[[...]]` links become `links_to` relations; quoted multi-word labels
   (`"implemented by"`) survive intact; nested paths keep slash-separated permalinks.
6. Search results carry `entity`/`permalink`/`matched_chunk`/`score`; vector similarity for
   a CJK note scored 0.648 with the pinned model; hybrid results exist for the same corpus.
7. Numeric DB ids are not stable contract values (canonicalized in golden); `external_id`
   UUIDs and permalinks are.

## 6b. Storage-layer parity findings (Phase 5)

- Rebuilding the fixture vault produces **15 entities / 16 observations / 16 relations**
  — exactly the reference projection — because the malformed-frontmatter file is skipped.
- Generated permalinks keep interior hyphens (`duplicates/dup-a/same-title`), are lowercased,
  and get the project prefix; explicit frontmatter permalinks stay verbatim.
- Relation targets are stored raw (`to_name`), then resolved to `to_id` when a matching
  permalink/file path exists.
- `external_id` in `auto-memory-rs` is a deterministic UUID-v4-shaped value derived from
  `project_permalink + file_path`. The reference uses random UUIDs persisted in the DB; numeric
  and external ids are explicitly outside the compatibility contract (see
  `specs/compatibility-spec.md`).

## 6c. Incremental-indexing findings (Phase 6)

- `update_permalinks_on_move` defaults to **false**: renaming a file updates `file_path` but keeps
  the existing permalink. Setting it true recomputes generated permalinks from the new path.
- Re-indexing compares checksums: unchanged files are skipped (`unchanged`), changed files are
  updated in place, and rows whose file disappeared are pruned.
- Malformed frontmatter files are skipped on every pass and never disturb healthy rows.
- Incremental reconciliation converges on the same projection as a full rebuild (verified by
  `tests/incremental_golden.rs`).

## 6d. Search-layer findings (Phase 7)

- The reference search runs **directly against the FTS5 table** (no join) because SQLite cannot
  use column-scoped `MATCH` with a join in the same query level; entity lookups are filled in
  afterwards. `auto-memory-rs` mirrors this with subquery filters plus a result lookup.
- `content_stems` is a legacy name: the value is a concatenation of text variants
  (`_generate_variants`: original, lowercase, path segments, words) plus the body, permalink,
  file path, and tags — no stemming. Truncated at 6000 chars.
- API scores are raw `bm25()` values (negative; lower is better).
- Default `search_notes` results are **entity rows only**; observation/relation rows require an
  explicit `entity_types` filter.
- Text queries report exact totals (`total_is_exact = true`), unlike the hybrid path which
  returned `total = 0 / total_is_exact = false` in the Phase 1 capture.
- The reference's `default_search_type` decides whether un-flagged searches are text, vector, or
  hybrid; the oracle harness now pins it to `text` so text goldens stay pure FTS5.
- **The zero-result relaxation retry is gated on query *shape*, not just length.** `_relaxed_fts_text`
  builds `word* OR word* …`, but only after `relaxed_query_words` clears four guards: quoted or
  explicit-boolean queries never relax; fewer than **three** word tokens never relax; any numeric
  token never relaxes (`SPEC 16` is an identifier); and whitespace-separated **CJK** terms relax
  from **two** words up (they are not space-delimited the way the token guard assumes). Words are
  counted with `relaxation_word_tokens`, which keeps combining marks, word-internal format
  characters, and apostrophes *inside* a token — counting those as separators would cut abugidas
  and decomposed text into fragments and let one word clear the three-token guard. A
  `RELAXATION_STOPWORDS` list (`the`, `this`, `when`, …) is pruned before the OR is built, and a
  term containing an apostrophe is emitted as a quoted prefix (`"don't"*`) because a bare
  apostrophe is FTS5 syntax, not text. `src/search/relaxation.rs` ports all of it, including the
  Cf/`Mn`/`N*` classification via `unicode-general-category`.
- **`search` and `recent_activity` parse "recent" differently.** `build_context`/`recent_activity`
  resolve a timeframe with `parse_timeframe` (aware local time, minimum one-day lookback), but
  `SearchQuery.after_date` goes through `dateparser.parse`, which returns a **naive** local
  timestamp; the reference binds it into `datetime(updated_at) > datetime(:after_date)`, and
  SQLite reads the naive value as UTC. The bound is therefore shifted by the local UTC offset:
  `after_date="1d"` means "one day *plus the offset* ago" in practice. `src/domain/dateparser.rs`
  reproduces the subset (`<n><unit>`/`<n> <unit>`, optional `ago`, `now`/`today`/`yesterday`, and
  absolute `YYYY-MM-DD[ HH:MM[:SS]]`), and an unparsable value means "no date filter" exactly as
  `dateparser` returning `None` does. Relative bounds are wall-clock dependent, so the corpus pins
  the behaviour with absolute dates (`after-date-absolute-rust`, `after-date-future-rust`) and a
  unit test covers the relative arithmetic.
- **A category filter changes the implicit `entity_types` default.** After deciding a request has
  criteria, the reference defaults `entity_types` to `["observation"]` when `categories` was
  supplied and to `["entity"]` otherwise — categories only exist on observation rows, and the
  entity default would AND the category against a `NULL` column and return nothing. This is why
  `search_notes(query="rust", categories=["decision"])` returns one observation row here and would
  return nothing without the default (`category-decision-implicit` in the corpus).
- **`metadata_filters` aliases `note_type` to `type`.** The MCP tool rewrites the model-column name
  before searching, so `metadata_filters={"note_type": "project"}` means `type: project`.

## 6e. Context-layer findings (Phase 9b)

- **The CLI always supplies a timeframe.** `bm tool build-context` defaults `--timeframe 7d`
  and the MCP tool defaults `timeframe="7d"`; the resolved `since` is applied as a *string*
  comparison against `entity.created_at` in the traversal. This is what removes
  `notes/frontmatter` (created `2026-01-02`) from the related entities of `notes/relations`
  while leaving the relation row that points at it.
- **`entity.created_at`/`updated_at` come from the parse layer**, not from index time:
  frontmatter `created`/`modified` when present, else `st_ctime`/`st_mtime`. All
  `search_index` rows of an entity copy those values.
- **Relations are inserted in sorted order.** `RelationGenerationPublisher.publish` keys
  relations by `(relation_type, to_name)` (first authored occurrence wins) and inserts them
  sorted, so a note's relation ids follow lexicographic order rather than document order.
  `observations` keep document order.
- **`find_related` is a single recursive CTE** (`_build_sqlite_query`) that emits relations at
  odd depths and entities at even depths, dedupes with `MIN(depth)`, and returns
  `ORDER BY depth, type, id LIMIT max_related`. `auto-memory-rs` ports this query verbatim
  (`Store::find_related`) and a golden replays it against the reference ids.
- **Related-results order is run-dependent.** The reference indexes files concurrently, so
  entity/relation ids differ between runs (two runs of the same vault produced different
  orders), and with them the `LIMIT` cut, the tail of `related_results`, and
  `total_relations`/`total_observations` for truncated cases. Only the rules — not the tail —
  are reproducible; see `specs/context-spec.md` §3.3.
- **Related entities carry no content.** API hydration only fills `content` for the primary
  search row; related `EntitySummary` rows serialize `content: null`. Observation summaries
  take `title` from the owning entity and their synthetic
  `<entity permalink>/observations/<category>/<content>` permalink.
- **Empty results, not errors.** An unresolvable `memory://` URL (or a page past the first for
  a direct lookup) returns `results: []` with zeroed counts.
- **`bm tool build-context --json` is the only CLI shape.** The CLI passes
  `output_format="json"` and renders `--plain`/rich locally; the MCP markdown formatter
  (`_format_context_markdown`) is unreachable from the CLI, so the harness replays it over the
  captured payload (`tools/dump_reference_context_text.py`).

## 6f. Embedding-runtime findings (Phase 8b)

- **The model cache is enough to run offline.** `~/.config/basic-memory/fastembed_cache` holds
  the huggingface-hub layout for `models--qdrant--bge-small-en-v1.5-onnx-q` with
  `model_optimized.onnx`, `tokenizer.json`, `config.json`, `special_tokens_map.json`,
  `tokenizer_config.json`. The Python runtime only hung earlier because
  `huggingface_hub` revalidated over the network: with `HF_HUB_OFFLINE=1` (or the Rust
  `try_new_from_user_defined` path) it loads instantly. 384 dimensions, CLS pooling,
  L2-normalized output.
- **Python fastembed 0.8.0 + onnxruntime 1.29.0** produce the vectors the reference scores
  come from; the wheel ships `libonnxruntime.so.1.29.0`, which the Rust `ort` crate can load
  dynamically (`ort::init_from` / `ORT_DYLIB_PATH`).
- **Rust parity is close, not bit-exact.** Running the same graph and tokenizer through
  `fastembed` 6.0.3 + `ort` 2.0.0-rc.13 gives a per-component drift up to ~2.3e-4 on the
  int8-quantized weights (≈1e-5 in cosine similarity, two orders of magnitude below the
  smallest score gap in the corpus) and preserves ranking. Session options (threads,
  session-level optimizations) are the likely source; the exact reference vectors are kept in
  `tests/golden/vector/embeddings-reference.json` for the bit-exact path.
- **The reference's own vectors are not bit-reproducible either.** Re-embedding the same chunk
  corpus with the same Python runtime (fresh harness run, different batch composition) moved
  per-result cosine scores by up to **4.7e-5** versus the score the same run's index produced.
  Parity for vector scores is therefore asserted at 1e-4, with rank order and `matched_chunk`
  compared exactly.
- **`matched_chunk` for vector hits** is the row's own `content_snippet` when it is at most
  `SMALL_NOTE_CONTENT_LIMIT = 2000` characters, otherwise the best
  `TOP_CHUNKS_PER_RESULT = 5` chunk texts joined with `\n---\n`.
  `repository/search_repository_base.py` `[V+golden]`
- **Hybrid fusion** keys on `(type, id)`, normalizes FTS by `|bm25| / max`, gates below
  `FTS_GATE_THRESHOLD = 0`, uses vector similarity raw, and fuses with
  `max(v, f) + 0.3 * min(v, f)` (`FUSION_BONUS = 0.3`). `[V]`
- Vector search results carry `total = 0`, `total_is_exact = false`, `has_more = true` for
  query-driven searches (same as the text-query behavior noted in §6d).
- **Semantic pagination is a probe.** The API fetches `page_size + 1` rows for vector/hybrid,
  derives `has_more = len(results) > page_size`, then truncates; `total` stays `0` with
  `total_is_exact = false`. Candidate windows: `candidate_limit = max(semantic_vector_k,
  (limit + offset) * 10)`, and when the reranker is off the vector leg expands its own chunk pool
  to `candidate_limit * 10` (capped by `SQLITE_VEC_MAX_K = 4096`) before returning
  `candidate_limit` rows for fusion. `[V]`
- **Chunk storage.** `search_vector_chunks` (one row per chunk with `entity_id`, `chunk_key`,
  `chunk_text`, `source_hash`, `entity_fingerprint`, `embedding_model`, `vector_index`,
  `embedding_status`, `updated_at`) plus a `vec0` companion table `search_vector_embeddings
  (embedding float[384], +source_hash text)` keyed by the chunk rowid. Vectors are reused while
  `(entity_id, chunk_key, source_hash)` matches. `[V]`

## 6g. Filesystem-watcher findings (Phase 6b)

- **Debounce window** is `index_delay = 1000 ms` (`config_models.py`); the watch service
  passes it straight to `watchfiles.watch(debounce=…)`, so one quiet window produces one
  sync pass. `[V]`
- **Ignore rules** come from `DEFAULT_IGNORE_PATTERNS` (dotfiles/`.*`, `*.db`, `config.json`,
  VCS dirs, Python/Node build artifacts, IDE dirs, `.obsidian`, temp files) plus the user's
  `<data dir>/.bmignore` and the project `.gitignore`; `should_ignore_path` matches directory
  patterns against any path part and globs against each part and the full relative path. `[V]`
- **Moves are detected by pairing a delete with a create of the same checksum**
  (`LocalWatchMoveProcessor`): the entity row is moved rather than deleted and re-inserted, so
  with `update_permalinks_on_move=false` the original permalink survives the rename. `[V]`
- The reference watcher only routes **markdown** files into the indexer; `.obsidian` and the
  vault's `.basic-memory` bookkeeping are filtered before that point. `[V]`

## 6h. Note-mutation findings (Phase 10)

- **Two write paths exist.** `MarkdownProcessor.to_markdown_string` rewrites a whole note
  (body + generated observation/relation sections); the *sync* path only merges frontmatter
  through `FileService.update_frontmatter_with_result`. `tests/golden/vault/` captures the
  latter: `{**current_fm, **updates}` → `yaml.dump(sort_keys=False, allow_unicode=True)` →
  `f"---\n{yaml}---\n\n{content.strip()}"`. Consequences: existing key order is preserved,
  new keys append, lists use block style, strings that would resolve to another YAML type are
  single-quoted (`'2026-01-02'`, `'true'`, `'3'`), and files end **without** a trailing newline.
- **Which files get updated** (`indexing/batch_indexer.py`): no frontmatter + `ensure_frontmatter_on_sync`
  → inject `title`/`type`/`permalink`; frontmatter without the resolved permalink → add just
  `permalink`; explicit permalink that already matches → leave the bytes alone; malformed
  frontmatter → refuse (`Refusing to update malformed frontmatter`).
- **Edit operations** (`services/note_preparation.py`): `append` adds a separating newline when
  needed; `prepend` inserts after the frontmatter (re-emitting it) or at the very top;
  `find_replace` counts non-overlapping occurrences and errors when the count differs from
  `expected_replacements`; `replace_section` prefixes `## ` when the header lacks `#`, strips a
  payload that repeats the header, skips fenced code when matching, appends when the section is
  missing, and — depending on `replace_subsections` — stops at the next equal-or-higher heading
  (or any heading); `insert_before/after_section` add a blank separator line only when the
  neighbouring line is non-empty. Duplicate headers are errors.
- **Metadata merge** (`_merge_metadata_into_markdown`): `title`/`type`/`permalink` are dropped
  (they have dedicated resolution paths), `null` values are rejected, and the body is reattached
  without reflowing — including the "no blank line after the closing fence" case.
- **Write semantics**: `write_note` refuses to clobber an existing file unless `overwrite` is
  set (`write_note_overwrite_default` is `false`); a new note's body defaults to `# <title>`.
  `[V]`

## 6h2. Timestamp findings (re-verified 2026-09-11)

- **`created_at` is the row's insert time, not a file time.** A fresh vault with one note
  (file written 09:11:50.939554, index run 09:11:53.419) gets `entity.updated_at =
  09:11:50.939554` (file `st_mtime`) and `entity.created_at = 09:11:53.419265` — later than both
  the file's ctime and mtime. `EntityParser` has a ctime fallback for its `created` field, but the
  value that lands in the row on the sync path is the insert default. An update never re-stamps it
  (`replace_document`'s UPDATE leaves the column alone, like the reference).
- `interval` handling, frontmatter overrides, and `search_index` copies of both values are
  unchanged from §6e.

## 6h3. SQLite connection profile (verified 2026-09-11)

- **The reference tunes every connection, not just the schema.** `db.py` hangs
  `_configure_sqlite_connection` off SQLAlchemy's `connect` event, so each checkout gets
  `journal_mode=WAL` (filesystem databases only — in-memory reports `memory`),
  `busy_timeout=10000`, `synchronous=NORMAL`, `cache_size=-64000`, `temp_store=MEMORY`,
  `wal_autocheckpoint=1000`, and configurable `mmap_size`/`page_size` that default to off.
  `bm`'s data directory listing even names the artifact (`memory.db` (+WAL)).
- **WAL is what makes a shared index work.** Under the rollback journal a reader that reopens
  the index in a loop holds `SHARED` across a writer's commit; the writer retries and can be
  starved past its timeout. That is the observed failure in this port: `tests/obsidian_compatibility.rs`
  `initial reconcile` returning `Sqlite(DatabaseBusy, "database is locked")` about once in ten
  full-suite runs, before `src/storage/store.rs` applied the profile. Confirmed by reproduction,
  not inference.
- **`busy_timeout` is parity, not the fix.** rusqlite installs its own 5 s default
  (`sqlite3_busy_timeout(db, 5000)` in `inner_connection.rs`), so the port's second writer
  already waited — it just gave up at 5 s where the reference waits 10. A raw probe connection
  measured `PRAGMA busy_timeout = 5000` and waited 5.005 s under a held `BEGIN IMMEDIATE`.
- Pinned by `tests/sqlite_profile.rs`: the exact profile through `Store::connection()`, and the
  shared-index write path (a writer waits for a lock released 400 ms later instead of failing,
  with a `busy_timeout=0` control proving the lock is really held).

## 6i. MCP-layer findings (Phase 11, second slice)

Captured by `tools/dump_reference_mcp.py` into `tests/golden/mcp/responses.json`; the replay lives
in `tests/mcp_golden.rs`. `[V]` unless noted.

- **FastMCP wraps by return type.** Every tool whose annotation is `str` or a union containing
  `str` gets `structuredContent: {"result": …}` plus `_meta.fastmcp.wrap_result`. `read_content`
  is annotated `-> dict[str, Any]`, so its payload appears as `structuredContent` directly. The
  `_meta` marker is server identity, not behavior, and is not mirrored.
- **`list_directory` collects with the glob gating inclusion only.** `_collect_nodes_recursive`
  appends a child when it matches `file_name_glob` and then recurses regardless, so
  `file_name_glob="*.md"` never hides files under a directory whose own name cannot match. This
  also means `dir_name="/"`, `depth=1`, `*.md` legitimately returns zero items — only directories
  are visible at that depth.
- **Sort tie behavior.** With `sort` unset the order is folders-first then `name.casefold`,
  `directory_path.casefold`, `directory_path`. With `sort` set, files sort by identity
  (`title or name`, path, `external_id`) and then by the requested key; the descending branches
  pass `reverse=True` to `list.sort`, which is stable and therefore leaves ties in identity order.
  Reversing the finished list instead flips ties and changes which items land on which page.
- **`entity.updated_at` is the frontmatter `modified` value or the file `st_mtime` captured
  during the scan — never the insert time.** `entity.created_at` is the frontmatter `created`
  value or the row's insert default, *not* `st_ctime`: in the captured index
  `notes/simple.md` has `created_at=22:49:27.154854` while the vault file's own mtime/ctime is
  `22:49:27.193879`, i.e. the row timestamp predates the file's existence. The scan therefore
  reads mtime before the normalizer rewrites the file.
  Because `updated_*` ordering reads mtimes, the golden ordering is only reproducible when the
  vault is copied with metadata preserved (`shutil.copy2`); the test helper mirrors that.
- **File times keep microseconds.** `FileService.get_file_metadata` goes through
  `datetime.fromtimestamp`, and the reference stores `%Y-%m-%d %H:%M:%S.%f`. Truncating to whole
  seconds collapses files written in the same second onto one timestamp and silently changes
  directory order.
- **`read_content` media types** come from `FileService.content_type`:
  `mimetypes.guess_type(name)`, `.canvas` forced to `application/json`, and `text/plain` as the
  fallback — so `.rs` is `application/rls-services+xml` and `.yaml` is `application/yaml`, neither
  of which takes the text branch. Text responses carry `; charset=utf-8`.
- **`view_note`** reads note text and wraps it in a pinned artifact template. `textwrap.dedent`
  computes a zero margin whenever the inserted markdown has a column-zero line, so the template's
  own indentation survives; only the outer `.strip()` runs.
- **`fetch` and `search`** are the ChatGPT compatibility adapters and return an
  "Unsupported MCP client" payload for any client that is not an OpenAI MCP client
  (`is_openai_mcp_client`). They are server-gated surfaces rather than local-core behavior.
- **`read_note(output_format="text")` is the reference default.** On a miss it first tries an
  exact title lookup (walking its own 10-per-page title pages, capped at 10 pages), then a text
  search whose hits render through `format_related_results`, and only with no hits at all
  `format_not_found_message`. The JSON surface is
  `{title, permalink, file_path, content, frontmatter}` where `content` is everything after the
  closing fence — the leading blank separator line included — unless `include_frontmatter=true`.
- **A bare path without an extension is not a valid identifier.** With
  `notes/frontmatter.md` carrying the explicit permalink `notes/frontmatter-note`,
  `read_note("memory://notes/frontmatter")` returns the empty JSON payload in the reference; it
  resolves here. The reference's accepted identifiers are a permalink, an exact file path, an
  exact title, or an external id.
- **Project tools are disabled in a constrained server.** `create_memory_project` and
  `delete_project` short-circuit on `BASIC_MEMORY_MCP_PROJECT` and return
  `# Error\n\nProject creation/deletion disabled …`, with a `PROJECT_CONSTRAINED` JSON variant for
  create. Since `auto-memory mcp` is always constrained, that refusal *is* the surface.
- **`recent_activity` is `build_context` with no `memory_url`.** The `/v2/.../memory/recent`
  route calls `ContextService.build_context(types=…, since=…, limit=page_size, offset=(page-1)*page_size,
  max_related=10)`; the primary rows come from `search(search_item_types=types, after_date=since,
  limit=page_size+1, offset=…)` and the traversal/hydration are shared with `memory://` context.
  Metadata echoes `uri: null` and the requested `types`; the MCP tool defaults `type` to
  `["entity"]` so a single hub note cannot fill the page with its observations and relations.
- **Recency means `updated_at DESC`, not title order.** With no query text the FTS `score` is a
  constant, so the `after_date` branch's `, search_index.updated_at DESC` decides the order — and
  `updated_at` is the note's file mtime (or frontmatter `modified`). The captured order over the
  fixture vault is exactly its mtime order: `dup-b`, `dup-a`, `unresolved`, `beta`, `alpha`.
  The date filter also uses `updated_at`, so a recently-edited old note still appears.
- **`list_memory_projects` reads `BASIC_MEMORY_MCP_PROJECT`** to detect the constrained-project
  mode; constrained servers return `Project: <name>` plus the single-project notice instead of a
  list. JSON returns `{projects, default_project, constrained_project}` where each entry carries
  `source`, `is_default`, workspace fields, `qualified_name`, and the `_sync_support_metadata`
  triple (`sync_supported`/`sync_reason`/`local_usage`).
- **`search` and `fetch` are gated on client identity, not on configuration.** Both adapters call
  `is_openai_mcp_client`, which reads the `initialize` request's `clientInfo` (`name`/`title`,
  trimmed, lowercased, exactly `openai-mcp` or prefixed `openai-mcp/`), falling back to the client
  info stored at initialize. A non-OpenAI client gets a *successful* payload explaining that the
  tool is OpenAI-only (`Unsupported MCP client`, plus `error_message` on search) rather than an
  error. `fetch` normalizes a path-like id into a memory URL (`notes/simple.md` →
  `memory://notes/simple.md`) and derives its title from a leading `# heading`, else from the last
  path segment through `str.title()` (`simple.md` → `Simple.Md`); a miss still returns a document
  with `metadata.error = "Document not found"`.
- **Those two tools return a *list* of content items, so their frame is the odd one out.**
  `content[0].text` is the whole content list serialized compactly with non-ASCII kept literal
  (`serde_json`), while each item's own `text` is `json.dumps(payload, ensure_ascii=False)` — i.e.
  Python's default `, `/`: ` separators. `structuredContent.result` is the list itself. Every
  other tool in the server returns `str | dict`, which is why this shape appears nowhere else.
- **`search_notes` defaults to markdown, and `search_type` picks the field or the mode.** The
  tool's `output_format` default is `text`, so the JSON payload has to be asked for; the text
  surface is `_format_search_markdown` (a `### title` block per hit plus a pagination footer), a
  `No results found …` line for a miss, and `# No Search Criteria` for a request with neither
  query nor filters. `search_type` maps the query onto exactly one target — `text` → `text`,
  `title` → the title filter, `permalink` → exact/`permalink_match` (glob when the query has `*`),
  `vector`/`semantic` → vector mode, `hybrid` → hybrid mode — and an unknown value raises
  `Invalid search_type '<x>'. Valid options: hybrid, permalink, semantic, text, title, vector`,
  which the tool renders through its generic `# Search Failed` template. Critically, mapping the
  query onto the title/permalink field *clears* the text field: asking SQLite for
  `(title MATCH … OR content_stems MATCH …) AND title MATCH …` fails with "unable to use function
  MATCH in the requested context" (FTS5 allows the OR group alone, or MATCHes combined by AND,
  but not an OR group ANDed with another MATCH). Semantic modes without an embedding runtime get
  the `# Search Failed - Semantic Search Disabled` guidance, not a silent text search.
- **The note-write family is upsert-shaped and answers differently per `output_format`.** All
  four tools default to a *text* summary and only return their structured payload when asked:
  `write_note` → `{title, permalink, file_path, checksum: null, action: created|updated}`, `edit_note`
  → `{…, operation, fileCreated}`, `delete_note` → `{deleted, title, permalink, file_path}`,
  `move_note` → `{moved, title, permalink, file_path, source, destination}`. `write_note` merges
  `note_type` into the frontmatter `type` and explicit `tags` over `metadata["tags"]`, while the
  *content's* own frontmatter stays authoritative for both; a plain body is written **verbatim**
  (trailing newline included) whereas a body that arrived with frontmatter is trimmed.
  `edit_note` **creates** the note the identifier names when it does not exist (reporting
  `fileCreated: true`), and `delete_note`/`move_note` switch to a whole subtree with
  `is_directory: true` — for a directory, `move_note` requires `destination_path` and answers
  `DESTINATION_FOLDER_NOT_FOR_DIRECTORIES` for `destination_folder`. `move_note` also rewrites the
  destination file with the *entity's* identity (title/type/permalink), so a note without
  frontmatter keeps its title instead of adopting the new filename, and the file's checksum
  changes. The refusals are structured, not transport errors: the directory guard answers
  `SECURITY_VALIDATION_ERROR` (and `/` means the project root), and a blocked overwrite answers
  `action: conflict` + `NOTE_ALREADY_EXISTS`.
- **The reference's MCP write path inserts duplicate `search_index` rows.** Creating a note through
  `write_note` leaves two identical rows (same `type`+`id`, different `rowids`) in its
  `search_index`, which the captured session's DB shows directly; a later search then returns that
  note twice and the duplicate consumes a page slot. This port indexes once — the duplication is a
  bug with no upside, so it is a deliberate divergence, pinned by
  `tests/mcp_note_tools_golden.rs` (suggestion lists are compared as sets, and the shared prefix of
  the pages must still match).
- **The reranker is off by default and rescored a fixed prefix.** `reranker_enabled=False` ships
  as a fastembed cross-encoder (`jinaai/jina-reranker-v1-tiny-en`), and enabling it requires
  `semantic_search_enabled` too. The flow: the top `reranker_candidates` (20) rows of the
  *stable* retrieval order form the pool, the untouched tail keeps its order, and each pool
  row's text is `body + "\n" + title` truncated to `reranker_max_document_chars` (2000) where
  `body` is the matched chunk, else the stored snippet. The cross-encoder's logit is squashed by
  a clamped sigmoid into `[0, 1]` and *replaces* that row's score; the tail is demoted to
  `floor / (index + 2)` so a raw retrieval score can never outrank a reranked row. Pagination
  re-scores the same pool and slices the requested page, so page two cannot reshuffle page one.
  A reranked vector and a reranked hybrid search return the same order, because both replace the
  fused/cosine scores with the relevance. `src/runtime/rerank.rs` (provider) and
  `src/search/rerank.rs` (flow) port it; `tools/dump_reference_rerank.py` captures four reranked
  searches into `tests/golden/search/rerank-*.json`, and `tests/rerank_golden.rs` replays the flow
  with a fixture provider plus the real ONNX model (skipped where the reranker model is not in
  the cache — set `AUTO_MEMORY_MODEL_CACHE` to point at one).
- **The semantic legs are filtered, and the two halves work differently.** `search` dispatches to
  the vector/hybrid paths with the full filter set. The FTS leg applies filters natively. The
  vector leg ranks its candidates first, then — only when a filter was requested — runs a
  *filter-only* FTS scan (`search_text=None` plus the filters, `limit=VECTOR_FILTER_SCAN_LIMIT`
  = 50000) and keeps the candidates whose `(type, id)` appears in that scan. `entity_types`
  participates in that check too, but intersecting on it is equivalent to filtering the ranked
  rows by type, which is what this port does instead. `src/search/vector.rs` implements the
  intersection and `tests/vector_golden.rs` + `tests/cli_golden.rs` replay captured filtered
  queries (`--type`, `--entity-type`, `--tag`, `--status`, `--category`).

## 6j. Picoschema findings (Phase 12–13, algorithm layer)

Captured by `tools/dump_reference_picoschema.py` into `tests/golden/schema/picoschema.json`;
replayed by `tests/schema_golden.rs`. `[V]`.

- **A schema mapping is ordered data.** Parsing walks the YAML mapping in document order, and both
  `suggested_schema` (frequency order) and `unmatched_observations` (first-appearance order) are
  dicts whose key order is visible in output. A sorted JSON map silently reorders them, so the
  Rust side runs `serde_json` with `preserve_order`.
- **Field-key parsing scans right to left.** `_split_modifier_suffix` only treats a trailing
  `(...)` as a modifier when the parenthesis closing it is the one paired with the final suffix,
  which is what keeps names like `risk(score)` and descriptions like
  `notes?(array, freeform (no format))` intact. An unrecognised modifier (`(list)`) leaves the key
  untouched rather than erroring.
- **Enum values arrive two ways.** As a YAML list (`[a, b, c]`), or as a quoted string
  `"[a, b], description"` — quoting is required whenever a description follows, and
  `_parse_enum_string` recovers both. A key-level description beats the value-level one.
- **`parse_schema_note` uses truthiness, not presence.** `not entity` and `not schema_dict` mean an
  empty `entity: ""` or an empty `schema: {}` raises the same `ValueError` as a missing key.
  `settings.validation` accepts `warn`, `strict`, and `error` (the early-guidance alias for
  strict); anything else raises with `repr` of the offending value.
- **Validation is a subset check, never a straitjacket.** Unmatched observation categories and
  relation types are reported (`unmatched_observations` counts them, `unmatched_relations` lists
  them) but never fail a note. Only a missing *required* field or an enum mismatch produces a
  diagnostic, and only `strict` mode records it as an error and clears `passed`.
- **Inference counts presence, not occurrences.** A field's percentage is
  notes-containing-it over notes-analyzed; array-ness is inferred when *more than half* of the
  notes that contain the field contain it more than once; relation target types come from the
  individual relations' resolved target types, and the suggested type is that most common target
  title-cased (or `string` when there is none). `Counter.most_common` ties keep first-appearance
  order, which the Rust port reproduces with a stable descending sort.
- **Drift reuses inference at a tighter sample size** (`max_sample_values=3`), flags undeclared
  fields at `>= 0.25` as new, declared fields below `0.10` as dropped (a field absent everywhere is
  reported as a synthetic zero-frequency row whose `source` follows whether the schema field was an
  entity ref), and reports cardinality mismatches in schema-field order.

## 6k. Schema tool-surface findings (Phase 12–13, tool surface)

Captured with `tools/dump_reference_schema_mcp.py` (three vaults: a schema vault, a vault with
notes but no schema notes, and a vault whose `settings.validation` is malformed) into
`tests/golden/mcp/schema.json`, and replayed by `tests/schema_mcp_golden.rs` +
`tests/schema_cli_golden.rs`.

- **Note types are canonicalized for comparison, not for storage.** `note_type` is written from
  the raw frontmatter string (`EntityFrontmatter.type` is a plain string property; the
  `NoteType` annotated type only guards the API write path), so `type: Person` and
  `type: person` coexist in the database. `_find_by_note_type` resolves the *canonical*
  `to_snake_case` form against every stored spelling, and the report echoes the canonical form:
  `schema_validate(note_type="Person")` reports `note_type: "person"` and covers `type: Person`.
- **Report payloads drop `null`s but keep empty lists.** `ValidationReport.note_type` is absent
  in all-types mode, `FieldResultResponse.message` is absent when there is none, and
  `FieldFrequencyResponse.target_type` only appears for relations with a resolved target — while
  `type_summaries: []`, `unmatched_observations: {}` and friends are always present.
- **All-types mode reports the *display label*, not the canonical spelling.** Coverage is
  `sorted(normalize(target)) → (first spelling seen, stored spellings)`, and the per-type loop
  iterates the display labels; standalone schema targets with no matching notes still appear as
  `total_entities: 0` → `- **<type>**: no notes` in the text form.
- **Schema definitions are read from the schema note's file**, not from `entity_metadata`, and an
  incomplete file (no `entity`, or `schema` not a mapping) falls back to the last indexed
  metadata. Because the file is re-parsed as raw YAML, `version: 1` stays an integer there while
  the indexed metadata would have stringified it.
- **Identifier mode is the link resolver, fuzzy tail included.** `resolve_link` walks permalink
  candidates → exact title → file path → `<path>.md` → path aliases → a best-hit search over
  entity rows. That last step is why `schema_validate(identifier="no-such-note")` resolves to an
  unrelated note (the search hit) and answers "No Schema Found for 'meeting'" rather than "no
  notes found". This port reproduces the search fallback for the schema path.
- **The report's types differ between the diff branches.** The success path echoes the caller's
  spelling (`DriftReport(note_type=note_type)`), the no-schema path uses the canonical form.
- **A malformed schema is a client error, not a crash.** `resolve_schema` raises, the router
  returns HTTP 400 with the parser's message as `detail`, `call_post` raises `ToolError(detail)`,
  and the tool renders `# Schema Validation Failed … Error validating schemas: <message>` — the
  message is the `parse_validation_mode` text verbatim, e.g. `Invalid settings.validation value
  'nonsense'; expected one of: 'warn', 'strict', or 'error' (alias for 'strict')`.
- **Percent formatting is Python's.** `f"{x:.0%}"` rounds half-to-even on the binary value:
  `0.125 → 12%`, `0.375 → 38%`, `2/3 → 67%`. `python_percent` reproduces it with
  `round_ties_even`.
- **`str.title()` is Python's.** `note_type` is title-cased inside the created-schema guidance
  snippet: `work_item → Work_Item`, `v2note → V2Note` (runs of cased characters define words).
- **Non-`str` tool results are compact JSON in the `content` text**, with non-ASCII kept raw
  (`{"note_type":"person", …}`, `中文` not `\u4e2d`), which FastMCP produces for dict/model
  returns. This port had been emitting pretty JSON in that field; the schema capture exposed it.
- **Per-note field order is first-appearance order.** `analyze_observations` /
  `analyze_relations` iterate `note_categories.items()` (a `dict`, so insertion order) and only
  the *global* counter is reordered by `most_common()`. Iterating the per-note counter by count
  instead (as this port did) silently swaps tie groups: with `[name] [role] [tags] [tags]
  [status]` the reference reports `name, status, role, tags, hobby`, not `…, tags, role, …`.
- **The CLI JSON surface is `json.dumps(..., indent=2, ensure_ascii=True, default=str)`**
  (`bm tool schema-validate|infer|diff`), i.e. two-space indent and `\uXXXX` escapes for every
  non-ASCII character (astral characters as surrogate pairs). `pycompat::python_json_dumps`
  reproduces it, and `tests/schema_cli_golden.rs` compares 14 captured runs byte for byte.

## 7. Open items requiring golden capture (G)

- Exact `search_notes` `search_type` alias mapping (`text|title|permalink|vector|semantic|hybrid` →
  retrieval modes), incl. `semantic` vs `vector` (CLI flags `--vector`/`--hybrid` captured).
- Exact FTS term-preparation edge cases (boolean syntax, quoting, prefix wildcarding, path terms)
  — summarized in `specs/search-spec.md`, exact strings to be captured as fixtures.
- Exact relaxed-query word lists and stopword list (`repository/search_query.py`).
- ~~Exact text (markdown) output of each MCP tool~~ — captured for `list_directory`,
  `read_content`, `view_note`, `read_note`, `recent_activity`, the project tools, and the three
  schema tools (see §6i, §6k). Still open: `search_notes`.
- Project/permalink resolution details incl. `update_permalinks_on_move`, `kebab_filenames`,
  `disable_permalinks`, `permalinks_include_project` behaviors.
- CLI text output formats for `status`, `doctor`, `reindex`, `orphans`, `project`.
- `write_note_overwrite_default` current config value.

### Captured since 0.23.2 Phase 9b

- Context text output: `tests/golden/context/*.txt` (CLI `--plain`) and `*.md` (MCP
  `output_format="text"`, replayed by `tools/dump_reference_context_text.py`).
- Reference graph ids + traversal rows: `tests/golden/index/graph-rows.json`,
  `tests/golden/context/find-related.json` (`tools/dump_reference_graph.py`).
- Reference MCP frames: `tests/golden/mcp/responses.json` (`tools/dump_reference_mcp.py`),
  replayed by `tests/mcp_golden.rs`.
- Schema tool frames + CLI output: `tests/golden/mcp/schema.json`
  (`tools/dump_reference_schema_mcp.py`), replayed by `tests/schema_mcp_golden.rs` and
  `tests/schema_cli_golden.rs`.
