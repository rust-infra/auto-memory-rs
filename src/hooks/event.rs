//! Harness hook stdin normalization.
//!
//! Each harness speaks its own hook JSON dialect; [`normalize`] maps it onto one
//! [`NormalizedHookEvent`] so the engine downstream is harness-agnostic. Ported
//! from `basic_memory.hooks.adapters` (Claude Code, Codex, Pi). Missing fields
//! normalize to empty/`None` rather than failing: hooks are fail-open, and a
//! partially-populated event is still worth acting on.

use serde_json::Value;

use super::profiles::Harness;

/// The v0 lifecycle events the hook front door handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// A session began (or resumed, or just finished compacting).
    SessionStarted,
    /// Compaction is imminent.
    CompactionImminent,
    /// A session ended.
    SessionEnded,
}

impl HookEvent {
    /// The reference envelope event name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStarted => "session_started",
            Self::CompactionImminent => "compaction_imminent",
            Self::SessionEnded => "session_ended",
        }
    }
}

/// One harness lifecycle event, normalized across Claude Code, Codex, Pi,
/// Tact, and the DeepSeek Harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHookEvent {
    /// SPEC-55 source id (`claude-code`, `codex`, `pi`, `tact`, `deepseek`).
    pub source: &'static str,
    /// The normalized event.
    pub event: HookEvent,
    /// Opaque, surface-defined session id (empty when the payload omitted it).
    pub session_id: String,
    /// Turn id, when the harness supplies one.
    pub turn_id: Option<String>,
    /// Working directory the harness ran in.
    pub cwd: String,
    /// Path to the transcript, when the harness supplies one.
    pub transcript_path: String,
    /// SessionStart: `startup|resume|compact|clear`; PreCompact: `manual|auto`.
    pub trigger: Option<String>,
    /// Active model slug, when known.
    pub model: Option<String>,
}

/// Normalize a raw hook payload for one harness and event.
///
/// Claude reports a SessionStart cause as `source` and a PreCompact cause as
/// `trigger`; both collapse into the trigger slot, so a caller reads one field.
pub fn normalize(harness: Harness, event: HookEvent, payload: &Value) -> NormalizedHookEvent {
    let object = payload.as_object();
    let field = |key: &str| -> Option<String> {
        object?
            .get(key)
            .map(|value| match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                _ => String::new(),
            })
            .filter(|text| !text.is_empty())
    };
    let trigger = field("trigger").or_else(|| field("source"));
    // Pi names its turn id `branch_id` in some payloads; Claude has neither.
    // Tact's payload is Codex-shaped (`build_payload` in
    // `crates/tact/src/plugin/hooks.rs` reports `turn_id` as the turn counter
    // and `model`), with one deliberate difference: `transcript_path` is
    // `null` rather than a path, which `field` already normalizes to empty.
    //
    // The DeepSeek Harness speaks the same dialect: its hook payload carries
    // `session_id`, `cwd`, `trigger`, `turn_id` and `model`, and no
    // `transcript_path`. It stays a separate branch from Tact's on purpose —
    // sharing a match arm is how the two identities would later get merged by
    // accident.
    let turn_id = match harness {
        Harness::Claude => None,
        Harness::Codex | Harness::Tact | Harness::Deepseek => field("turn_id"),
        Harness::Pi => field("turn_id").or_else(|| field("branch_id")),
    };
    let model = match harness {
        Harness::Claude => None,
        Harness::Codex | Harness::Pi | Harness::Tact | Harness::Deepseek => field("model"),
    };

    NormalizedHookEvent {
        source: harness.profile().source,
        event,
        session_id: field("session_id").unwrap_or_default(),
        turn_id,
        cwd: field("cwd").unwrap_or_default(),
        transcript_path: field("transcript_path").unwrap_or_default(),
        trigger,
        model,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn codex_normalizes_source_into_trigger_and_keeps_turn_and_model() {
        let payload = json!({
            "hook_event_name": "SessionStart",
            "session_id": "s1",
            "cwd": "/tmp/x",
            "source": "compact",
            "turn_id": "t1",
            "model": "gpt-5",
        });
        let event = normalize(Harness::Codex, HookEvent::SessionStarted, &payload);
        assert_eq!(event.source, "codex");
        assert_eq!(event.session_id, "s1");
        assert_eq!(event.cwd, "/tmp/x");
        assert_eq!(event.trigger.as_deref(), Some("compact"));
        assert_eq!(event.turn_id.as_deref(), Some("t1"));
        assert_eq!(event.model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn claude_has_no_turn_or_model_and_prefers_trigger_over_source() {
        let payload = json!({
            "session_id": "s2",
            "trigger": "manual",
            "source": "startup",
            "turn_id": "ignored",
            "model": "ignored",
        });
        let event = normalize(Harness::Claude, HookEvent::CompactionImminent, &payload);
        assert_eq!(event.source, "claude-code");
        assert_eq!(event.trigger.as_deref(), Some("manual"));
        assert_eq!(event.turn_id, None);
        assert_eq!(event.model, None);
    }

    #[test]
    fn pi_reads_branch_id_as_turn() {
        let payload = json!({"session_id": "s3", "branch_id": "b1", "model": "m"});
        let event = normalize(Harness::Pi, HookEvent::CompactionImminent, &payload);
        assert_eq!(event.turn_id.as_deref(), Some("b1"));
        assert_eq!(event.model.as_deref(), Some("m"));
    }

    /// Tact's real payload, field for field (`build_payload` in
    /// `crates/tact/src/plugin/hooks.rs`): the session id is the live one,
    /// `source` carries the SessionStart trigger, `turn_id` is the turn counter
    /// and `transcript_path` is `null` — not a path.
    #[test]
    fn tact_reads_its_real_payload_including_a_null_transcript_path() {
        let payload = json!({
            "session_id": "3f6a1c",
            "transcript_path": null,
            "cwd": "/Users/me/repo",
            "hook_event_name": "SessionStart",
            "model": "gpt-5",
            "permission_mode": "default",
            "turn_id": "0",
            "source": "startup",
        });
        let event = normalize(Harness::Tact, HookEvent::SessionStarted, &payload);
        assert_eq!(event.source, "tact");
        assert_eq!(event.session_id, "3f6a1c");
        assert_eq!(event.cwd, "/Users/me/repo");
        assert_eq!(event.transcript_path, "");
        assert_eq!(event.trigger.as_deref(), Some("startup"));
        assert_eq!(event.turn_id.as_deref(), Some("0"));
        assert_eq!(event.model.as_deref(), Some("gpt-5"));
    }

    /// The compaction trigger is the only SessionStart `source` a checkpoint
    /// rides on, so it has to survive normalization.
    #[test]
    fn tact_reports_the_compact_trigger_after_compaction() {
        let event = normalize(
            Harness::Tact,
            HookEvent::SessionStarted,
            &json!({"hook_event_name": "SessionStart", "source": "compact"}),
        );
        assert_eq!(event.trigger.as_deref(), Some("compact"));
    }

    #[test]
    fn missing_fields_normalize_to_empty_not_error() {
        let event = normalize(
            Harness::Codex,
            HookEvent::SessionStarted,
            &json!({"hook_event_name": "SessionStart"}),
        );
        assert_eq!(event.session_id, "");
        assert_eq!(event.cwd, "");
        assert_eq!(event.transcript_path, "");
        assert_eq!(event.trigger, None);
        assert_eq!(event.turn_id, None);
    }

    /// The DeepSeek Harness payload: the same dialect Tact reports
    /// (`turn_id`/`model`, no `transcript_path`), but it must normalize under
    /// its **own** source id — the source is what the checkpoint note records as
    /// `agent`, so sharing Tact's would make the two indistinguishable.
    #[test]
    fn deepseek_reads_its_payload_under_its_own_source_id() {
        let payload = json!({
            "session_id": "dsh-session-1",
            "cwd": "/Users/me/repo",
            "hook_event_name": "SessionStart",
            "model": "deepseek-v4.1-flash",
            "turn_id": "4",
            "source": "startup",
        });
        let event = normalize(Harness::Deepseek, HookEvent::SessionStarted, &payload);
        assert_eq!(event.source, "deepseek");
        assert_eq!(event.session_id, "dsh-session-1");
        assert_eq!(event.cwd, "/Users/me/repo");
        assert_eq!(event.trigger.as_deref(), Some("startup"));
        assert_eq!(event.turn_id.as_deref(), Some("4"));
        assert_eq!(event.model.as_deref(), Some("deepseek-v4.1-flash"));
    }

    #[test]
    fn deepseek_reports_the_compact_trigger_after_compaction() {
        let event = normalize(
            Harness::Deepseek,
            HookEvent::SessionStarted,
            &json!({"hook_event_name": "SessionStart", "source": "compact"}),
        );
        assert_eq!(event.trigger.as_deref(), Some("compact"));
    }

    /// Every harness parses from its `--harness` spelling and back, so the CLI
    /// surface and the profile table cannot drift.
    #[test]
    fn every_harness_round_trips_its_spelling() {
        for harness in [
            Harness::Claude,
            Harness::Codex,
            Harness::Pi,
            Harness::Tact,
            Harness::Deepseek,
        ] {
            assert_eq!(Harness::parse(harness.as_str()), Some(harness));
        }
        assert_eq!(Harness::parse("deepseek-harness"), None);
    }

    /// Source ids are what a checkpoint note records as `agent`; two harnesses
    /// sharing one would make "which host wrote this" unanswerable.
    #[test]
    fn source_ids_are_distinct() {
        let sources = [
            Harness::Claude,
            Harness::Codex,
            Harness::Pi,
            Harness::Tact,
            Harness::Deepseek,
        ]
        .map(|harness| harness.profile().source);
        let mut unique = sources.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            sources.len(),
            "duplicate source id: {sources:?}"
        );
    }

    /// Only the harnesses that drop `PreCompact` stdout ask for the checkpoint
    /// after compaction. Claude and Pi have no checkpoint path at all.
    #[test]
    fn only_post_compaction_harnesses_request_a_checkpoint() {
        assert!(Harness::Codex.checkpoint_after_compaction());
        assert!(Harness::Tact.checkpoint_after_compaction());
        assert!(Harness::Deepseek.checkpoint_after_compaction());
        assert!(!Harness::Claude.checkpoint_after_compaction());
        assert!(!Harness::Pi.checkpoint_after_compaction());
    }
}
