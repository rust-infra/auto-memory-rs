//! Harness lifecycle hooks (`auto-memory hook ...`).
//!
//! Agents (Codex, Claude Code, Pi) call a hook command at session boundaries:
//! `SessionStart` on startup/resume/after-compaction, `PreCompact` before the
//! transcript is summarized. The hook is *advisory*: it reads the index and
//! prints context, and **every failure path is fail-open** — a hook must never
//! disrupt a session, so a missing index, malformed stdin, or unknown project
//! degrades to "no brief" rather than a non-zero exit.
//!
//! This is a scoped port of the reference hook front door
//! (`basic_memory.cli.commands.hook` + `basic_memory.hooks.adapters`). The first
//! slice implements the two verbs Codex needs — `session-start` (brief, plus the
//! post-compaction checkpoint prompt) and `pre-compact` — for the Codex harness.
//! Not yet ported: the SPEC-55 envelope/inbox WAL, the `install`/`remove`/
//! `status`/`flush` verbs, transcript extraction, and auto-capture note writing
//! for Claude/Pi. See `docs/hooks.md`.
//!
//! Contract (identical for Claude Code and Codex plugins):
//!
//! ```text
//! stdin : one JSON object  (hook_event_name, session_id, cwd, transcript_path, …)
//! stdout: the brief (plain text, or {"hookSpecificOutput": {…}}); nothing else
//! exit  : always 0
//! ```

pub mod brief;
pub mod checkpoint;
pub mod event;
pub mod profiles;
pub mod settings;

pub use event::{HookEvent, NormalizedHookEvent};
pub use profiles::{Harness, HarnessProfile};
pub use settings::{Settings, load_harness_settings, mapping_dir};

pub use brief::build_session_brief;
pub use checkpoint::checkpoint_prompt;
pub use event::normalize;
