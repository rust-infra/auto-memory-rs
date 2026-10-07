# Configuration Discovery — Design Spec

**Status:** proposed (2026-10-06). Supersedes nothing; resolves open decision #1 of
`specs/compatibility-spec.md` §7 for the *runtime* surfaces (the index-location question) and
leaves the on-disk *project* layout as-is.

**Why this is not a compatibility contract:** the reference implementation resolves
configuration through `BASIC_MEMORY_CONFIG_DIR` + `~/.config/basic-memory/config.json` and has no
plugin/hook front end of its own to keep working. This spec covers surfaces the reference does
not have (`auto-memory hook`), so it is a design spec with a deliberate deviation, not a port.

## 1. Problem

Every value the tool needs — which index, which project, which vault — is today supplied by a
**flag or a process environment variable**:

| Value | Current chain |
|---|---|
| index | `--index` → `AUTO_MEMORY_INDEX` → `~/.local/share/auto-memory/memory.db` |
| project (hook) | `--project` → the harness mapping file's `primaryProject` |
| vault | `--vault` (required by `reindex`; `mcp`/`watch` reconcile against it) |

Environment variables are the wrong mechanism for this product, for a reason that is structural
rather than aesthetic:

- A hook is a **child of the host agent**, so it inherits the *host process's* environment. The
  only way to configure it is a shell profile — i.e. per-user, per-login, global to every agent
  on the machine. The identity being configured (which harness is running) is per-host; the
  channel is per-process. They do not match.
- Two agents on one machine cannot hold different values.
- `docs/hooks.md` §4 already has to instruct the user to "export the `AUTO_MEMORY_*` variables in
  the **host process**", which is a documentation smell: the product is asking the user to do the
  product's job.

Evidence that the current default is wrong: `docs/integration-guide.md` §9's first troubleshooting
row for "client connected but finds nothing" is a path typo in a flag, and §2.1 documents that a
wrong `--vault` **prunes the project's index rows** when `mcp`/`watch` reconcile. Both are
consequences of asking the user to re-state facts the index already knows.

## 2. What the index already knows

`projects` rows carry the vault path (`ProjectRow.path`, `src/storage/records.rs:8-19`), written
by `upsert_project`. So for any project that has ever been registered:

- the vault is recoverable from the index — the user should not have to pass `--vault` again;
- deriving it is **safer** than accepting it: a typo can no longer point reconcile at the wrong
  directory and prune rows.

This is the fact the whole spec is built on: *configuration that the index can prove should not
be re-stated on the command line.*

## 3. Resolution chains

Precedence, highest first. `[new]` marks a step this spec adds.

### 3.1 Index path

1. `--index <path>`
2. `$AUTO_MEMORY_INDEX` *(kept; deprecated, see §6)*
3. `[new]` user config `index` key (§4)
4. default `~/.local/share/auto-memory/memory.db`

### 3.2 Project (permalink)

1. `--project <permalink>`
2. the harness mapping file's `primaryProject` — `.tact/auto-memory.json` (an `autoMemory`
   block; auto-memory's own file, since nothing else reads `.tact/`),
   `.codex/basic-memory.json`, `.claude/settings.json`, `.pi/basic-memory.json` (the reference
   product's names, which it reads too), resolved from `--project-dir` or the payload `cwd`
   *(unchanged; `src/hooks/settings.rs`)*
3. `[new]` user config `default_project` key (§4)
4. no project: print the first-run nudge and stop (fail-open, §5)

### 3.3 Vault

1. `--vault <path>` (explicit override; still validated)
2. `[new]` the registered project row's `path` (`ProjectRow.path`)
3. no project row: `--vault` is **required** (first `reindex` of a new vault has nothing to look
   up), and its absence is a usage error naming that fact.

`reindex --vault … --project …` keeps working exactly as documented. What changes is that after
the first registration, `reindex` / `watch` / `mcp` no longer *need* `--vault`, and `mcp` in
particular can no longer be pointed at a vault that does not match its project.

## 4. User config file

`[new]` `$XDG_CONFIG_HOME/auto-memory/config.json`, defaulting to
`~/.config/auto-memory/config.json`. It is the **user-level** tier; the project tier stays the
per-harness mapping files of §3.2.

```json
{
  "index": "~/.local/share/auto-memory/memory.db",
  "default_project": "oracle",
  "permalinks_include_project": true,
  "ensure_frontmatter_on_sync": true,
  "disable_permalinks": false,
  "index_changes": true,
  "update_permalinks_on_move": false
}
```

**Keys are snake_case**, like the reference's own `config.json` and the rest of `Config`. The
harness mapping files of §3.2 are the ones that use camelCase (`primaryProject`), because they
follow their host's conventions — a camelCase key here is an *unknown* key and is ignored, which
is exactly the silent failure the `Config` loader's leniency would otherwise hide. A test pins it.

- The last five keys are the existing `Config` struct (`src/config.rs`) — **today it is dead code:
  nothing in the repository reads it** (`docs/README.md`, productization pass; the reference
  defaults are hard-coded instead). This spec is what makes it live: unknown keys stay ignored, so
  a reference `config.json` can still be loaded, and the behavior knobs finally have an entry
  point.
- `index` and `default_project` are additions. Paths support a leading `~`.
- Precedence is per-key, not per-file: a file that sets only `index` does not shadow
  `default_project` from anywhere else.
- **Malformed file** (unreadable / not JSON / not an object): for `hook`, warn and continue with
  the remaining chain (fail-open, §5); for every other command, fail with a message naming the
  file. This mirrors the existing split between the hook's fail-open contract and the CLI's
  fail-closed one.

## 5. Failure semantics

| Surface | Malformed config | No project resolved | Vault unresolvable |
|---|---|---|---|
| `hook session-start` / `pre-compact` | warn on stderr, continue down the chain | first-run nudge, exit 0 | n/a (the hook never needs the vault) |
| `reindex` / `watch` / `mcp` | error, naming the file | `project not found` + the registered permalinks | usage error naming `--vault` |
| `doctor` | reports the file as unusable | reports "no project resolved" | reports which step of §3.3 failed |

`doctor` is the diagnostic surface for the whole chain: it already prints the index, schema
version, projects, vault, and the two optional semantic-search pieces — this spec adds **where
each value came from** (which file, which step, or "default"), so "why is it using that index" is
one command away. That is the same read-only posture as today: `doctor` must not create the file
it is inspecting.

## 6. First-run nudge becomes actionable

Today, when no project resolves, the hook prints the profile's `setup_nudge` ("this repo is not
configured yet, add `.tact/auto-memory.json`"). It does not say **what to put in it**, and the
user has to run `auto-memory project list` to find a permalink.

`[new]` The message gains a second paragraph naming the permalinks the index actually has:

```
# Auto Memory

_This repo is not configured for Auto Memory yet. Add `.tact/auto-memory.json` with an
`autoMemory.primaryProject` naming the project permalink to turn on session briefings for this
repo._

_Registered projects in this index: oracle, work-notes._
```

A separate paragraph rather than a clause appended to the profile's text: the nudge is
per-harness prose that ends with its own emphasis, and splicing into it would break the first
time a profile is reworded.

- Empty or unreadable index → the current wording, unchanged.
- The list is capped (10) with a trailing `…`; reading it is best-effort and never an error.
- **It must not create the index.** `Store::open` creates a missing database, so the lookup
  checks `Path::exists` first: a diagnostic that writes is a diagnostic you cannot trust.
- No guessing: the hook does **not** fall back to "the only registered project". Picking a
  project the user did not name would brief a session from the wrong knowledge graph, and a
  wrong brief is worse than no brief.

**Also fixed here (found while implementing §6):** the message was gated on "no settings source
was found", so a user whose mapping file exists but has no `primaryProject` got **nothing** — no
brief and no explanation, because the profile's `pin_tip` is only reachable from inside the brief
builder. Now that case prints the `pin_tip` plus the same candidate list. Silence was the one
outcome the two profiles' nudge/tip pair was never meant to produce.

## 7. Compatibility impact

- **No default changes**: with no user config file and no flags, every chain resolves exactly as
  it does today.
- `AUTO_MEMORY_INDEX` / `AUTO_MEMORY_BIN` keep working; they stop being the documented mechanism.
  `AUTO_MEMORY_BIN` is the plugin shim's business and is not affected by this spec.
- `--vault` / `--index` / `--project` keep their meanings; `--vault` becomes optional where §3.3
  can prove it.
- The plugin packages (`plugins/agents`, `plugins/tact`) need **no** change: they pass flags, and
  flags still win. Their READMEs' "Environment" sections get the deprecation note.
- Golden corpus: untouched (it drives the CLI with explicit flags).

## 8. Rust rules

- Resolution lives in one place. `src/config.rs` grows the load + `~` expansion, and a single
  `resolve_*` family owns the chains of §3; the CLI and the hook call it rather than each
  re-deriving precedence. A second implementation of "which index" is how the hook and the CLI
  drift apart.
- Precedence order is data (an ordered list), not nested `if let`s, so a test can assert the
  sequence rather than sample it.
- Every step is unit-tested with a table: value present at each level, absent at each level, and
  the malformed-config case for both failure policies.
- No new dependency: `serde_json` and `std::env`/`dirs`-free path handling (the repo already
  expands `~` from `$HOME` in `default_index_path`).

## 9. Open decisions

1. **Read the reference's config?** `~/.config/basic-memory/config.json` holds the same kind of
   data under a different `projects` shape. Recommendation: **do not read it**, but have `doctor`
   mention its presence, because adopting another implementation's index path silently invites two
   writers on one `memory.db` (`docs/integration-guide.md` §7.3 warns about exactly that).
2. **`XDG_DATA_HOME` for the default index?** The current default hard-codes
   `~/.local/share/auto-memory/memory.db`; the config file makes the location moot, so moving it is
   not worth a migration. Recommendation: leave it.
3. **A per-project file for the vault.** A repo-local mapping file could carry a `vault` key so a
   monorepo can point several repos at one project. Recommendation: defer until someone asks; the
   index registry (§2) covers the common case without a new file format.
4. **Does `mcp` still accept `--vault` at all?** Recommendation: yes, as an override, because a
   first-time user may register the project and start the server in one step.

## 10. Definition of done

- The chains of §3 are implemented once and used by both the CLI and `auto-memory hook`.
- `auto-memory doctor` prints the resolved value *and its origin* for index, project, and vault.
- The §6 nudge names the registered permalinks.
- `tests/hook.rs` covers: hook with only a payload `cwd` + user config (no flags, no env); hook
  with a malformed user config (warn + exit 0); `reindex` with `--vault` omitted after
  registration; `mcp` deriving the vault from the project row.
- `docs/usage.md` §install and both plugin READMEs stop presenting `AUTO_MEMORY_INDEX` as the way
  to configure the tool.
