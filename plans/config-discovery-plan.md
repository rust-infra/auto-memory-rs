# Configuration Discovery — Execution Plan

- **Status:** complete (2026-10-06) — slices A–D delivered
- **Spec:** [`specs/config-discovery-spec.md`](../specs/config-discovery-spec.md)
- **Slice rule:** every step lands runnable and tested; the gates (`./scripts/check-rust.sh`)
  must be green at each step.

## 0. Progress

**Slice A — done (2026-10-06).** `src/config.rs` is live: `Config` gained `index` /
`default_project` (snake_case keys, matching the reference's own `config.json` — the harness
mapping files are the camelCase ones), `user_config_path()` / `load_user_config()` return
`Absent` / `Loaded` / `Malformed` so each caller picks its own policy, and `resolve_index` /
`resolve_project` are ordered chains returning `Resolved { value, origin }`. The hook, `doctor`,
and `project add|list|remove` all use them; the hook warns and continues on a malformed file while
the CLI errors; `doctor` gained a `config` check and now prints where each value came from. Seven
unit tests cover the chains (including the camelCase-key trap) and two new `tests/hook.rs` cases
drive the real binary with **only** a payload `cwd` + a user config, and with a broken config.
`tests/cli_project.rs` and `tests/hook.rs` now isolate `HOME`/`XDG_CONFIG_HOME`, so a developer's
own config file cannot change what they observe.

Two things this slice deliberately did **not** do, both recorded below: `--index` is still
required on the seven commands that took it as `PathBuf` (search/status/context/schema/reindex/
watch/mcp), and the vault work is Slice B.

**Slice B — done (2026-10-06).** `--vault` is optional on `reindex` / `watch` / `mcp` (falling
back to the registered project row's `path`) and `--index` is optional on all seven commands that
used to require it, so the config file now reaches the commands people actually run. The
precedence lives in one new function (`resolve_project_target`), and a given `--vault` is
directory-checked before any scan — including for `watch`/`mcp`, which previously skipped the
check and could reconcile against a missing directory.

## 1. Slices

### Slice A — the user config file and the resolution chains *(delivered)*

1. **`src/config.rs` becomes live.** Add `index` and `default_project` to `Config`, plus
   `user_config_path()` (XDG) and `load_user_config()`, which never fails: it returns
   `Absent` / `Loaded` / `Malformed` so the caller picks the policy (hook: warn and continue;
   CLI: error naming the file). Before this slice the whole module was dead code — nothing read it.
2. **One resolution family.** `resolve_index` / `resolve_project` as ordered, testable chains
   returning `Resolved { value, origin }` with `Origin::{Flag, Environment, Config, Default}`.
   Origin is not decoration: `doctor` must print it and the CLI's errors must name the step.
3. **The hook uses them.** `run_hook` stops reading `AUTO_MEMORY_INDEX` inline and stops
   re-deriving the project; a malformed user config warns and continues (fail-open).
4. **`doctor` reports origins** for the index and the project, and names a malformed config file.
5. **Tests:** table-driven unit tests for both chains (value at each level, absent at each level,
   malformed config under both policies); a `tests/hook.rs` case that runs the hook with **only**
   a payload `cwd` and a user config, no flags and no environment.

**Definition of done for Slice A:** with a user config file present, a hook invocation needs
nothing but the payload; with no config file, every chain resolves exactly as it does today.

### Slice B — the vault stops being re-stated *(delivered)*

6. `--vault` becomes `Option<PathBuf>` on `reindex` / `watch` / `mcp`, resolved as
   flag → the registered project row's `path` → usage error naming `--vault`.
7. The resolved vault is validated as a directory **before** any reconcile, so a stale registry
   entry fails loudly instead of pruning rows (`docs/integration-guide.md` §2.1).
8. **Also in this slice:** `--index` becomes `Option<PathBuf>` on the seven commands that still
   require it (`search`, `status`, `context`, `schema`, `reindex`, `watch`, `mcp`), so the config
   file's `index` key actually applies to the commands people run.
9. Tests: `reindex` without `--vault` after registration; `mcp` deriving it; both error paths.

**Delivered:** `resolve_project_target` (`src/main.rs`) owns the `(name, permalink, vault)`
precedence for all three commands, so it cannot drift between them; `cli_index` returns the index
*and* the loaded config, because a command that needs one usually needs `default_project` too.
`--index` is optional on all seven commands, `schema_context` resolves it internally so its three
verbs stay unchanged, and a given `--vault` is now directory-checked by `watch`/`mcp` as well as
`reindex`. Four new `tests/cli_project.rs` cases: reindex deriving the vault, the two refusal
paths, and `search` taking its index from the config file.

Note on §7: the check catches a **missing** vault; a path that exists but holds no notes can still
prune (the trap `docs/integration-guide.md` §2.1 documents). Omitting `--vault` is what removes
that risk, which is why the docs now lead with it.

### Slice C — the nudge becomes actionable *(delivered)*

10. The first-run nudge lists the registered permalinks (capped at 10, read from the index the
    hook already opened; a read failure degrades to today's wording).
11. Tests: an index with projects names them; an empty index keeps the current wording.

**Delivered:** `registered_permalinks` + `nudge_with_projects` in `src/main.rs`, appended as a
second paragraph rather than spliced into the profile's prose. The lookup checks `exists()` before
`Store::open`, because opening creates the index — a nudge that writes is one you cannot trust
(pinned by a test). Two `tests/hook.rs` cases: an index with a project names it; a missing index
keeps the generic wording *and* is still missing afterwards.

**Also fixed:** the message was gated on "no settings source found", so a mapping file that exists
without a `primaryProject` produced **silence** — no brief, no explanation, because the profile's
`pin_tip` is only reachable from inside the brief builder. That case now prints the `pin_tip` plus
the same candidate list. Recorded in `specs/config-discovery-spec.md` §6.

### Slice D — documentation *(delivered)*

12. `docs/usage.md` §3, both plugin READMEs, and `docs/integration-guide.md` §2.1 document the
    chains and stop presenting `AUTO_MEMORY_INDEX` as *the* way to configure the tool (it stays as
    a documented override).

**Not changed:** `docs/hooks.md` §2 lists the environment of `tools/auto-memory-hook.py`, a
standalone legacy script that reads its own variables and does not go through this chain.

## 2. Deliberately out of this plan

- Reading the reference's `~/.config/basic-memory/config.json` (spec §9.1) — `doctor` will
  mention it if present, but nothing consumes it.
- A per-project `vault` key in the mapping files (spec §9.3).
- Any change to the plugin packages: they pass flags, and flags still win.

## 3. Risks

- **Precedence drift.** The chains must exist once. If the CLI and the hook each keep their own
  copy, the first divergence will be invisible until a user reports a wrong brief.
- **Silent behavior change.** Every step must be covered by a test that fails when the step is
  removed, otherwise "flags still win" is a claim rather than a property.
- **`Config` gaining fields.** It is `PartialEq` and used in comparisons; the two new keys are
  `Option`s and default to `None`, so existing literals keep compiling — verify with `cargo
  clippy --all-targets` before touching call sites.
