# auto-memory-rs — project notes

Curated, long-lived notes. Daily logs live beside this file as `YYYY-MM-DD.md`.

## Conventions

- `.workbuddy-ai/` is real project data, not scratch — it is **tracked**, so it is committed
  alongside the docs rather than left dirty in `git status`. `.idea/` is ignored.
- `tests/golden/manifest.json` is capture evidence — never "unify" paths in it.
- Docs are bilingual and are synced at push time, in one pass (not per edit).

## Gates

- `./scripts/check-rust.sh` = `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --all-targets --locked`, `cargo doc --no-deps --locked`. Wired as `.githooks/pre-push`
  (opt in per clone: `git config core.hooksPath .githooks`).
- The script exports `no_proxy=127.0.0.1,localhost` because the mock-HTTP tests otherwise leave
  through a proxy and never come back.
- **The watcher tests used to be blamed on the environment — that was wrong.** 5 of them
  (`tests/async_transport.rs` (3), `tests/watch_golden.rs` (1), `tests/obsidian_compatibility.rs` (1))
  were failing on a pristine tree because FSEvents reports canonical paths while the watcher's root
  was not canonical (`/var/…` vs `/private/var/…`), so `map_notify_event` dropped every event.
  Fixed 2026-10-07 by canonicalizing the root once in `VaultWatcher::new` (`resolve_watch_root`).
  **Lesson: a failing filesystem-watcher test is not automatically a sandbox limitation — probe the
  event path against the root before writing it off.** See the user-level `MEMORY.md` for the probes.
  Everything else in the suite is green (168 lib tests).

## Integration surface

- **`watch` reconciles *before* installing the OS watch — a known, deliberately unfixed gap.**
  A write that lands during the startup catch-up raises no event and the backend cannot replay
  one it never saw, so it waits for the next start. Making it airtight means installing first
  and moving the catch-up into `watch_vault`/`watch_once`; that was implemented 2026-10-07 and
  **reverted the same day** on Rg's call — no test can pin the ordering (the install moment is
  unobservable, the gap is a window rather than an event), so the change was not worth carrying.
  Report it, don't re-implement it unless a pin appears. `VaultWatcher::with_ready_signal` means
  "the OS watch is installed", nothing more.
- `auto-memory` is a **standalone product**, like Basic Memory: agents consume it over MCP
  (`auto-memory mcp`) or via plugin command hooks. Do **not** link the `auto_memory` library into
  another workspace — AGPL + unconditional `fastembed`/`ort` + `&mut Store`-shaped services.
  See `docs/tact-ui-integration.md`.
- The built-in hook front end (`auto-memory hook <verb> --harness <…>`) supports
  `claude` / `codex` / `pi` / `tact`. `tact` was added on top of the reference; its hook payload
  is Codex-shaped (`source`, `turn_id`, `model`, `transcript_path: null`).
- `plugins/agents` (Codex) and `plugins/tact` (Tact) are two packages over one skills/schemas set.
  Install one per host: Tact loads every installed plugin's skills and hooks, so both installed
  means two briefs and two `am-*` skill sets. Their vocabularies differ on purpose
  (`codex_session` vs `tact_session`, `codex_turn_id` vs `turn_id`) and each is a **joint
  contract with the engine** — flip both sides together.

## Configuration

- Locations resolve through one chain, implemented once in `src/config.rs`
  (`resolve_index` / `resolve_project` + `Origin`) and used by every command and by the hook:
  flag → environment → `~/.config/auto-memory/config.json` → default; the vault additionally falls
  back to the registered project row. Spec: `specs/config-discovery-spec.md`; plan (complete):
  `plans/config-discovery-plan.md`.
- Config-file keys are **snake_case** (`default_project`); the per-harness mapping files are
  camelCase (`primaryProject`). A camelCase key in the config file is silently ignored, which is
  why a test pins the difference.
- A malformed config file is an **error for the CLI** and a **warning for the hook** — one chain,
  two policies. `doctor` prints each value's origin.
- **Mapping-file names are per harness, and the rule is "who else reads it".** Harnesses the
  reference product also serves keep the reference's own names — `.codex/basic-memory.json`
  (`basicMemory`), `.pi/basic-memory.json`, `.claude/settings.json` — because its hook reads the
  same files, so the name is a shared contract. Tact is ours alone (nothing under `.tact/` is read
  by anything else), so it uses `.tact/auto-memory.json` under an `autoMemory` block. The engine
  takes the pair as parameters (`load_agent_dir_settings`); `plugins/tact` must move with it.
- Rg's real vault is `~/agent-memory`, registered here as project **`1m`** — deliberately the same
  name Basic Memory uses, so the project permalink and every note's `1m/…` permalink agree (the
  notes carry their own `permalink:` in frontmatter, written by Basic Memory, and frontmatter
  wins over the generated prefix). It is also a tact workdir, and the Tact mapping
  `~/.tact/auto-memory.json` pins `primaryProject: 1m`. Renaming the project back to anything else
  re-splits the names and silently no-ops the hook.
