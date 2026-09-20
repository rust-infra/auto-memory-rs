# auto-memory-rs

A local-first Rust knowledge base: markdown notes in an Obsidian vault are the only durable
state, a SQLite index (full-text + vector) is derived from them, and an MCP server exposes the
resulting knowledge graph to agents.

Behavior is pinned to the reference implementation (0.23.2) and proved by a golden
corpus captured from it (`tests/golden/`) — see [`docs/reference.md`](docs/reference.md) and
[`docs/compatibility-spec.md`](docs/compatibility-spec.md).

## Quick start

```bash
./scripts/install.sh                      # prebuilt binary from the latest release
auto-memory doctor                        # what is usable on this machine
auto-memory reindex --vault ~/vault \
    --index ~/.local/share/auto-memory/memory.db --project oracle
auto-memory mcp --vault ~/vault \
    --index ~/.local/share/auto-memory/memory.db --project oracle
```

`scripts/install.sh` downloads the release asset for this platform and verifies its
SHA-256. A public repository needs no credential; if the repository is private, set
`GITHUB_TOKEN` (or `GH_TOKEN`) to a token with `Contents: Read`. Building from source
needs only a Rust toolchain (`cargo build --release`).

`reindex` is incremental; text search, context and the MCP server work without the embedding
runtime. See [`docs/usage.md`](docs/usage.md) for the full walkthrough.

The container image published on the same tags bundles that runtime and the embedding model, so
semantic search needs nothing installed:

```bash
docker run --rm -p 8765:8765 \
    -v ~/vault:/vault -v auto-memory-index:/index \
    ghcr.io/rust-infra/auto-memory-rs:latest
```

It serves streamable HTTP MCP on `/mcp`; see [`docs/release-checklist.md`](docs/release-checklist.md)
§1c for the image's defaults and how to run it over stdio instead.

## Layout

| Path | Contents |
|---|---|
| `src/` | the library (`auto_memory`) and the `auto-memory` CLI |
| `tests/golden/` | golden corpus captured from the reference implementation |
| `tools/` | the oracle harness that captures it (needs the reference CLI) |
| `plugins/agents/` | the Codex plugin package (hooks, skills, schemas) |

## Documentation

[`docs/README.md`](docs/README.md) is the index: spec, execution plan, data format, search,
context, MCP, usage, hooks, architecture, and the 中文 integration guide.

## License

AGPL-3.0-or-later — the full text is in [`LICENSE`](LICENSE). This is a derivative work, so
the AGPL's network clause applies: see [`docs/licensing.md`](docs/licensing.md) for what that
permits and what it rules out.
