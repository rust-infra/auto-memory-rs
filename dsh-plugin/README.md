# auto-memory-dsh — DeepSeek Harness bundle

A DSH **bundle** (npm package) that wires a local `auto-memory-rs` index into
DeepSeek Harness: the six `am-*` skills plus the `auto-memory mcp` server.

This is a different distribution channel from `plugins/deepseek/`, which targets
Codex/Tact-style command hooks. DSH has no command-hook host, so a bundle is the
only form that actually runs there.

## What it installs

Two rows, both `insert` (this package is not part of any DSH bundle, so a bare
`- id:` row would be rejected with `entry "…" not found`):

| Row id | Plugin | Effect |
|---|---|---|
| `auto-memory-skill-filesystem` | `@deepseek-ai/dsh-skill-filesystem` | publishes this package's `skills/` through the shared skill registry (`providerName: auto-memory`, `includeDefaultRoots: false` so the host's own provider keeps owning the project/user roots) |
| `auto-memory-mcp` | `@deepseek-ai/dsh-mcp-client` | mounts `auto-memory mcp` over stdio; tools appear as `mcp__memory__*` |

The skill root is resolved at load time with
`createRequire(baseUrl).resolve('auto-memory-dsh/package.json')`, which is the
only correct answer under pnpm's content-addressed layout — concatenating a path
onto `baseUrl` would point into the virtual store's symlink farm.

## Install

Two commands. Nothing to edit by hand.

```bash
# 1. install the bundle
dsh plugin --profile desktop add /absolute/path/to/auto-memory-rs/dsh-plugin

# 2. tell auto-memory which project to serve
auto-memory project default 1m
auto-memory project list         # the default is marked with `*`
```

`project default` writes `default_project` into `~/.config/auto-memory/config.json`
for you, and refuses a permalink the index does not have (so a typo fails here
rather than on every later command). If the project does not exist yet, register
and pin it in one step:

```bash
auto-memory project add "My Vault" ~/vault --default
```

`index` is optional and defaults to `~/.local/share/auto-memory/memory.db`; pass
`auto-memory config set index <path>` only if your index lives elsewhere.

Then fully quit and reopen the app (the bundle list is read at startup; a page
refresh is not enough). Tools appear as `mcp__memory__*` and the six `am-*`
skills load from the package.

**`default_project` is a permalink, and you probably do not know yours yet.**
Ask the index instead of guessing:

```bash
auto-memory project list
# name  entities  permalink                 path
# 1m           4  1m                        /Users/you/agent-memory
```

The `permalink` column is the value to use (the `name` column is rejected with
`project not found`). If the list is empty, nothing is indexed yet:

```bash
auto-memory project add "My Vault" ~/vault     # registers + indexes
```

### Why there is nothing else to configure

The bundle ships `args: ["mcp", "--read-only"]` with no `--project`, `--vault`,
or `--index`, because `auto-memory mcp` resolves all three itself:

- **index** — `--index` → `$AUTO_MEMORY_INDEX` → `~/.config/auto-memory/config.json` → the
  standard per-user path;
- **project** — `--project` → the harness mapping file → `default_project` in that same
  config file;
- **vault** — the registered project's own path.

`command: auto-memory` resolves through `PATH`, which `dsh-subprocess` preserves
when it scrubs the child environment. A machine-specific path therefore never has
to be baked into the package.

### Overriding per deployment

If one profile should serve a different vault or project, override the row by id
in that profile's `cordis.patch.yml`. This is an **id-targeted override** — the
row already exists (the bundle inserted it), so a bare `- id:` *modifies* it
rather than adding a second one:

```yaml
- id: auto-memory-mcp
  config:
    args: ["mcp", "--project", "other", "--vault", "/path/to/vault", "--read-only"]
```

Validate without booting the desktop app (it is Electron-managed and the CLI
refuses to compose it directly — copy it to a probe profile instead):

```bash
cp -R ~/.dsh/profiles/desktop ~/.dsh/profiles/probe
# set "name" in probe/package.json to dsh-profile-probe
dsh --profile probe --dump-config | grep -A10 auto-memory-mcp
```

## Verify the skill root

The `bundledSkillDir` expression is the one piece that can fail silently. Check
it against the profile pnpm actually installed into:

```bash
node -e "
const { createRequire } = require('node:module');
const { dirname, join } = require('node:path');
const fs = require('node:fs');
const baseUrl = 'file://' + process.argv[1] + '/package.json';
const m = createRequire(baseUrl).resolve('auto-memory-dsh/package.json');
const skills = join(dirname(m), 'skills');
console.log(skills, fs.existsSync(skills), fs.readdirSync(skills).join(', '));
" ~/.dsh/profiles/desktop
```

Expected: `…/node_modules/auto-memory-dsh/skills true am-checkpoint, am-decide, …`

## Config file

Skills and the MCP server do not read a config file, but the `am-*` skills and
the `auto-memory hook` front end do:

1. `$DSH_HOME/auto-memory.json` (default `~/.dsh/auto-memory.json`)
2. `<nearest ancestor>/.dsh/auto-memory.json`

```json
{ "autoMemory": { "primaryProject": "my-project" } }
```

`$DSH_HOME` already **is** the `.dsh` directory, so the user-level file sits
directly in it; the project-level one is under `.dsh/` in the checkout. See
[`plugins/deepseek/README.md`](../plugins/deepseek/README.md) for the full
settings vocabulary and the isolation guarantees against Codex/Tact.

## Checks

`scripts/check-dsh-plugin.sh` runs in `scripts/check-rust.sh` and in CI. It
verifies the three things that rot silently:

- `skills/` and `schemas/` still match `plugins/deepseek` (fix with
  `sh scripts/sync-dsh-plugin.sh`);
- `package.json` still declares `dsh.bundle.patch` and every `files[]` entry
  exists;
- every row is under `- insert:` — a top-level `- id:` row is rejected at load
  with `entry "…" not found`, which is exactly the bug this check guards.

The `dsh-bundle` CI job goes further: it packs the tarball, installs it into a
throwaway profile with the published DSH CLI, and resolves the skill root through
`createRequire` the same way the Loader does.

## Not included

- **Command hooks.** DSH has no `SessionStart`/`PreCompact` host; `plugins/deepseek/hooks/`
  ships the contract for a host that does, but nothing here runs it.
- **The `auto-memory` binary.** The bundle assumes `auto-memory` is on `PATH`
  (or a `command:` override pointing at it).
- **Indexing.** Run `auto-memory reindex` / `watch` separately; the MCP server
  reconciles once at startup and does not watch the vault.
