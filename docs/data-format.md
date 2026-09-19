# Basic Memory Data Format — Compatibility Contract

Reference: `basic_memory` 0.23.2 (see `docs/reference.md`). Status **[V]** = verified in source,
**[G]** = golden capture pending. Rust implementation must reproduce the semantics below.

## 1. Project and filesystem layout

- A **project** = a registered directory (vault) containing markdown files.
- Project registry + app-level SQLite index live outside the vault (app data dir, e.g.
  `~/.config/basic-memory/`); each vault has `.basic-memory/config.json` (project config/logs).
  `[V] models/project.py`, `config.py`
- `.obsidian/`, `.tact/`, and other non-note dirs are not indexed as notes; `.bmignore`
  (config-dir file) holds ignore patterns. `[G]` exact ignore rules.
- Suggested vault (Obsidian-compatible): any folder layout; Obsidian is the manual editor.

## 2. Entity = one markdown file

Each note file is parsed into one `Entity` plus derived `Observation` and `Relation` rows. `[V] models/knowledge.py`

Entity fields of contract interest: `external_id` (UUID), `title`, `note_type`, `permalink`
(URL-friendly id, nullable), `file_path` (relative, slash-separated), `content_type`
(`text/markdown`), `checksum`, `mtime`, `size`, `created_at`, `updated_at`.

Uniqueness: `(permalink, project_id)` unique when non-null; `(file_path, project_id)` unique.
`[V] models/knowledge.py`

## 3. Frontmatter parsing

- YAML frontmatter via python-frontmatter. Malformed YAML → file treated as plain markdown
  (warning), never a hard failure. `[V] markdown/entity_parser.py`
- Values normalized: dates→ISO strings, numbers→strings, booleans→"True"/"False", lists/dicts
  recursive. `[V] entity_parser.normalize_frontmatter_value`
- `title`: coerced to string; missing/empty/"None" → file stem. `[V] entity_parser.py`
- `type`: default `note`. `[V] entity_parser.py`
- `tags`: string or list, parsed (see §5). `[V] entity_parser.py`
- `created` / `modified`: ISO 8601 from frontmatter override file times; naive → local tz.
  `[V] entity_parser.py`
- Storage: `entity.updated_at` is the frontmatter `modified` value, else the file `st_mtime`
  captured during the scan — verified against a live reference index where the vault file mtime
  was 4 s *older* than the row (the normalizer rewrote the file after the metadata was read).
  `entity.created_at` is the frontmatter `created` value, else the entity row's own insert
  default: in the same capture `created_at` (22:49:27.1525) preceded the *file copy* time
  (22:49:27.1938), which rules out `st_ctime`. The DB layout is `%Y-%m-%d %H:%M:%S.%f` and file
  times keep microseconds; truncating to whole seconds collapses same-second files and changes
  `sort="updated_*"` directory ordering. Every `search_index` row owned by the entity copies
  those two values (`created_at=entity.created_at`). `[V+golden]`
  **Confirmed by direct measurement** (fresh one-note vault, file written 09:11:50.939554, index
  run 09:11:53.419): `entity.updated_at` = `09:11:50.939554` (the file mtime) while
  `entity.created_at` = `09:11:53.419265` (the row's insert time — later than both the file's
  ctime and mtime, so it is not a file-time fallback at all). `basic-mem-rs` matches this now:
  `src/indexing/document.rs` takes "now" for `created` and the file mtime for `modified`, and
  `tests/index_timestamps.rs` pins the ordering plus the "an update does not re-stamp
  `created_at`" rule.
- `permalink`: optional explicit frontmatter permalink; kept verbatim. Otherwise derived from
  file path and prefixed with the project permalink when `permalinks_include_project=true`
  (reference default), e.g. file `notes/simple.md` → `oracle/notes/simple`. `[V+golden]`
- Missing frontmatter: with `ensure_frontmatter_on_sync=true` (default) the reference writes
  `title` (file stem), `type: note`, and the generated `permalink` back into the file during
  indexing; the post-sync vault is captured in `tests/golden/vault/`. `[V+golden]`
- Malformed YAML frontmatter: parser warns and falls back to plain markdown, but the batch
  normalizer fails and the file is **skipped** (not indexed). `[V+golden]`

## 4. Permalink generation

`generate_permalink(path)` in `utils.py`. Rules (verified examples in docstring): `[V]`

- Convert to posix path.
- Real file extension (via mimetypes) is stripped; periods in version numbers preserved.
- Spaces and underscores → hyphens; lowercase Latin; non-ASCII preserved.
- CJK: ideographs/symbols preserved; fullwidth punctuation dropped; Latin accented chars
  transliterated; hyphens inserted between CJK and Latin/digit transitions.
- Examples: `docs/My Feature.md` → `docs/my-feature`; `specs/API (v2).md` → `specs/api-v2`;
  `中文/测试文档.md` → `中文/测试文档`; `Version 2.0.0` → `version-2.0.0`.
- Project prefix: generated (not explicit) permalinks get `<project-permalink>/` prepended by
  default (`oracle/notes/simple`); explicit frontmatter permalinks are never rewritten. `[V+golden]`

Project permalink derived from project name with same function. `[V] models/project.py`
Synthetic observation permalink: `entity.permalink/observations/<category>/<content[:200]>`
(+ sha256 digest when content > 200 chars). `[V] models/knowledge.py`
Relation permalink: `<from>/<relation_type>/<to>` where from/to fall back to file_path when
permalink is null. `[V] models/knowledge.py`

## 5. Tags

`parse_tags`: list or comma-separated string → list of trimmed non-empty strings.
`[V] utils.py`

## 6. Body grammar: observations

Parsed from markdown bullet/inline tokens (markdown-it + plugin). `[V] markdown/plugins.py`

Observation line form:

```text
- [category] Content text #tag1 #tag2 (context)
- Content text #tag1        # no category is valid when the line has tags
- [] Content text #tag      # empty brackets only count when the line also has a tag
```

Rules:
- Category must be a single bracket group; trailing `(context)` stripped; inline `#tags`
  extracted (multi-tag `a#b#c` splits).
- `[]` alone does **not** produce an observation; it is parsed as category-less only when the
  line has a tag (`[V+golden]`).
- Excluded shapes: task checkboxes `[ ] [x] [-] [/] [>] [?] [X]`; timestamp-like categories
  `[1:02:03.500]`; markdown links `[text](url)`; wikilink-only lines `[[...]]`; content inside
  blockquotes (Obsidian callouts). `[V] plugins.py`
- A trailing `#bm:links_to` directive is removed before classification. `[V] plugins.py`
- `category` default when absent: stored as `note` (golden-verified). `[V+golden]`

## 7. Body grammar: relations

`[V] markdown/plugins.py`

Explicit relation line forms:

```text
- depends_on [[Target]]            # single token label
- "multi word type" [[Target]]     # quoted label with spaces
- 'multi word type' [[Target]]
- - type [[Target]] (context)      # optional parenthesized context
```

Rules:
- Label = text before first `[[`; single token only unless quoted. Any prose tail after the
  `]]` (other than one balanced `(...)`) makes the line **not** an explicit relation.
- Inline wikilinks anywhere in prose → implicit relation `links_to` per link:
  `parse_inline_relations` handles nested `[[ ]]`, target normalized via
  `normalize_project_reference`, label/display part after `|` handled. `[G]` display-label detail.
- `#bm:links_to` suppresses explicit-relation parsing on that line.

Insertion order (decides relation ids, and therefore `build_context` traversal order): the
reference keys relations by `(relation_type, to_name)` — first authored occurrence wins — and
inserts the survivors in lexicographic order, **not** document order.
`RelationGenerationPublisher.publish` `[V+golden]`. Observations keep document order.

## 8. Note serialization (write format)

`markdown/markdown_processor.py` `[V]`:

- Frontmatter order: `title`, `type`, `permalink`, then remaining metadata keys.
- Body starts with user content (new file: `# <title>\n`).
- Trailing whitespace stripped; one blank line; then observations lines; then relations lines.
- `- [category] content (context)` and `- type [[target]] (context)` string forms.

Frontmatter-only rewrites (the sync path that injects `title`/`type`/`permalink`) use
`FileService.update_frontmatter_with_result` instead: existing keys keep their order, updates
are merged in (`{**current, **updates}`), lists are emitted in block style, strings that would
resolve to another YAML type (bools, numbers, timestamps like `2026-01-02`) are single-quoted,
and the body is re-attached stripped: `---\n<yaml>---\n\n<body.strip()>` — so files end without
a trailing newline. Malformed frontmatter is never rewritten. `[V+golden]` (see
`tests/golden/vault/` and `tests/note_golden.rs`)

## 9. Search index row model

FTS5 `search_index` rows cover three types (`entity`, `observation`, `relation`) `[V] models/search.py`:

- entity: title, permalink, file_path, content (body/notes), type = note_type.
- observation: entity link, category, content.
- relation: from/to entity, relation_type, target.

FTS5 columns: `id UNINDEXED, title, content_stems, content_snippet, permalink, file_path
UNINDEXED, type UNINDEXED, project_id UNINDEXED, from_id/to_id/relation_type/entity_id/
category/metadata/created_at/updated_at UNINDEXED`, tokenizer
`unicode61 tokenchars 0x2F`, prefix `1,2,3,4`. `[V]`

Semantic rows live beside it: `search_vector_chunks` (one row per chunk, with `entity_id`,
`chunk_key` = `type:id:index`, `chunk_text`, `source_hash` = sha256 of the chunk text,
`entity_fingerprint`, `embedding_model`, `vector_index`, `embedding_status`, `updated_at`) and
`search_vector_embeddings` (the 384-dim vector keyed by chunk rowid plus its `source_hash`).
The reference stores the vector in a sqlite-vec `vec0` table; `basic-mem-rs` stores it as a BLOB
and computes the same exact-KNN cosine in Rust, so the rows are compatible in every column
except `vector_index` (`blob` instead of `sqlite-vec`) and the table type. `[V+golden]`

## 10. Content hashing

`checksum` for change detection; note_content materialization tracks db/file versions and
sha256 checksums. `[G]` exact checksum algorithm (compute_checksum in `file_utils.py`).
