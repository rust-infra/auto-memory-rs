# MCP Server — Compatibility Contract

Reference: `basic_memory` 0.23.2 (`docs/reference.md`). Parameter surfaces below are taken from
the running 0.23.2 MCP server (same build); exact output JSON examples and error strings are
golden-capture items **[G]**.

## 1. Transport & process

- Local server runs on **stdio** by default (`basic-memory mcp`); transport options exist.
- Protocol purity: stdout carries MCP frames only; logs go to stderr/files.
- CLI parity: `basic-memory tool <tool> …` exposes the same tools via CLI.

### 1b. `auto-memory-rs` implementation status

`auto-memory mcp --vault <dir> --index <db> [--project <name>]` serves newline-delimited
JSON-RPC 2.0 on stdout: `initialize` (protocol `2024-11-05`, `serverInfo.name = auto-memory-rs`),
`notifications/initialized` (no response), `ping`, `tools/list`, and `tools/call`. Tool results
use the MCP content convention (`content: [{type: "text", text: <json>}]`, `isError: false`);
protocol errors (unknown method/tool, bad arguments, missing notes) come back as JSON-RPC
`error` frames with the message text, and stderr carries diagnostics only.

Implemented tools: the 20 names in `ToolName::ALL` (`src/adapters/mcp/server.rs`) — the note
family (`write_note`, `read_note`, `view_note`, `read_content`, `edit_note`, `move_note`,
`delete_note`), search (`search_notes`, plus the `search`/`fetch` ChatGPT adapters, gated on the
`initialize` `clientInfo`), `build_context` (honours `output_format="text"`), `list_directory`
(text + json), `recent_activity` (text + json), the project trio (`list_memory_projects`,
`create_memory_project`, `delete_project`), the schema trio (`schema_validate`, `schema_infer`,
`schema_diff`), and `basic_memory_diagnostics`. That enum is the one source of truth for the
names: it drives `tools/list` (each variant carries its description and input schema),
`tools/call` dispatch (an exhaustive match, so a variant cannot go unhandled), the diagnostics
report's `tools` array, and the `unknown tool: <name>` error path. `list_workspaces`
is the one reference tool this port does not expose (Web/Cloud surfaces stay out of scope).
`auto-memory mcp --read-only` is this port's own addition (not in the reference): it
hides the six mutating tools (`write_note`, `edit_note`, `move_note`, `delete_note`,
`create_memory_project`, `delete_project`) from `tools/list` and refuses them with
`tool not available in read-only mode: <name>`.

### 1b-ii. Streamable HTTP transport

`auto-memory mcp --http [--host HOST] [--port PORT] [--path PATH]` serves the same session over
the MCP **Streamable HTTP** transport (`--host` default `127.0.0.1`, `--port` default `8765`,
`--path` default `/mcp`). This is the specification's HTTP transport, not a bespoke endpoint:
`POST` carries a JSON-RPC frame with `Accept: application/json, text/event-stream`, the
`initialize` response returns an `Mcp-Session-Id` header, and every later request echoes it.
The protocol half (sessions, SSE framing, content negotiation) is the official `rmcp` SDK; the
tool surface is the *same* `McpServer::call_tool` / `tools/list` the stdio transport uses, so
the two cannot drift, and `--read-only` applies to both. Both transports share one embedding
and reranker runtime (loaded once, before the transport is selected). `initialize` reports the
same protocol version and server name over either transport. Diagnostics still go to stderr.

Two more closed vocabularies are enums for the same reason: `OutputFormat` (`output_format`;
every tool renders text unless it is asked for JSON, except `build_context`, whose payload is
JSON unless it is asked for text) and `SearchType` (`search_type`, whose `Valid options: …`
error list is generated from the enum).

`read_note` now matches the reference default: `output_format="text"` returns the raw markdown,
and a miss walks the reference's chain — direct resolution, then an exact-title lookup, then a
text search whose hits render as `format_related_results`, and only with no hits at all
`format_not_found_message`. `output_format="json"` returns
`{title, permalink, file_path, content, frontmatter}` where `content` is the body after the
closing fence (so it keeps the blank separator line unless `include_frontmatter=true`).

### 1c. Captured reference responses

`tools/dump_reference_mcp.py` starts the reference server in a hermetic temp `HOME` /
`BASIC_MEMORY_CONFIG_DIR`, runs a scripted session, and writes `tests/golden/mcp/responses.json`.
`tests/mcp_golden.rs` replays it. Verified against that capture:

- `list_directory` text output matches character for character, including the block-character
  prefix (`📁`/`📄`), the 30-character name field, ` | ` metadata suffixes, the summary line, the
  beyond-`page` message, and the `list_directory(dir_name='…', …)` continuation hint (Python
  `repr` quoting, single quotes by default).
- `read_content` returns `{"type":"text","text",…,"content_type":"<media type>; charset=utf-8",
  "encoding":"utf-8"}` for markdown, with the file served verbatim (frontmatter included).
- `view_note` returns the artifact instruction block verbatim, template indentation included
  (`textwrap.dedent` cannot strip it once the inserted markdown has a column-zero line).
- `recent_activity` text output matches character for character, including the
  `📄 Recent Notes & Documents (n):` heading with its two-space `•` rows, the
  `**Activity Summary:** Showing n items (page p). Use page=p+1 to see more.` footer, and the
  empty/next-page guidance branches (the binary's name inside that guidance is this port's —
  see §4 of `compatibility-spec.md`).
- `list_memory_projects` in a `--project`-constrained server returns the pinned
  `Project: <name>` notice, and its JSON form matches field for field.
- `create_memory_project` and `delete_project` keep the reference's constrained-server refusal
  wording (`# Error\n\nProject creation/deletion disabled - MCP server is constrained to
  project '<name>'…`), including the `PROJECT_CONSTRAINED` JSON variant. The `Use the CLI …`
  hint is deliberately **not** verbatim: the reference points at `basic-memory project
  add|remove`, which this CLI does not implement, so it names `auto-memory reindex` and the
  unconstrained-server route instead.
- Every tool except `read_content` nests its structured payload under
  `structuredContent.result`; `read_content` (a plain `dict` return type) exposes the payload
  directly. The reference's `_meta.fastmcp.wrap_result` marker is server-specific and is
  deliberately not mirrored.
- `tools/dump_reference_schema_mcp.py` captures the three schema tools over three vaults (a
  schema vault, a vault with no schema notes, and a vault whose schema is malformed) into
  `tests/golden/mcp/schema.json` (30 `tools/call` frames plus 14 `bm tool schema-*` CLI runs).
  `tests/schema_mcp_golden.rs` and `tests/schema_cli_golden.rs` replay all of them byte for byte,
  which pins the report shapes, the dropped `null`s, the guidance wording, the percent rounding,
  and the CLI's `json.dumps(indent=2, ensure_ascii=True)` rendering. The one phrase exempted
  from the byte comparison is the CLI named in the guidance (`auto-memory status`,
  `auto-memory reindex` instead of the reference's `basic-memory …`) — see §4 of
  `compatibility-spec.md`.
- `tools/dump_reference_chatgpt_mcp.py` captures `search`/`fetch` twice — once from a neutral
  client and once from a client reporting `openai-mcp` — into `tests/golden/mcp/chatgpt.json`
  (9 calls). `tests/chatgpt_mcp_golden.rs` replays every frame byte for byte, including the
  content-list result shape that only these two tools use.
- `tools/dump_reference_search_mcp.py` captures `search_notes` once per `search_type` with
  semantic search disabled into `tests/golden/mcp/search-types.json` (9 calls), pinning the
  markdown surface, the no-results text, the semantic-disabled guidance, and the invalid-type
  message. `tests/mcp_search_types_golden.rs` replays them and also runs a semantic search with
  an attached runtime.

Known divergences, each with a reason:

- **Identifier resolution is more permissive here.** `memory://notes/frontmatter` resolves in this
  port but not in the reference, whose note has the explicit permalink `notes/frontmatter-note`:
  the reference accepts only a permalink, an exact file path, an exact title, or an external id,
  while `graph::resolve_entity_path` also tries `<path>.md`. The captured frame
  `read-note-json-frontmatter` in `tests/golden/mcp/responses.json` is the evidence, and
  `tests/mcp_golden.rs` pins both behaviours so the difference stays deliberate.
- Non-`str` tool results carry compact JSON in their `content[0].text`, in reference key order
  (`serde_json` runs with `preserve_order`, and the reference emits pydantic/model order). The
  `structuredContent` payload is the same object.
- `read_content` returns original image bytes with the true media type; the reference resizes and
  re-encodes through Pillow, which cannot be reproduced byte-for-byte from Rust.
- Files without frontmatter are not rewritten on index here (Obsidian owns the vault), so
  `read_content` serves what is on disk rather than reference-synthesized `title`/`type`/`permalink`.

## 2. Tool inventory (local core)

| Tool | Group | Notes |
|---|---|---|
| `write_note` | notes | create; overwrite default from `write_note_overwrite_default` |
| `read_note` | notes | by title/permalink/memory:// URL |
| `view_note` | notes | formatted artifact rendering |
| `read_content` | notes | raw file content / binary (images) |
| `edit_note` | notes | append/prepend/find_replace/replace_section/insert_before/insert_after + metadata merge |
| `move_note` | notes | move/rename note or directory; maintains links |
| `delete_note` | notes | delete note or directory |
| `search_notes` | search | advanced syntax; search_type text/title/permalink/vector/semantic/hybrid |
| `search` | search | ChatGPT adapter; OpenAI MCP clients only, else an "Unsupported MCP client" payload |
| `fetch` | search | ChatGPT adapter; returns the note as a document (`id`/`title`/`text`/`url`/`metadata`) |
| `recent_activity` | search/activity | per-project or cross-project activity |
| `list_directory` | navigation | vault directory listing |
| `build_context` | graph | memory:// context (see `context-spec.md`) |
| `list_memory_projects` | projects | merged local(+cloud) project list |
| `create_memory_project` | projects | register project |
| `delete_project` | projects | unregister; notes retained unless `delete_notes` |
| `schema_validate` | schema | validate notes against Picoschema |
| `schema_infer` | schema | infer Picoschema from notes |
| `schema_diff` | schema | schema drift report |
| `basic_memory_diagnostics` | diagnostics | version/config report |

Cloud/workspace-only surfaces (out of scope for `auto-memory-rs`): `list_workspaces` and any
`chatgpt`/`ui` variants.

## 3. Note tools — parameters (V surface)

`write_note(title, content, directory, project?, project_id?, workspace?, note_type?, tags?,
metadata?, overwrite?, output_format?)` — directory optional; default overwrite from config.

`read_note(identifier, project?, project_id?, output_format?, page?, page_size?)` — identifier:
exact title, permalink, memory:// URL, or external UUID; ambiguous titles resolved by
related-results fallback (not fuzzy overwrite).

`view_note(identifier, project?, project_id?)` — returns rendered artifact markdown.

`read_content(path, project?, project_id?, output_format?)` — resolves path/permalink;
supports binary content (image resize/optimize for inline).

`edit_note(identifier, operation, content, project?, project_id?, section?, find_text?,
expected_replacements?, replace_subsections?, metadata?, output_format?)` — operations:
`append`, `prepend`, `find_replace`, `replace_section`, `insert_before_section`,
`insert_after_section`. Edit targets require exact identifier match.

`move_note(identifier, destination_path | destination_folder, project?, project_id?,
output_format?)` — note or directory; DB-first move, links updated.

`delete_note(identifier, is_directory?, project?, project_id?, output_format?)` —
note or directory; directory deletion requires `is_directory=true`.

## 4. Search tools — parameters

`search_notes(query?, project?, project_id?, search_all_projects?, search_type?, page?,
page_size?, output_format?, entity_types?, note_types?, categories?, tags?, status?,
metadata_filters?, after_date?, min_similarity?)` — query optional for filter-only searches.

`output_format` defaults to `text`, and the text surface is markdown: a result block per hit
(`### title`, `- permalink`, `- external_id`, `- score`, `- match`), then a pagination footer; a
miss renders `No results found for '<query>' in project '<project>'. …` pointing at
`recent_activity`; a request with no query and no filters answers `# No Search Criteria`.

`search_type` maps the query onto a field or a retrieval mode: `text` (FTS), `title`
(`title` filter), `permalink` (exact, or a glob when the query contains `*`), `vector`/`semantic`
(vector-only), `hybrid` (fused). An unknown type returns the generic `# Search Failed` guidance
whose message names the valid options in sorted order. The semantic modes need an embedding
runtime; without one the tool answers `# Search Failed - Semantic Search Disabled` rather than
falling back to a text search. `auto-memory mcp --embedding-fixture FILE` (or `--model-cache DIR`)
attaches the runtime.

`entity_types` defaults to `["observation"]` when a `categories` filter is present and to
`["entity"]` otherwise; `metadata_filters` aliases `note_type` to `type`; `after_date` is parsed
by `dateparser` semantics (see `docs/reference.md` §6d). `search_all_projects` is not supported —
the server is constrained to one project.

`search(query)` — ChatGPT adapter: one required argument, ten results, each reshaped to
`{id, title, url}` with `total_count` counting the *page*, not the match total. Non-OpenAI clients
receive `{results: [], error: "Unsupported MCP client", error_message: …}`.

`fetch(id)` — ChatGPT adapter: the note's raw markdown plus a derived title. A path-like id is
routed as `memory://<id>`; a miss returns a document whose `metadata.error` is
`"Document not found"`. Non-OpenAI clients receive a document-shaped refusal.

`recent_activity(project?, timeframe?, type?, page?, page_size?, output_format?)` —
timeframe natural language (`"2 days ago"`, `"7d"`, `"2024-01-01"`).

`list_directory(dir_name?, depth?, file_name_glob?, sort?, page?, page_size?, project?,
project_id?, output_format?)`.

## 5. Project & schema tools — parameters

`list_memory_projects(output_format?)` → merged project entries (name/permalink/external_id/
path/mode/…).

`create_memory_project(project_name, project_path, set_default?, workspace?)` → project info.

`delete_project(project_name, delete_notes?, workspace?)` → default keeps notes on disk.

`schema_validate(note_type?, identifier?, project?, project_id?, output_format?)`.
`schema_infer(note_type, project?, project_id?, threshold?, output_format?)`.
`schema_diff(note_type, project?, project_id?, output_format?)`.

All three take `output_format="text"` by default and return `str | report`, so their structured
payload is nested under `structuredContent.result` like the other union-typed tools. The
reference's guard order decides what a request turns into (see
`src/application/schema_tools.rs`, which ports it):

1. `schema_validate()` with neither `note_type` nor `identifier` validates every type that has a
   schema; with no schema notes at all it returns the "No Schemas Defined" guidance (JSON:
   `{"error": "No schemas defined in this project"}`).
2. A scope with no notes of the type returns the "No Notes Found of Type" guidance
   (`{"error": "No notes found of type '…'"}`).
3. Notes that resolve to no schema return the "No Schema Found" guidance
   (`{"error": "No schema found for type '…'"}`).
4. `schema_infer` on notes that share no field above the threshold returns "No Schema Pattern
   Found" (`{"error": "No schema pattern found for '…' (threshold: 25%)"}`).
5. A malformed schema note (a bad `settings.validation`, a missing `entity`/`schema`, an
   unparsable field) becomes the tool's "Failed" template whose `{e}` is the parser's message
   verbatim — the reference surfaces the authoring error as an HTTP 400 and the tool catches it.

Note types are compared through `normalize_note_type` (`to_snake_case`), because indexed
frontmatter keeps the author's spelling: `schema_validate(note_type="Person")` reports
`note_type: "person"` and covers a note whose file says `type: Person`.

`basic_memory_diagnostics()` → version/system/config summary (secrets redacted).

## 6. Behavior contracts

- Errors: tools return structured markdown "Failed" responses or typed errors; exact strings [G].
- `write_note` on existing note errors unless `overwrite=True` (config default).
- `edit_note`/`delete_note`/`move_note` require exact identifier resolution.
- All note mutations update index; moves update permalinks/links per `update_permalinks_on_move`.
- `output_format=json` returns pydantic model dumps; `text` returns markdown. Defaults vary by tool.

## 7. Rust rules

1. Tool names, parameter names (snake_case), and defaults must match the reference surface.
2. JSON responses must match pydantic field names and shapes (see schema models).
3. MCP/CLI share one application layer; stdout purity enforced.
4. Exact error text and not-found formatting captured as golden fixtures.
