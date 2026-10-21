# Auto Memory for DeepSeek Harness (`plugins/deepseek`)

A DeepSeek Harness (DSH) package that wires a local `auto-memory-rs` index into the
agent lifecycle: **orient** a session from the knowledge graph, and write an
**authored checkpoint** so a later session can resume.

This is the **DSH** package. Its siblings are `plugins/agents` (Codex) and
`plugins/tact` (Tact) — same skills and schemas, different harness identity
(`.dsh/` config paths, `--harness deepseek`, `deepseek_session` note type). The
three are deliberately isolated: installing one never changes what the others
read, recall, or write. See [Isolation](#isolation).

The skills and schemas are derived from the Codex package, which was ported from
the reference implementation's `plugins/codex` (MIT). The hook engine lives in the
`auto-memory` binary (`auto-memory hook … --harness deepseek`), not in a shim.

## What DSH actually supports today

DSH is not a command-hook host the way Codex and Tact are. Two facts drive the
whole integration and are worth stating before the instructions:

| Capability | Codex / Tact | DSH |
|---|---|---|
| MCP servers | `config.toml` / `.mcp.json` | a `dsh-mcp-client` plugin row in the profile's `cordis.patch.yml` |
| Command hooks (`SessionStart`, `PreCompact`) | yes, `hooks.json` | **no** — `dsh-mcp-client` bridges MCP tools only |
| Skills (`SKILL.md`) | plugin `skills/` dir | scanned from `<root>/.dsh/skills`, `<root>/.agents/skills`, `$DSH_HOME/skills`, `~/.agents/skills` |
| Workspace instructions | `AGENTS.md` | `AGENTS.md` (same chain, plus `$DSH_HOME/AGENTS.md`) |

So on DSH today the **skills plus the MCP server** carry the behavior, and the
brief is produced on demand (`am-orient`) rather than injected at session start.
The `hooks/` directory is still shipped: it is the same contract Codex and Tact
use, it costs nothing, and a DSH hook plugin would consume it unchanged. Until
then, do not expect a DSH session to run these scripts.

## Layout

```
plugins/deepseek/
├── .codex-plugin/plugin.json   # manifest (name auto-memory-deepseek, skills, hooks)
├── hooks/
│   ├── hooks.json              # SessionStart / PreCompact → shims
│   ├── session_start.sh        # → auto-memory hook session-start --harness deepseek
│   └── pre_compact.sh          # → auto-memory hook pre-compact --harness deepseek
├── skills/                     # am-checkpoint, am-orient, am-decide, …
└── schemas/                    # deepseek_session, coding_session, decision, task
```

`.codex-plugin/` is the manifest directory name the Codex plugin ABI reads; it is
not a statement about the harness.

## Install into DSH

DSH **does** support plugins — as npm **bundles** (`dsh plugin --profile <name>
add <pkg>`), the same form `@tt-a1i/archify-dsh` uses. This directory is the
*skills/schemas source*; the installable artifact is
[`dsh-plugin/`](../../dsh-plugin/README.md), which packages these files plus the
MCP row and installs in one command.

The hand-wiring below still works and is what the bundle does under the hood,
so it stays documented for anyone vendoring the skills without npm.

#### Manual route

### 1. The skills

DSH scans `$DSH_HOME/skills` (rank 400) and `~/.agents/skills` (rank 500), plus
`<projectRoot>/.dsh/skills` (rank 100) and `<projectRoot>/.agents/skills` (rank
200) for the checkout you are working in. Either copy or symlink this package's
skills into one of those roots:

```sh
# user scope: every DSH session sees them
mkdir -p ~/.dsh/skills
ln -sfn "$PWD/plugins/deepseek/skills/"* ~/.dsh/skills/
ls ~/.dsh/skills            # am-checkpoint am-decide am-orient am-remember am-share am-writing
```

Symlinks are fine — DSH follows them (`watchFollowSymlinks` defaults to true).
Skill names must be kebab-case and unique across roots, which `am-*` is.

### 2. The MCP server

One plugin instance per MCP server, in the profile's patch layer. Add to
`~/.dsh/profiles/<profile>/cordis.patch.yml` (`desktop` for the desktop app,
`web` for `dsh web`):

```yaml
- id: mcp-auto-memory
  name: '@deepseek-ai/dsh-mcp-client'
  config:
    serverName: memory
    transport: stdio
    command: /Users/you/.cargo/bin/auto-memory
    args: ['mcp', '--read-only']
    toolCallTimeoutMs: 60000
```

The shipped row carries no `--project`/`--vault`/`--index`: `auto-memory mcp`
resolves them from `~/.config/auto-memory/config.json` (`index`,
`default_project`). If you do pass `--project`, it takes a **permalink** — the
`permalink` column of `auto-memory project list`, not `name`. An empty list means
nothing is indexed yet: run `auto-memory project add "<name>" <vault>` first.
Getting this wrong is the usual reason a client connects but shows no tools.

**The row must live under `- insert:`.** DSH patch semantics are *id-targeted*:
a bare `- id: mcp-auto-memory` row tries to **modify an existing entry** and the
loader rejects it with `entry "mcp-auto-memory" not found`. Because this plugin
is not in any bundle, it has to be added, which is what `insert` is for. The
complete block:

```yaml
- insert:
    - id: mcp-auto-memory
      name: "@deepseek-ai/dsh-mcp-client"
      config:
        serverName: memory
        transport: stdio
        command: /Users/you/.cargo/bin/auto-memory
        args: ["mcp", "--project", "my-project", "--read-only"]
        toolCallTimeoutMs: 60000
        failOnStartupError: true
```

`failOnStartupError` is `false` by default, which makes a failed spawn look like
"this server has no tools". Set it to `true` for the first load, then relax it.

The desktop profile cannot be booted from the CLI (`profile "desktop" is managed
exclusively by the Electron application`), so validate the patch as an overlay
against another profile:

```sh
dsh --profile web --patch ~/.dsh/profiles/desktop/cordis.patch.yml --dump-config \
  | grep -A12 mcp-auto-memory
```

`scripts/wire-dsh-desktop.sh` in this repository does all three steps
idempotently; see [Scripted install](#scripted-install).

Save the file and DSH hot-swaps the entry (disconnect + reconnect) without a
restart. Tools appear to the model as `mcp__memory__search_notes`,
`mcp__memory__write_note`, … — the server-qualified shape Claude Code and Codex
use, which is also what keeps this server's tools from colliding with another
one's.

Drop `--read-only` only if you want the agent writing notes; it hides the six
mutating tools (`write_note`, `edit_note`, `move_note`, `delete_note`,
`create_memory_project`, `delete_project`) from `tools/list`.

The full walkthrough — vault preparation, `reindex`, the vault/index/project
distinction that most "connected but no data" reports come from — is
[`docs/integration-guide.md`](../../docs/integration-guide.md) §3–§7.

### Scripted install

[`scripts/wire-dsh-desktop.sh`](../../scripts/wire-dsh-desktop.sh) performs all
three steps for the **desktop** profile and is idempotent (it replaces its own
insert block rather than stacking a second one, and backs up the patch file
first):

```sh
sh scripts/wire-dsh-desktop.sh
# overridable: VAULT=/path PROJECT=slug BIN=/path/to/auto-memory sh scripts/wire-dsh-desktop.sh
```

It writes outside the repository (`~/.dsh/profiles/desktop/cordis.patch.yml`,
`~/.dsh/skills`, `$VAULT/.dsh/`), so run it in a normal shell, not inside a
sandboxed agent session.

## Configure

Project mapping is read from JSON, project keys overriding user keys:

1. `$DSH_HOME/auto-memory.json` (default `~/.dsh/auto-memory.json`)
2. `<nearest ancestor>/.dsh/auto-memory.json`

```json
{
  "autoMemory": {
    "primaryProject": "my-project",
    "captureFolder": "deepseek/my-repo",
    "recallTimeframe": "7d",
    "checkpointOnCompact": true,
    "placementConventions": "Put decisions in decisions/."
  }
}
```

`$DSH_HOME` is honoured because that is where the harness itself resolves its
home; with it unset the loader falls back to `$HOME/.dsh`.

The engine reads `primaryProject`, `captureFolder`, `recallTimeframe`,
`recallPrompt`, `focus`, `placementConventions`, `sessionProfile`, `repository`,
`checkpointOnCompact`, and `captureEvents`. A malformed file **fails closed**
(capture and checkpointing disabled) rather than merging a partial route.

### Where the index and project come from

The shims pass no `--index` and no `--project`, so the hook resolves both itself,
from files — there is nothing to export in a shell profile. Two chains, each
highest precedence first:

- **Index:** `--index` → `$AUTO_MEMORY_INDEX` → the user config file → the
  built-in default (`~/.local/share/auto-memory/memory.db`).
- **Project:** `--project` → the mapping file's `primaryProject` (`.dsh/auto-memory.json`,
  or the nearest ancestor that has one) → `default_project` in the user config
  file. The mapping file is the *more* specific source, so it wins.

The user config file, `~/.config/auto-memory/config.json`:

```json
{ "index": "~/.local/share/auto-memory/memory.db", "default_project": "my-project" }
```

With no project named anywhere, the hook prints the first-run nudge instead of
guessing.

Keys in the config file are **snake_case** (`default_project`), unlike the mapping
files' camelCase (`primaryProject`) — a camelCase key there is ignored as an
unknown key.

`AUTO_MEMORY_BIN` is separate: it selects the binary the shim invokes (default
`auto-memory` from `PATH`).

## What the hooks do (for a host that runs them)

| Event | Verb | Effect |
|---|---|---|
| `SessionStart` (`startup`/`resume`) | `hook session-start` | Prints a brief: pinned project, active tasks, open decisions, recent checkpoints. |
| `SessionStart` (`compact`) | `hook session-start` | Same brief, prefixed with the checkpoint request when `checkpointOnCompact` is on. |
| `PreCompact` | `hook pre-compact` | Captures nothing yet (lifecycle WAL not ported). The harness keeps only the hook's `control`, so the stdout is ignored. |

**The checkpoint flow**: because the `PreCompact` stdout is dropped, the request
is delivered by the *post-compaction* `SessionStart`. That prompt tells the
resumed agent to run the [`am-checkpoint`](skills/am-checkpoint/SKILL.md) skill,
which writes one **immutable** `deepseek_session` (or `coding_session`) note
through the MCP `write_note` tool and links it to its predecessor with
`continues [[…]]`.

Both hooks are **fail-open**: `auto-memory` exits 0 on every error path (missing
index, malformed stdin, unknown project, unknown harness), so a hook can never
break a session. stdout carries the brief and nothing else; diagnostics go to
stderr.

## Isolation

The three packages share one vault and one index, so the boundary that matters is
**which settings file each harness reads** and **which note type each writes**:

| | `plugins/agents` (Codex) | `plugins/tact` (Tact) | `plugins/deepseek` (this one) |
|---|---|---|---|
| Plugin id | `auto-memory-rs` | `auto-memory-tact` | `auto-memory-deepseek` |
| `--harness` | `codex` | `tact` | `deepseek` |
| Source id (`agent:`) | `codex` | `tact` | `deepseek` |
| User mapping | `~/.codex/basic-memory.json` | `~/.tact/auto-memory.json` | `$DSH_HOME/auto-memory.json` |
| Project mapping | `.codex/basic-memory.json` | `.tact/auto-memory.json` | `.dsh/auto-memory.json` |
| Block key | `basicMemory` | `autoMemory` | `autoMemory` |
| Session note type | `codex_session` | `tact_session` | `deepseek_session` |
| Checkpoint title | `Codex checkpoint - …` | `Tact checkpoint - …` | `DeepSeek checkpoint - …` |
| Recalls | `codex_session` | `tact_session` + `codex_session` | `deepseek_session` |

Two deliberate differences from Tact:

- **No cross-harness recall.** Tact recalls `codex_session` notes because a vault
  may already hold Codex-authored checkpoints and hiding them would be worse than
  showing another host's. DeepSeek does not: its brief is scoped to what this
  harness wrote, so "which host produced this context" stays answerable from the
  brief alone.
- **The mapping directory is DSH's own.** `.dsh/` is also where DSH keeps its
  project-scoped skills and its home directory, which is why the mapping file is
  `auto-memory.json` rather than borrowing `.tact/` or `.codex/`.

The isolation is enforced in both directions by tests:
`deepseek_ignores_the_codex_and_tact_mapping_files`,
`codex_and_tact_ignore_the_deepseek_mapping_file`, and the CLI-level
`cli_deepseek_does_not_inherit_a_codex_mapping` in `tests/hook.rs`.

DSH loads the skills of **every** installed root, so if you also install the Codex
or Tact packages into `~/.dsh/skills`, you get near-identical skill sets and the
model may pick the wrong one. Install this package's skills into DSH and the
other packages into their own hosts.

## Not yet ported

The first slice covers the two verbs the harnesses need. Still missing from the
reference front door: the SPEC-55 envelope/inbox WAL, `hook stop|flush|status`,
`hook install|remove`, transcript extraction / auto-capture note writing, and the
Claude Code / Pi plugin packages. DSH additionally needs a hook host before any
of this runs automatically.
