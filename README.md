# auto-memory-rs

A local-first Rust knowledge base: markdown notes in an Obsidian vault are the only durable
state, a SQLite index (full-text + vector) is derived from them, and an MCP server exposes the
resulting knowledge graph to agents.

Behavior is pinned to the reference implementation (0.23.2) and proved by a golden
corpus captured from it (`tests/golden/`) — see [`docs/reference.md`](docs/reference.md) and
[`specs/compatibility-spec.md`](specs/compatibility-spec.md).

## Quick start

```bash
./scripts/install.sh                      # prebuilt binary from the latest release
auto-memory doctor                        # what is usable on this machine
auto-memory reindex --vault ~/vault \
    --index ~/.local/share/auto-memory/memory.db --project oracle
auto-memory mcp --vault ~/vault \
    --index ~/.local/share/auto-memory/memory.db --project oracle
```

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
| `specs/` | behavior and compatibility contracts |
| `plans/` | execution plans and delivery records |
| `tools/` | the oracle harness that captures it (needs the reference CLI) |
| `plugins/agents/` | the Codex plugin package (hooks, skills, schemas) |
| `plugins/tact/` | the Tact plugin package (same skills/schemas, `.tact/` config, `--harness tact`) |
| `plugins/deepseek/` | the DeepSeek Harness package (same skills/schemas, `$DSH_HOME`/`.dsh/` config, `--harness deepseek`) |
| `dsh-plugin/` | the installable DSH **bundle** (`dsh plugin --profile desktop add …`): skills + MCP row |

## Development

The gates CI runs are available locally as one script — formatting, lints, the test suite and
the doc build:

```bash
./scripts/check-rust.sh
```

`.githooks/` also carries a `pre-commit` hook that refuses a commit made directly on `main`.
Work lands on a branch and merges, so a commit on `main` skips review and cannot become a pull
request without rewriting history. The hook prints the two ways forward (create a branch, or
`git commit --no-verify`).

Install them once per clone:

```bash
./scripts/install-git-hooks.sh
```

The script copies `.githooks/*` into `.git/hooks`, which is outside the working tree and so
survives every checkout. **Do not use `git config core.hooksPath .githooks` instead**: that
points git at the working tree, so the hooks vanish the moment you switch to a branch that
predates them — including `main` before this landed, which is precisely when the branch guard
needs to be present. Re-run the script after editing a hook.

The same gates run as `pre-push`, so a push runs them before anything leaves the machine.
`git push --no-verify` skips that; `git commit --no-verify` skips the branch guard.

## Documentation

[`docs/README.md`](docs/README.md) indexes the user docs and links to the `specs/` and
`plans/` trees: data format, search, context, MCP, usage, hooks, architecture, and the 中文
integration guide.

## License

AGPL-3.0-or-later — the full text is in [`LICENSE`](LICENSE). This is a derivative work, so
the AGPL's network clause applies: see [`docs/licensing.md`](docs/licensing.md) for what that
permits and what it rules out.
