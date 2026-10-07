# Oracle Harness

`export_reference.py` captures golden outputs from the **pinned reference implementation**
(0.23.2) so `auto-memory-rs` can prove behavior compatibility.

## What it does

1. Creates a throwaway work dir under `/tmp` (never touches the real `~/.config/basic-memory`).
2. Copies `tests/fixtures/vault` into the work dir and registers it as project `oracle`
   using an isolated `BASIC_MEMORY_CONFIG_DIR`.
3. Runs `basic-memory reindex --full --search` and, when a fastembed model cache is
   available, `basic-memory reindex --embeddings` (model cache copied into the work dir,
   so the run stays offline).
4. Dumps parsed/indexed rows from the reference SQLite DB.
5. Captures search (text/title/permalink/filters/vector/hybrid), `build_context`
   (JSON + text), CLI `status --json`, and error cases.
6. Writes canonicalized artifacts to `tests/golden/` plus `manifest.json` and
   `reference-env.json`.

## Usage

```bash
# full capture (needs the reference CLI + fastembed cache for vector/hybrid)
python3 tools/export_reference.py

# skip semantic cases
python3 tools/export_reference.py --no-embeddings

# keep the throwaway work dir for debugging
python3 tools/export_reference.py --keep-workdir
```

Options: `--fixtures`, `--out`, `--work-dir`, `--model-cache`, `--no-embeddings`,
`--keep-workdir`.

## Raw parse capture

`dump_reference_parse.py` captures the parse layer (frontmatter + observations + relations)
*before* the indexer normalizes files. Run it with the reference interpreter:

```bash
~/.local/share/uv/tools/basic-memory/bin/python tools/dump_reference_parse.py
```

It writes `tests/golden/parse/reference-parse.json`, which `tests/parser_golden.rs` diffs the
Rust parser against. Parsing needs no database, so this script runs without sandbox escalation.

## Graph + traversal replay capture

`export_reference.py` also runs `dump_reference_graph.py` with the reference interpreter once
the index is built:

```bash
~/.local/share/uv/tools/basic-memory/bin/python tools/dump_reference_graph.py \
    --db /tmp/basic-memory-oracle-XXXX/config/memory.db
```

It writes the reference `entity`/`relation`/`observation` rows *including ids*
(`tests/golden/index/graph-rows.json`) and the rows its SQLite traversal returns for the
context cases (`tests/golden/context/find-related.json`). `tests/context_golden.rs` loads the
rows into a throwaway index and replays `Store::find_related`, which is the only way to check
the ported query's ordering and `max_related` cut: the reference assigns ids concurrently, so
its own order is not reproducible from the markdown alone.

`dump_reference_context_text.py` renders a raw `build_context` payload with the reference MCP
formatter (`_format_context_markdown`), producing `tests/golden/context/<case>.md`; the CLI can
only request JSON, so the markdown surface would otherwise be uncapturable.

`dump_reference_edits.py` captures the note-edit operations (`apply_edit_operation`,
`replace_section_content`, `insert_relative_to_section`, `_merge_metadata_into_markdown`) as a
22-case table in `tests/golden/note/edit-operations.json`, including the reference `ValueError`
messages; `tests/note_golden.rs` replays it. It is pure text handling, so it needs no database.

`dump_reference_embeddings.py` embeds the captured chunk corpus and the vector queries with the
reference `fastembed` runtime and writes `tests/golden/vector/embeddings-reference.json`
(`FixtureEmbeddingProvider` replays it; `tests/vector_golden.rs` reproduces the reference vector
search from it). It runs fully offline — set `HF_HUB_OFFLINE=1` and point `--model-cache` at the
fastembed cache; without that variable `huggingface_hub` tries to revalidate over the network
and appears to hang.

## MCP response capture

```bash
python3 tools/dump_reference_mcp.py --keep-workdir   # needs escalation
```

`dump_reference_mcp.py` reuses this harness's hermetic setup (temp `HOME` +
`BASIC_MEMORY_CONFIG_DIR`, fixture vault, `reindex --full --search`), then starts
`basic-memory mcp --project oracle` and drives a scripted session frame by frame, writing
`tests/golden/mcp/responses.json`.

Two things about the transport are easy to get wrong. Requests must be written and their
responses read interactively: closing stdin after a single write makes FastMCP tear down the
connection and answer every pending request with `{"error": {"message": "Connection closed"}}`.
And `tools/list` is only accepted after the `notifications/initialized` notification that
follows `initialize`, which the driver sends for you.

`dump_reference_picoschema.py` runs the reference interpreter over the pure
`basic_memory.picoschema` package (parser, resolver, validator, inference, diff) and writes
`tests/golden/schema/picoschema.json` (38 cases). It needs no database and no model cache:

```bash
~/.local/share/uv/tools/basic-memory/bin/python tools/dump_reference_picoschema.py
```

## Schema tool capture

```bash
python3 tools/dump_reference_schema_mcp.py --keep-workdir   # needs escalation
```

`dump_reference_schema_mcp.py` drives three vaults through the same hermetic setup — a schema
vault (`tests/fixtures/schema-vault`), the plain fixture vault (no schema notes), and a copy of
the schema vault whose `settings.validation` is invalid — and records both surfaces of the three
schema tools:

- every `tools/call` frame of a scripted `basic-memory mcp` session (`schema_validate` in all
  four coverage modes plus its three guidance branches, `schema_infer`, `schema_diff`);
- the stdout of the reference's `bm tool schema-validate|infer|diff` runs, which is the JSON
  surface our CLI mirrors.

The result is `tests/golden/mcp/schema.json`, replayed byte for byte by
`tests/schema_mcp_golden.rs` (MCP frames) and `tests/schema_cli_golden.rs` (CLI stdout).

## ChatGPT adapter capture

```bash
python3 tools/dump_reference_chatgpt_mcp.py --keep-workdir   # needs escalation
```

`dump_reference_chatgpt_mcp.py` runs the same scripted session twice against the fixture vault,
changing only the `initialize` `clientInfo`: once as a neutral client (the `search`/`fetch`
"Unsupported MCP client" payloads) and once as `openai-mcp` (real results, a path-style id, a
memory URL, a title lookup, and a missing document). It writes `tests/golden/mcp/chatgpt.json`,
replayed by `tests/chatgpt_mcp_golden.rs`.

## Offline smoke check

```bash
python3 tools/smoke.py            # add --skip-build to reuse target/release/auto-memory
```

`smoke.py` is the release gate for a clean machine: it builds offline, indexes a throwaway vault,
exercises `status`/`search`/`context`/`schema`, drives the MCP server over stdio, deletes the index
and rebuilds, and hashes the vault before and after. It needs no reference install and no model
cache.

## Requirements

- `basic-memory` on `PATH` (reference 0.23.2).
- `~/.config/basic-memory/fastembed_cache` (or `--model-cache`) for vector/hybrid cases.
- The reference interpreter for the dumps above: set `BASIC_MEMORY_ORACLE_PYTHON` to override
  the default `~/.local/share/uv/tools/basic-memory/bin/python`.

## Note on the sandbox

The reference CLI runs Alembic migrations that hang inside the Codex filesystem sandbox.
Run the harness with sandbox escalation (it still only writes to `/tmp` and the repo).

The reference CLI drops a `:memory:.ses` session file in the working directory (the harness
runs it with the repo as `cwd`), so that name is ignored by git.

## Not part of the harness

`auto-memory-hook.py` is user-facing: an agent-lifecycle hook that briefs a Codex
session from an existing index — or a Tact session, if it is hung off
`UserPromptSubmit` (`SessionStart` output is dropped by Tact; see `docs/hooks.md`
§1). It talks to the built binary, never to the reference implementation, and
nothing in the test suite imports it.
