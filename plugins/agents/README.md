# Auto Memory for Codex (`plugins/agents`)

A Codex plugin package that wires a local `auto-memory-rs` index into the agent
lifecycle: **brief** a session from the knowledge graph on start, and request an
**authored checkpoint** after compaction so a later thread can resume.

The package lives in `plugins/agents` because it installs into the shared
**`~/.agents`** agent home (the same root Tact reads as its Codex-compatibility
skill/marketplace directory), not into a Codex-only location. The harness is
still Codex, so the manifest directory is the standard `.codex-plugin/`.

The schemas and skills are ported from the reference implementation's
`plugins/codex` (Basic Memory 0.23.2, MIT); the hook engine lives in the
`auto-memory` binary (`auto-memory hook …`), not in a Python shim.

## Layout

```
plugins/agents/
├── .codex-plugin/plugin.json   # manifest (skills, hooks)
├── hooks/
│   ├── hooks.json              # SessionStart / PreCompact → shims
│   ├── session_start.sh        # → auto-memory hook session-start --harness codex
│   └── pre_compact.sh          # → auto-memory hook pre-compact --harness codex
├── skills/                     # am-checkpoint, am-orient, am-decide, …
└── schemas/                    # codex_session, coding_session, decision, task
```

## Install into `~/.agents`

Copy the package (or symlink it, so the repo stays the source of truth) into the
shared agent home, then register it in the personal marketplace catalog that
Codex reads from `~/.agents/plugins/marketplace.json`:

```sh
ln -s "$PWD/plugins/agents" ~/.agents/plugins/auto-memory-rs
```

```json
// ~/.agents/plugins/marketplace.json — add to "plugins"
{
  "name": "auto-memory-rs",
  "source": { "source": "local", "path": "./.agents/plugins/auto-memory-rs" },
  "policy": { "installation": "AVAILABLE", "authentication": "NONE" },
  "category": "Coding"
}
```

The `source.path` form must match the existing entries in that catalog (Codex
resolves local paths against the marketplace root; the bundled entries use the
`./.codex/plugins/<name>` shape).

## What the hooks do

| Event | Verb | Effect |
|---|---|---|
| `SessionStart` (`startup`/`resume`) | `hook session-start` | Prints a brief: pinned project, active tasks, open decisions, recent checkpoints. |
| `SessionStart` (`compact`) | `hook session-start` | Same brief, prefixed with the checkpoint request when `checkpointOnCompact` is on. |
| `PreCompact` | `hook pre-compact` | Captures nothing yet (lifecycle WAL not ported). Codex ignores PreCompact stdout. |

**The checkpoint flow** (ported from the reference): Codex ignores `PreCompact`
stdout, so the request is delivered by the *post-compaction* `SessionStart`. That
prompt tells the resumed agent to run the [`am-checkpoint`](skills/am-checkpoint/SKILL.md)
skill, which writes one **immutable** `codex_session` (or `coding_session`) note
through the MCP `write_note` tool and links it to its predecessor with
`continues [[…]]`.

Both hooks are **fail-open**: `auto-memory` exits 0 on every error path (missing
index, malformed stdin, unknown project), so a hook can never break a session.
stdout carries the brief and nothing else; diagnostics go to stderr.

## Configure

Project mapping is read from JSON, project keys overriding user keys:

1. `~/.codex/basic-memory.json`
2. `<nearest ancestor>/.codex/basic-memory.json`

```json
{
  "basicMemory": {
    "primaryProject": "my-project",
    "captureFolder": "codex/my-repo",
    "recallTimeframe": "7d",
    "checkpointOnCompact": true,
    "placementConventions": "Put decisions in decisions/."
  }
}
```

The engine reads `primaryProject`, `captureFolder`, `recallTimeframe`,
`recallPrompt`, `focus`, `placementConventions`, `sessionProfile`, `repository`,
`checkpointOnCompact`, and `captureEvents`. A malformed file **fails closed**
(capture and checkpointing disabled) rather than merging a partial route.

### Environment

- `AUTO_MEMORY_BIN` — binary to invoke (default `auto-memory` from `PATH`).
- `AUTO_MEMORY_INDEX` — index path (default `~/.local/share/auto-memory/memory.db`),
  used when `--index` is not passed.

### MCP server

The plugin deliberately ships **no** `.mcp.json`: a plugin bundle's server config
can only carry static argv, and Tact does not expand environment variables in
MCP arguments — a `${VAR}` placeholder would be passed literally and
`auto-memory mcp` would create a junk file named `${AUTO_MEMORY_INDEX}`. The vault
and index paths are per-user, so declare the server in the user-level config
instead:

```json
// ~/.tact/mcp.json
{
  "mcpServers": {
    "auto-memory-rs": {
      "command": "auto-memory",
      "args": ["mcp", "--vault", "/path/to/vault", "--index", "/path/to/memory.db", "--project", "my-project"]
    }
  }
}
```

The server provides the `write_note`, `search`, `fetch`, and `build_context`
tools the skills call. (For Codex, add the same entry under `mcpServers` in
`~/.codex/config.toml`.)

## Not yet ported

The first slice covers the two verbs Codex needs. Still missing from the
reference front door: the SPEC-55 envelope/inbox WAL, `hook stop|flush|status`,
`hook install|remove`, transcript extraction / auto-capture note writing, and the
Claude Code / Pi plugin packages.
