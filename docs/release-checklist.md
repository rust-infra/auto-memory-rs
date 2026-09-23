# Release Checklist

What has to be true before tagging a release of the local core. Every item is meant to be run,
not reasoned about; the compatibility evidence lives in `tests/golden/` and is summarized per
phase in `reference.md`.

## 1. Quality gates

The same gates run in CI on every push and pull request
(`.github/workflows/ci.yml`), so a red gate should never reach a tag:

```bash
cargo fmt --all -- --check
cargo check --offline --all-targets
cargo clippy --offline --all-targets -- -D warnings
cargo test --offline
cargo doc --offline --no-deps
```

All five must be clean, and `cargo test` must report no failures. Ignored tests are the opt-in
benchmarks (`tests/benchmarks.rs`) only; any other `#[ignore]` needs a written reason in
`reference.md` or the test itself.

## 1b. Shipping

`./scripts/install.sh` is the user-facing install path and is worth running with `--dry-run`
against a real release before announcing it. Pushing the tag runs
`.github/workflows/release.yml`, which builds six targets, writes `SHA256SUMS`, and attaches
the archives to the release:

| Target | Runner |
|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` |
| `x86_64-unknown-linux-musl` | `ubuntu-latest` |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` |
| `aarch64-apple-darwin` | `macos-latest` |
| `x86_64-pc-windows-msvc` | `windows-latest` |

The musl archives are statically linked and so carry no glibc floor; the gnu ones need a host
glibc at least as new as the runner's (Ubuntu 24.04 → 2.39). `scripts/install.sh` still maps
Linux to `unknown-linux-gnu`, so musl is the manual download for now.

**Semantic search does not work on the musl artifacts.** ONNX Runtime is published only as a
glibc-linked `.so`, and a static musl binary cannot `dlopen` it (measured against 1.29.0:
`failed to load from …: dlopen failed`); `--vector` and `--hybrid` are therefore unavailable
there, while text search, context, schema and the MCP server are not affected. `doctor` reports
`onnx_runtime: ok` on such a host because the search path only locates the file — it does not
try to load it — so do not read that check as "semantic search works".

ONNX Runtime is not bundled (the crate builds with `ort`'s `load-dynamic`), which is what makes
the archives portable; `auto-memory doctor` tells a user whether semantic search is available.

## 1c. Container image

The same tag also publishes `ghcr.io/rust-infra/auto-memory-rs` from
`.github/workflows/container.yml`: `linux/amd64` and `linux/arm64`, each built on a runner of
that architecture and joined into one manifest by the `merge` job (which is what attaches the
`vX.Y.Z` and `latest` tags). Unlike the archives, the image **does** bundle semantic search, so
there is nothing to install and nothing to download at run time:

```bash
docker run --rm -p 8765:8765 \
    -v ~/vault:/vault -v auto-memory-index:/index \
    ghcr.io/rust-infra/auto-memory-rs:latest
```

- `Dockerfile` — `rust:bookworm` builds the binary; a `python:3.13-slim` stage extracts
  `libonnxruntime.so` (1.29.0) and downloads the
  `qdrant/bge-small-en-v1.5-onnx-q` snapshot in the huggingface-hub cache layout;
  `debian:bookworm-slim` carries all three. Nothing is fetched at run time.
- The bundled ONNX Runtime is the same 1.29.0 the reference captures were produced with, so
  container scores and golden scores share one runtime version. (1.30.0 was also measured to
  pass the embedding, reranker, and MCP semantic compatibility tests, but scores drift slightly
  across releases — the image deliberately pins the capture version instead.)
- Defaults: streamable HTTP on `0.0.0.0:8765` (`/mcp`), vault `/vault`, index
  `/index/memory.db`. Override the command for stdio (`docker run -i --rm … mcp --vault …`) or
  add `--read-only` to serve without writing back.
- `--model-cache` in that default command is load-bearing, not decoration: `mcp` only builds the
  embedding provider when `--model-cache`, `--onnx-runtime` or `--embedding-fixture` is passed,
  so `AUTO_MEMORY_MODEL_CACHE` alone would leave semantic search reporting itself unavailable.
- The workflow's smoke test asserts `doctor --json` reports `onnx_runtime` and `model_cache` as
  `ok`, then runs `reindex --embeddings` against a one-note vault — per architecture, against the
  pushed digest, before the tags exist. That second step proves the bundled 1.29.0 runtime can
  initialise the embedding session, not just that the files are present. The reranker model is not
  bundled (only needed for `--reranker`).
- Both halves are fetched at build time over the network (PyPI and huggingface.co), so a local
  build needs a route to them: behind a proxy,
  `docker build --network=host --build-arg HTTPS_PROXY=… --build-arg HTTP_PROXY=… .` — BuildKit
  passes the proxy build args to `RUN` without an `ARG` declaration.

## 2. Compatibility evidence

- The golden corpus is current: `python3 tools/export_reference.py --keep-workdir` regenerates
  `tests/golden/` (needs escalation; it writes only to `/tmp` and the repo). **Regeneration is not
  byte-identical**, and the drift is expected rather than suspicious:
  - the reference assigns row ids while indexing concurrently, so ordering that falls back to id
    order changes between runs — the no-text-query search goldens, `index/graph-rows.json`,
    `index/search-index.json`, and the `context/**` files (which is also why the traversal is
    replayed against the reference's own ids);
  - semantic scores move by up to ~1.2e-4 (ONNX Runtime batching/threading), which is why the
    vector/hybrid comparisons use a 5e-4 envelope while ranking and `matched_chunk` stay exact;
  - `manifest.json` / `reference-env.json` carry timestamps, durations, and hashes.
  Review the diff for changes *outside* those categories before accepting a regeneration.
- **File mtimes are restored, not inherited.** `entity.updated_at` falls back to the file
  mtime, so `list_directory`'s timestamp column and `updated_desc` ordering, and
  `recent_activity`'s recency list, encode the mtimes of the tree the capture ran against.
  Git stores no mtimes, so `tests/common::copy_fixture_vault` puts the captured values back
  from `tests/golden/index/graph-rows.json` before a session starts. A golden that renders a
  date or a recency order therefore reproduces on a fresh clone.
- **Wall-clock windows are anchored to the corpus, not to today.** `context_golden`s
  traversal replay and `mcp_golden`'s `1d` recency case compute their `since`/shift from
  timestamps recorded in the corpus; deriving them from the machine clock would make those
  assertions expire a few days after each capture.
- The MCP captures are current: `tools/dump_reference_mcp.py`, `tools/dump_reference_schema_mcp.py`,
  and `tools/dump_reference_chatgpt_mcp.py` reproduce their committed `tests/golden/mcp/*.json`.
- Pinned suites are green: parser (16 fixtures), storage projection, incremental/full convergence,
  FTS ordering and scores (1e-6), chunking (78-chunk corpus), vector/hybrid ranking (1e-4),
  `build_context` + traversal, note mutation, MCP surfaces, schema reports, and the ChatGPT
  adapters.
- Every deliberate divergence is listed in `docs/mcp-spec.md` §1c or `docs/usage.md` §8, and each
  one is pinned by a test rather than left implicit.
- Robustness suites are in place: `tests/hardening.rs` (path containment, malformed UTF-8,
  write/parse round trip), `tests/properties.rs` (parser, permalink, and traversal invariants over
  generated inputs), and `tests/storage_migration.rs` (schema creation, version repair, idempotent
  migration).

## 3. Offline smoke

```bash
python3 tools/smoke.py
```

It builds with `--offline`, indexes a throwaway vault, exercises `status`/`search`/`context`/
`schema`, drives the MCP server over stdio, deletes the index, rebuilds, and proves the vault
bytes are untouched. It must print `SMOKE OK` on a machine with no network access.

## 4. Performance smoke

```bash
cargo test --offline --test benchmarks -- --ignored --nocapture
```

Baseline on the development machine (debug build, 400-note generated vault): full rebuild ~450
docs/s, checksum-only reconcile of 400 notes ~54 ms, text search median ~0.6 ms / p95 ~3.4 ms,
chunking ~78k rows/s. These are smoke numbers, not a contract — a drastic change means something
structural happened.

## 5. Compatibility inputs that live outside the repo

- Reference interpreter and CLI: the reference implementation (0.23.2) (`~/.local/share/uv/tools/basic-memory`),
  recorded in `tests/golden/reference-env.json`.
- Embedding model cache: `BAAI/bge-small-en-v1.5` under
  `~/.config/basic-memory/fastembed_cache`, plus the ONNX Runtime shared library. Only the vector
  and hybrid suites need these; text search, context, schema, and MCP do not.
- Reranker model cache: `jinaai/jina-reranker-v1-tiny-en` in the same directory, needed by
  `tests/rerank_golden.rs::onnx_reranker_reproduces_the_reference_scores` (it skips without it;
  point `AUTO_MEMORY_MODEL_CACHE` at a cache that has both models to run it).

## 6. Known parked work

Nothing from the execution plan's phases is outstanding. What remains is out of scope or an
accepted internal difference, and each is recorded in the execution plan's `## 0. Progress`:

- **`vec0` storage** — embeddings live in BLOB tables and are scored in Rust instead of in a
  `sqlite-vec` virtual table. Both paths do exact KNN and every semantic golden matches, so the
  only observable trace is the reference's `bm inspect`.
- **Retrieval-inspection diagnostics** — the reference CLI's `bm inspect query` / `bm inspect
  chunks` (and the `doctor`/`orphans` reports) are not ported; they render internal retrieval
  stages that this port's golden replays already pin.
- **Web/Cloud surfaces** — `list_workspaces` and everything else that needs a workspace or cloud
  route, which the project scoped out from the start (no Web UI, no cloud sync).
