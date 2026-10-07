# Auto Memory for Tact (`plugins/tact`)

A Tact plugin package that wires a local `auto-memory-rs` index into the agent
lifecycle: **brief** a session from the knowledge graph on start, and request an
**authored checkpoint** after compaction so a later session can resume.

This is the **Tact** package. Its sibling `plugins/agents` is the Codex package —
same skills and schemas, different harness identity (`.codex/` config paths,
`--harness codex`, `codex_session` note type). Install **one per host**: see
[Relationship to the Codex package](#relationship-to-the-codex-package).

The skills and schemas are derived from that Codex package, which was ported from
the reference implementation's `plugins/codex` (MIT). The hook engine lives in the
`auto-memory` binary (`auto-memory hook … --harness tact`), not in a shim.

## Layout

```
plugins/tact/
├── .codex-plugin/plugin.json   # manifest (name auto-memory-tact, skills, hooks)
├── hooks/
│   ├── hooks.json              # SessionStart / PreCompact → shims
│   ├── session_start.sh        # → auto-memory hook session-start --harness tact
│   └── pre_compact.sh          # → auto-memory hook pre-compact --harness tact
├── skills/                     # am-checkpoint, am-orient, am-decide, …
└── schemas/                    # tact_session, coding_session, decision, task
```

`.codex-plugin/` is the manifest directory name Tact reads (the Codex plugin ABI),
not a statement about the harness.

## Install into Tact

Tact auto-discovers local marketplaces — `$HOME/.agents/plugins/marketplace.json`
(personal) or, walking up from the cwd, the first `<root>/.agents/plugins/marketplace.json`.
This repo ships one, so from the repo root:

```sh
tact-ui plugin marketplace list            # auto-memory (discovered)
tact-ui plugin install auto-memory-tact@auto-memory
tact-ui plugin list
tact-ui hooks list                         # this package's hooks land under "Needs review"
tact-ui hooks trust                        # unapproved hooks are never registered
```

The catalog entry's `name` must equal the manifest `name` (`auto-memory-tact`), and
`source` is resolved relative to the marketplace root. The two names are the same
string on purpose: the plugin id is also the skill namespace
(`auto-memory-tact:am-checkpoint`) and the hook trust label
(`plugin auto-memory-tact`), so renaming it later means re-trusting the hooks and
updating every place that names the skill in full.

## What the hooks do

| Event | Verb | Effect |
|---|---|---|
| `SessionStart` (`startup`/`resume`) | `hook session-start` | Prints a brief: pinned project, active tasks, open decisions, recent checkpoints. |
| `SessionStart` (`compact`) | `hook session-start` | Same brief, prefixed with the checkpoint request when `checkpointOnCompact` is on. |
| `PreCompact` | `hook pre-compact` | Captures nothing yet (lifecycle WAL not ported). Tact keeps only the hook's `control`, so the stdout is ignored. |

Tact collects a `SessionStart` hook's `additionalContext` and injects it as a
`<hook-context>` message before the first turn
(`crates/tact/src/plugin/hooks.rs`, `collect_session_start_output`), so the brief
is applied — this is the half that used to be dropped.

**The checkpoint flow**: because Tact ignores `PreCompact` stdout, the request is
delivered by the *post-compaction* `SessionStart`. That prompt tells the resumed
agent to run the [`am-checkpoint`](skills/am-checkpoint/SKILL.md) skill, which
writes one **immutable** `tact_session` (or `coding_session`) note through the MCP
`write_note` tool and links it to its predecessor with `continues [[…]]`.

Both hooks are **fail-open**: `auto-memory` exits 0 on every error path (missing
index, malformed stdin, unknown project, unknown harness), so a hook can never
break a session. stdout carries the brief and nothing else; diagnostics go to
stderr.

## Configure

Project mapping is read from JSON, project keys overriding user keys:

1. `~/.tact/auto-memory.json`
2. `<nearest ancestor>/.tact/auto-memory.json`

```json
{
  "autoMemory": {
    "primaryProject": "my-project",
    "captureFolder": "tact/my-repo",
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

The shims pass no `--index` and no `--project`, so the hook resolves both itself, from files —
there is nothing to export in a shell profile. Two chains, each highest precedence first:

- **Index:** `--index` → `$AUTO_MEMORY_INDEX` (still honoured, no longer the way to configure the
  tool) → the user config file → the built-in default
  (`~/.local/share/auto-memory/memory.db`).
- **Project:** `--project` → the mapping file's `primaryProject` (`.tact/auto-memory.json`, or the
  nearest ancestor that has one) → `default_project` in the user config file. The mapping file is
  the *more* specific source, so it wins over the config file.

The user config file, `~/.config/auto-memory/config.json`:

   ```json
   { "index": "~/.local/share/auto-memory/memory.db", "default_project": "my-project" }
   ```

With no project named anywhere, the hook prints the first-run nudge instead of guessing.

Keys in the config file are **snake_case** (`default_project`), unlike the mapping files'
camelCase (`primaryProject`) — a camelCase key there is ignored as an unknown key.

`AUTO_MEMORY_BIN` is separate: it selects the binary the shim invokes (default `auto-memory` from
`PATH`).

### MCP server

The plugin deliberately ships **no** `.mcp.json`: a plugin bundle's server config
can only carry static argv, and Tact does not expand environment variables in MCP
arguments — a `${VAR}` placeholder would be passed literally and `auto-memory mcp`
would create a junk file named `${AUTO_MEMORY_INDEX}`. The vault and index paths
are per-user, so declare the server in the user-level config instead:

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

The server provides the `write_note`, `search`, `fetch`, and `build_context` tools
the skills call.

## Relationship to the Codex package

| | `plugins/agents` | `plugins/tact` (this one) |
|---|---|---|
| Plugin id | `auto-memory-rs` | `auto-memory-tact` |
| Shims | `--harness codex` | `--harness tact` |
| Mapping file | `~/.codex/basic-memory.json`, `.codex/…` | `~/.tact/auto-memory.json`, `.tact/…` |
| Session note type | `codex_session` | `tact_session` |
| Checkpoint title | `Codex checkpoint - …` | `Tact checkpoint - …` |

Tact loads the skills of **every** installed plugin, so installing both into the
same host gives you two near-identical skill sets and two `SessionStart` hooks —
two briefs in one session, one of them reading `.codex/`. Install this package in
Tact and the other in Codex; that is a per-host choice the packages cannot make
for you.

## Not yet ported

The first slice covers the two verbs the harnesses need. Still missing from the
reference front door: the SPEC-55 envelope/inbox WAL, `hook stop|flush|status`,
`hook install|remove`, transcript extraction / auto-capture note writing, and the
Claude Code / Pi plugin packages.
