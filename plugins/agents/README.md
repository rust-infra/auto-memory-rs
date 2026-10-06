# Auto Memory for Codex (`plugins/agents`)

A Codex plugin package that wires a local `auto-memory-rs` index into the agent
lifecycle: **brief** a session from the knowledge graph on start, and request an
**authored checkpoint** after compaction so a later thread can resume.

The package lives in `plugins/agents` because it installs into the shared
**`~/.agents`** agent home (the same root Tact reads as its Codex-compatibility
skill/marketplace directory), not into a Codex-only location. The harness is
still Codex, so the manifest directory is the standard `.codex-plugin/`.

The schemas and skills are ported from the reference implementation's
`plugins/codex` (MIT); the hook engine lives in the
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

## Install into Tact

Tact **auto-discovers** local marketplaces — `$HOME/.agents/plugins/marketplace.json`
(personal) or, walking up from the cwd, the first `<root>/.agents/plugins/marketplace.json`.
`tact-ui plugin marketplace add` does *not* take a `file://` path (`MarketplaceSource::parse`
accepts only git/http/https/ssh or an `owner/repo` shorthand), so the discovered file is
the only local route. This repo ships one, so from the repo root:

```sh
tact-ui plugin marketplace list            # auto-memory (discovered)
tact-ui plugin install auto-memory-rs@auto-memory
tact-ui plugin list
tact-ui hooks list                         # the package's two hooks land under "Needs review"
tact-ui hooks trust                        # unapproved hooks are never registered
```

The catalog entry's `name` must equal the manifest `name` (`auto-memory-rs` in
`.codex-plugin/plugin.json`), and `source` is resolved relative to the marketplace root.

**This package is the Codex one.** Tact has its own: [`plugins/tact`](../tact/README.md)
(same skills and schemas, `.tact/` config paths, `--harness tact`, `tact_session`
note type). Install one per host — Tact loads the skills of every installed
plugin, so installing both gives you two near-identical skill sets and two
`SessionStart` hooks, one of them reading `.codex/`.

Installing *this* package into Tact still works, but only as the Codex
configuration: the shims hardcode `--harness codex` and the skills read
`.codex/basic-memory.json`, so a Tact session gets Codex-worded briefs and a
`codex/<repo>` capture folder.

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

On **Codex** this is what briefs a session. On **Tact** the `SessionStart` brief is
applied too — Tact collects a hook's `additionalContext` and injects it as a
`<hook-context>` message before the first turn (`crates/tact/src/plugin/hooks.rs`,
`collect_session_start_output`). The shim still has to ask for the Tact harness
(`--harness tact`) for the brief to be worded for Tact and to read
`.tact/basic-memory.json`; see the packaging note above and `docs/hooks.md` §1 for the
per-event table.

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

### Where the index and project come from

The shims pass no `--index` and no `--project`, so the hook resolves them itself, from files —
there is nothing to export in a shell profile. Highest precedence first:

1. `--index` / `--project` (a flag, if you wire one up yourself)
2. `$AUTO_MEMORY_INDEX` — still honoured, no longer the way to configure the tool
3. the user config file, `~/.config/auto-memory/config.json`:

   ```json
   { "index": "~/.local/share/auto-memory/memory.db", "default_project": "my-project" }
   ```

4. the mapping file for the project (`.codex/basic-memory.json` → `primaryProject`), for the
   project only; the index has no equivalent here
5. the built-in default index (`~/.local/share/auto-memory/memory.db`); with no project at all the
   hook prints the first-run nudge instead of guessing

Keys in the config file are **snake_case** (`default_project`), unlike the mapping files'
camelCase (`primaryProject`) — a camelCase key there is ignored as an unknown key.

`AUTO_MEMORY_BIN` is separate: it selects the binary the shim invokes (default `auto-memory` from
`PATH`).

### MCP server

The plugin deliberately ships **no** `.mcp.json`: a plugin bundle's server config
can only carry static argv, and Tact does not expand environment variables in
MCP arguments — a `${VAR}` placeholder would be passed literally and
`auto-memory mcp` would create a junk file named `${AUTO_MEMORY_INDEX}`. The vault
and index paths are per-user, so declare the server in the user-level config
instead:

```json
// ~/.tact/.mcp.json   <- note the leading dot
{
  "mcpServers": {
    "auto-memory-rs": {
      "command": "/absolute/path/to/auto-memory",
      "args": ["mcp", "--vault", "/path/to/vault", "--index", "/path/to/memory.db", "--project", "my-project"]
    }
  }
}
```

Tact reads `~/.tact/.mcp.json` (user scope) and `<workdir>/.tact/.mcp.json` (project
scope); a bare `~/.tact/mcp.json` is a legacy path and is **not** read. Because Tact
does not expand environment variables in `args`, both paths must be absolute, and
`command` must be absolute too unless `auto-memory` is on `PATH`.

The server provides the `write_note`, `search`, `fetch`, and `build_context`
tools the skills call. (For Codex, add the same entry under `mcpServers` in
`~/.codex/config.toml`.)

## Not yet ported

The first slice covers the two verbs Codex needs. Still missing from the
reference front door: the SPEC-55 envelope/inbox WAL, `hook stop|flush|status`,
`hook install|remove`, transcript extraction / auto-capture note writing, and the
Claude Code / Pi plugin packages.
