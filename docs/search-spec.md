# Search Behavior — Compatibility Contract

Reference: `basic_memory` 0.23.2 (`docs/reference.md`). **[V]** verified in source, **[G]** golden capture pending.

## 1. Query entry points

- Exact permalink lookup.
- Glob permalink / path match (`*`).
- Title-only search.
- Full-text search (FTS5).
- Vector search.
- Hybrid search (FTS + vector fusion).

`SearchItemType`: `entity | observation | relation` (filter `entity_types`).
`SearchRetrievalMode`: `fts | vector | hybrid`. `[V] schemas/search.py`
This port does not model `SearchRetrievalMode`: `search_notes` picks a `SearchType`
(`src/application/search_text.rs`), and each leg takes its own options struct
(`TextSearchOptions` / `VectorSearchOptions`).

MCP `search_notes` exposes `search_type: text | title | permalink | vector | semantic | hybrid`
(default: config `default_search_type`, else `hybrid` when semantic enabled, else `text`).
`[V] mcp/tools/search.py:_default_search_type`; alias mapping `[G]`.

## 2. FTS5 index & scoring (V)

Table DDL in `models/search.py` (see `docs/data-format.md` §9). Tokenizer:
`unicode61 tokenchars 0x2F`, `prefix '1,2,3,4'` (path-aware prefix search).

- Score: `bm25(search_index)`; SQL orders `score ASC` (lower bm25 = better). `[V] sqlite_search_repository.py`
- Default strict semantics: all terms AND. Prefix wildcard appended to simple terms. `[G]` exact term prep.
- Boolean operators supported in text (`AND`, `OR`, `NOT`, parentheses, quotes). `[G]` exact grammar.
- FTS5 syntax errors are caught → empty result (not a crash). `[V]`
- Zero-result strict multi-word queries may retry with OR-joined relaxed terms when allowed
  (hybrid path opts in; service-level FTS has its own conservative fallback). Relaxation has
  detailed guard rules (≥3 Latin tokens; CJK ≥2 tokens; no numeric tokens; no quotes/booleans;
  stopword list). `[V] repository/search_query.py`

## 3. Vector search (V + G)

- Provider: fastembed local ONNX; model `BAAI/bge-small-en-v1.5`; dims 384. `[V]`
- Storage: sqlite-vec `vec0` virtual table `search_vector_embeddings(embedding float[384],
  +source_hash text)`; chunk metadata in `search_vector_chunks`. `[V] models/search.py`
- Chunking: deterministic, max 900 chars, overlap 120 chars; chunk_key `type:id:idx`;
  sha256 source hash; entity fingerprint. `[V] repository/semantic_chunking.py`
- Retrieval: L2 distance → cosine similarity `max(0, 1 - d²/2)` (vectors normalized). `[V]`
- `semantic_vector_k = 100` candidates; `semantic_min_similarity = 0.55` threshold
  (0.0 disables). Per-query `min_similarity` override. `[V] config_models.py`
- Semantic search can be disabled via config/env (`BASIC_MEMORY_SEMANTIC_SEARCH_ENABLED`).
  `[V]`

## 3b. Phase 8 / 8b status: chunking, ranking, and the ONNX runtime verified

- Chunking is ported and verified against `tests/golden/vector/chunks.json`
  (captured by `tools/dump_reference_vectors.py --no-embeddings`): 78 chunks with identical
  `(permalink, chunk_text, source_hash)` triples, no duplicate chunk keys.
- Implemented constants: `MAX_VECTOR_CHUNK_CHARS = 900`, `VECTOR_CHUNK_OVERLAP_CHARS = 120`,
  `semantic_min_similarity = 0.55`, `semantic_vector_k = 100`.
- Ranking keeps the best chunk per `(type, id)` row, filters below the similarity threshold,
  and breaks ties by chunk key.
- Fusion is implemented as `max(v, f) + 0.3 * min(v, f)` with FTS scores normalized by
  absolute maximum and the 0.0 gate — matching `FUSION_FORMULA_VERSION = "max+0.3*min/v1"`.
- **Runtime (8b):** `src/runtime/embedding.rs` runs the reference `model_optimized.onnx`
  (`qdrant/bge-small-en-v1.5-onnx-q`) from the fastembed cache through `fastembed` + `ort`
  (dynamic ONNX Runtime, CLS pooling, 384 dims, L2-normalized). The earlier "hang" was
  `huggingface_hub` revalidating over the network; with the model files loaded locally nothing
  touches the network. Vectors agree with the Python runtime to ≤ ~2.3e-4 per component
  (≈1e-5 cosine) — the documented, ranking-preserving envelope for int8-quantized weights
  across two ORT bindings. `tests/embedding_runtime.rs` skips when the cache or a loadable
  runtime is absent.
- **Reference vectors:** `tests/golden/vector/embeddings-reference.json` (chunk texts + the
  `local index` / `rust` queries) lets `FixtureEmbeddingProvider` replay exact reference
  vectors. `tests/vector_golden.rs::vector_search_replays_reference_scores` runs the whole
  downstream path (chunk→row mapping, cosine, 0.55 threshold, best chunk per row, entity-only
  filtering, ordering, `matched_chunk`) and reproduces `tests/golden/search/vector-local-index.json`
  rank-for-rank and `matched_chunk`-exact, with scores inside 1e-4 — the envelope set by the
  reference's own run-to-run variation (measured 4.7e-5), not by our arithmetic.
- **`matched_chunk` for vector hits:** the row's own `content_snippet` when it is at most
  `SMALL_NOTE_CONTENT_LIMIT = 2000` characters; otherwise the best
  `TOP_CHUNKS_PER_RESULT = 5` chunk texts joined by `\n---\n`.
- **Storage and CLI (8b):** `search_vector_chunks` + `search_vector_embeddings` mirror the
  reference tables, except that vectors are BLOBs scored in Rust instead of a sqlite-vec
  `vec0` table (`vector_index = 'blob'`); the reference does exact KNN too, so the math is the
  same (`d² = 2 - 2·cos` for normalized vectors). `reindex --embeddings` embeds only chunks
  whose `source_hash` changed (reference upsert semantics) and replaces the project's vector
  rows transactionally. `search --vector|--hybrid [--min-similarity F]` runs the legs and
  `--embedding-fixture <json>` replays captured vectors offline.
- **Verified end-to-end:** `tests/vector_golden.rs::stored_vector_index_replays_reference_search`
  builds the vector index through `IndexService::reindex_embeddings`, then matches
  `search/vector-local-index.json` **and** `search/hybrid-rust.json` (rank order, `matched_chunk`
  exact, scores within 1e-4); `tests/cli_golden.rs` does the same through the CLI. The live ONNX
  path was also run manually (`reindex --embeddings` + `search --vector/--hybrid`), producing the
  same order with scores inside the same 1e-4 envelope.
- **Still open (8b):** the reranker (reference default off), vector-leg filters beyond
  `entity_types` (the reference prefilters those through an FTS pass), and `vec0`-level storage
  parity.

## 4. Hybrid fusion (V)

In `search_repository_base.py`:

1. FTS scores normalized to [0,1]: `abs(score) / max_abs`; gate threshold 0.0.
2. Vector similarity used raw (already [0,1]).
3. Fusion keyed on `(type, id)` (separate id sequences per type): `[V]`
   `fused = max(v, f) + 0.3 * min(v, f)` — version `max+0.3*min/v1`.
4. Sort by fused score descending; dual-source rows in [0,1.3], single-source in [0,1].
5. FTS-only results lacking a vector matched chunk fall back to `content_snippet` as
   `matched_chunk`.
6. Optional reranker (default off): cross-encoder over top `reranker_candidates=20`, each text
   capped at `reranker_max_document_chars=2000`.

## 5. Filters (V)

- `note_types` (frontmatter `type`), `entity_types` (entity/observation/relation),
  `categories` (exact observation category), `after_date`, `metadata_filters`
  (structured frontmatter JSON), `tags`, `status` (frontmatter), `project`/`project_id`.
- `title`, `permalink` (exact), `permalink_match` (glob) query fields.

## 6. Pagination & response (V)

Repository: `limit=10`, `offset=0`. Search response model `SearchResponse`: `[V] schemas/search.py`

```json
{
  "results": [ SearchResult ],
  "current_page": 1,
  "page_size": 10,
  "total": 0,
  "total_is_exact": true,
  "has_more": false
}
```

`SearchResult` fields: `id`, `title`, `type` (entity|observation|relation), `score`, `entity`
(permalink), `external_id`, `permalink`, `content`, `matched_chunk`, `file_path`, `updated_at`,
`metadata`, `entity_id`, `observation_id`, `relation_id`, `category`, `from_entity`,
`to_entity`, `relation_type`. `[V]`

## 6b. Golden-verified behaviors (0.23.2 corpus)

Captured in `tests/golden/search/`:

- **Query-based searches do not compute an exact total**: full-text, vector, and hybrid
  responses return `total: 0`, `total_is_exact: false`, `has_more: true` even when the page
  holds 10 results.
- **Filter-only searches compute exact totals**: filter-only cases (entity type/category,
  metadata, tag, title, type, permalink glob) returned `total_is_exact: true` and a correct
  `has_more`. Title search also reports an exact total.
- **Permalink glob matches the stored (project-prefixed) permalink**: with
  `permalinks_include_project=true`, `--permalink "projects/*"` matched 0 rows while notes live
  at `oracle/projects/alpha`; globs must account for the project prefix.
- **Relaxation is observable**: the hyphenated nonsense query `zzzz-not-present` returned 10
  relaxed hits instead of an empty page — relaxation eligibility/tokenization needs its own
  focused fixtures before Rust matches it.
- Result items expose `entity`/`permalink`/`matched_chunk`/`score`/`metadata.note_type`;
  numeric ids in golden are canonicalized (`<entity_id>`) and are not contract values.

## 6c. Phase 7 implementation notes (byte-verified)

The reference query shape is **column-scoped**, not a whole-row match:

```sql
WHERE (search_index.title MATCH :text
       OR search_index.content_stems MATCH :text
       OR search_index.content_snippet MATCH :text)
ORDER BY bm25(search_index) ASC
```

- `content_stems` is **not stemmed**: it is `title variants + body + permalink variants + file-path variants + tags`,
  joined by newlines and capped at 6000 chars (`MAX_CONTENT_STEMS_SIZE`). The FTS5 tokenizer is
  `unicode61 tokenchars 0x2F` with prefixes `1,2,3,4`.
- The API returns the raw SQLite `bm25()` value, which is **negative** (most relevant first).
- Title search uses `search_index.title MATCH :title_text` with **no** prefix wildcard.
- Term preparation: `rust` → `rust*`; `emoji unicode` → `emoji* AND unicode*`;
  `zzzz-not-present` → `"zzzz-not-present"*`; `"source of truth"` → `"""source of truth"""*`
  (the reference escapes existing quotes).
- Permalink globs use `GLOB` (case-sensitive) and are not FTS-prepared; `projects/*` therefore
  matches nothing when generated permalinks carry the project prefix.
- Note-type filters compare `LOWER(json_extract(metadata,'$.note_type'))`.
- Zero-result multi-word queries retry with OR-joined prefix terms (relaxation).
- Verified: our scores and ordering equal the reference for `text-rust`,
  `text-case-insensitive`, `text-phrase`, `text-boolean`, `text-prefix`, `text-cjk`, and
  `title-alpha` (tolerance 1e-6).

## 7. Tie-break / stability (G)

Fusion sorting uses fused score only; equal-score ordering must be pinned by golden capture
(then reproduced with a documented secondary key, or exact-order fixtures).

## 8. Rust compatibility rules

1. FTS must use the same tokenizer semantics: `/` as token char and 4-char prefix support.
2. bm25 ordering direction must be preserved (lower first) even if bm25 itself differs; if
   exact bm25 values matter for hybrid normalization, run the same engine (SQLite FTS5).
3. Hybrid normalization + fusion formula are fixed (`max+0.3*min/v1`); no RRF / weighted
   average substitution.
4. Vector similarity conversion fixed (`1 - d²/2`, normalized vectors).
5. Chunking must be deterministic and identical to reference chunk keys for the same corpus.
6. Relaxation eligibility rules must match; exact stopword list is a fixture.
