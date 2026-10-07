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
- **Known environment failure:** 5 watcher tests fail in the sandbox (see the user-level
  `MEMORY.md`) — `tests/async_transport.rs` (3), `tests/watch_golden.rs` (1),
  `tests/obsidian_compatibility.rs` (1). Verified pre-existing by stashing the working tree.
  Everything else in the suite is green (158 lib tests).

## Integration surface

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
