# Alignment with basic-memory 0.24.1

This document records what changed when `auto-memory-rs` moved its behavior baseline from
the reference implementation **0.23.2** to **0.24.1**, why each change was made, and which
parts of the 0.23.2 golden corpus were deliberately left untouched.

The short version: every local-first feature 0.24.0 introduced is implemented, each with
tests. The cloud/workspace/Web surface stays out of scope, as it has been since the
project's first spec.

## 1. How the baseline works now

`tests/golden/` still holds the **0.23.2** capture. It is not re-recorded, for two
reasons: it is the only independent evidence that the parser, indexer and search still
match the reference, and re-capturing it would invalidate every regression signal at once.

What changed instead is the *comparison*. Where 0.24.0 intentionally changes an observable
value, the replay test names the change, strips exactly the field or assertion it affects,
and pins the new behavior in a separate test:

| Corpus | What the replay ignores | Where the new behavior is pinned |
|---|---|---|
| `search/*` | absolute bm25 scores | `tests/mcp_cjk_search.rs`, order still exact |
| `search/text-boolean.json` | the score value | `tests/mcp_boolean_queries.rs` |
| `mcp/note-tools.json` | `checksum`, `updated_at` | `tests/mcp_revision_guard.rs` |
| `mcp/responses.json` | the same two fields | `tests/mcp_resources.rs` |
| suggestion lists in a miss | rank order (membership kept) | the search tests above |
| `parse/reference-parse.json` | metadata scalar *types* | `tests/mcp_metadata_filters.rs` |

A golden value that a change did **not** explain still fails. That is the property worth
protecting: the corpus stays authoritative about everything 0.24.0 did not touch.

## 2. What was added, by upstream change

### Security and protocol

| Upstream | Change |
|---|---|
| GHSA-4gfv-c2mm-mvj2 | A loopback HTTP bind rejects a foreign `Host`/`Origin`. `--allow-host`/`--allow-origin` widen it for a reverse proxy; a non-loopback bind logs a warning instead. |
| #1804 | `build_context` `depth` is bounded to 1–5. |
| #1805 | `read_content` answers only PNG/JPEG/WebP images. |
| #1683, #1688, #1641 | A refused `write_note`/`edit_note`/`move_note` is an MCP tool error (`isError: true`). `delete_note` on a missing note still answers `false`. |

### Revisions and token economy

| Upstream | Change |
|---|---|
| #1552, #1642, #1719 | `write_note`/`edit_note` take `expected_checksum`; a lost race writes nothing and answers `NOTE_REVISION_CONFLICT`. `read_note` gains `include_content`. |
| #1586 | Both tools report the note's real checksum instead of `null`. |
| #686, #1702 | `search_notes(compact=True)` and `build_context(compact=True)` drop bodies; compact context carries no `observations` key. |
| #1501 | `read_note(start_line, end_line)` returns numbered lines and the next range. |
| #1455 | Search results carry `content_length` and `content_truncated`. |

### Retrieval

| Upstream | Change |
|---|---|
| #1676 | `metadata_filters` accepts `$eq`/`$ne`/`$gt`/`$gte`/`$lt`/`$lte`/`$in`/`$contains`/`$exists`, bare operator names, and several operators per field. |
| #1455 | Frontmatter metadata keeps its JSON types (numbers are numbers). |
| #1683 | A boolean query runs as full text and is evaluated with one table-level `MATCH`, so `NOT` applies to the whole note. |
| #1294 | Chinese, Japanese, Korean, Thai, Lao, Tibetan, Myanmar and Khmer are indexed by character unigrams and bigrams in a `cjk_grams` column. |
| #1426 | Observations carry a valid-time qualifier (`@effective`, `@valid`, `@occurred`, `@due`, `@mentioned`), filterable with `valid_at`, `valid_overlaps`, `time_kind`. |
| #1558 | `search_notes(projects=[...])` scopes to chosen projects; merged rows carry `project`/`project_id`; a filtered vector search over-fetches. |

### Tools and protocol

| Upstream | Change |
|---|---|
| #1398 | `note_section` table; `cat`, `grep`, `ls`, `find`, `tail`, `man` MCP tools and CLI verbs. |
| #610 | Notes answer `resources/read`; the manual ships as resources; a `man` tool. |
| #1145 | A `getting_started` prompt. |
| #1031, #1036, #1037 | Every tool carries `readOnlyHint`/`destructiveHint`/`idempotentHint`/`openWorldHint`. |

### Data model

| Upstream | Change |
|---|---|
| #993 | `locked: true` makes a note read-only to the API. |
| #1246, #1514 | An ordinary Markdown link to a project file becomes a `links_to` relation. |
| #1344 | `relation.to_id` is `ON DELETE SET NULL`, so a deleted target leaves its inbound links unresolved rather than erasing them. |
| #1608 | A runtime that supplies an author stamps `created_by`/`updated_by`. |
| #1455 | `entity.format` records a file's media type. |

### Maintenance

| Upstream | Change |
|---|---|
| #1006, #1512 | `import document` extracts CSV and text into a sidecar note plus a run note. |
| #1422 | `wiki status|validate|rebuild` projects `index.md`/`log.md` navigation pages. |
| #1550 | `okf export|check` writes and validates an OKF v0.2 bundle. |
| #1254 | `prune` removes index entries the ignore rules now exclude. |
| #1414 | `project index NAME`; `status` reports readiness per stage. |

## 3. What this port does differently, and why

Each of these is a deliberate difference, not a gap:

- **No PDF or Office text extraction.** The reference shells out to `pdf-inspector` and
  Microsoft's `markitdown`. This port recognises those formats and writes a sidecar that
  says the text was not extracted, naming the missing tool. An empty sidecar would read as
  "this document has no text", which is a lie a model would act on.
- **bm25 absolute scores differ from the 0.23.2 capture.** Adding the `cjk_grams` column
  changes FTS5's length normalization. Ranking, membership and `matched_chunk` are still
  pinned exactly; the score assertions now check the invariant that survives a rescale
  (negative scores, best-first order).
- **`--read-only` remains this port's own addition.** It hides the six mutating tools and
  refuses them; the annotations derive from the same predicate, so the hint and the
  enforcement cannot disagree.
- **No `--transport sse`.** stdio and Streamable HTTP are supported; the third reference
  transport is not.
- **Cloud, workspace and Web surfaces stay out of scope.** `list_workspaces`,
  `bm cloud *`, `bm workspace`, and `ci` are not implemented, by the project's original
  scope decision.

## 4. Test coverage

The alignment added 12 test files. Each is named for the surface it covers, and each drives
the real binary rather than a mock:

| File | Covers |
|---|---|
| `tests/mcp_tool_errors.rs` | `isError` on refused mutations |
| `tests/mcp_revision_guard.rs` | `expected_checksum`, `include_content`, checksums |
| `tests/mcp_token_economy.rs` | `compact`, bounded reads, content lengths |
| `tests/mcp_metadata_filters.rs` | filter operators and typed metadata |
| `tests/mcp_boolean_queries.rs` | boolean routing and whole-note `NOT` |
| `tests/mcp_cjk_search.rs` | unsegmented-script search |
| `tests/mcp_valid_time.rs` | valid-time qualifiers and filters |
| `tests/mcp_project_scope.rs` | `projects`, attribution, unknown names |
| `tests/mcp_posix_tools.rs` | `cat`/`grep`/`ls`/`find`/`tail`/`man` |
| `tests/mcp_resources.rs` | resources, prompts, annotations |
| `tests/data_model_0_24.rs` | `locked`, Markdown links, unresolved inbound links |
| `tests/cli_posix.rs`, `tests/cli_ingest.rs`, `tests/cli_wiki_okf.rs`, `tests/cli_maintenance.rs` | the CLI verbs |

Unit tests live beside the code they pin: `src/application/{lines,posix,ingest,wiki,okf}.rs`,
`src/search/{filters,ngram}.rs`, `src/markdown/sections.rs`, `src/domain/valid_time.rs`,
`src/application/manual.rs`.

The suite is now **53 test binaries / 643 tests**, all passing, and
`./scripts/check-rust.sh` (format, clippy `-D warnings`, the full suite, the doc build and
the DSH plugin check) is green.
