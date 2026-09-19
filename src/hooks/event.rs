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

/// One harness lifecycle event, normalized across Claude Code, Codex, and Pi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHookEvent {
    /// SPEC-55 source id (`claude-code`, `codex`, `pi`).
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
    let turn_id = match harness {
        Harness::Claude => None,
        Harness::Codex => field("turn_id"),
        Harness::Pi => field("turn_id").or_else(|| field("branch_id")),
    };
    let model = match harness {
        Harness::Claude => None,
        Harness::Codex | Harness::Pi => field("model"),
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
}
