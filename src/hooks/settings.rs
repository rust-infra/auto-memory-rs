//! Harness settings resolution.
//!
//! Ported from the reference `load_*_settings` family in
//! `basic_memory.cli.commands.hook`. Each harness stores its Basic Memory
//! mapping in a different place; this module merges user-level and project-level
//! files and reports whether *any* source was found, which drives the first-run
//! setup nudge. A malformed source **fails closed** (capture disabled) rather
//! than mixing a later route with incomplete earlier settings.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::profiles::{Harness, HarnessProfile};

/// The resolved Basic Memory mapping for one harness evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `primaryProject` — the project permalink checkpoints and recall target.
    pub primary_project: Option<String>,
    /// Folder checkpoints are written to.
    pub capture_folder: String,
    /// SessionStart recall window (e.g. `7d`).
    pub recall_timeframe: String,
    /// Overrides the profile's default recall guidance when set.
    pub recall_prompt: Option<String>,
    /// Short user-declared emphasis shown in the brief header.
    pub focus: Option<String>,
    /// Project placement conventions surfaced in the "where to write" section.
    pub placement_conventions: Option<String>,
    /// `general` (default) or `coding`.
    pub session_profile: Option<String>,
    /// Stable repository identifier, required when `session_profile` is coding.
    pub repository: Option<String>,
    /// Whether a post-compaction checkpoint is requested.
    pub checkpoint_on_compact: bool,
    /// Whether lifecycle events are captured (unused in this slice, kept for parity).
    pub capture_events: bool,
}

impl Settings {
    fn defaults(profile: &HarnessProfile, capture_folder: String) -> Self {
        Self {
            primary_project: None,
            capture_folder,
            recall_timeframe: profile.default_recall_timeframe.to_owned(),
            recall_prompt: None,
            focus: None,
            placement_conventions: None,
            session_profile: None,
            repository: None,
            checkpoint_on_compact: false,
            capture_events: false,
        }
    }

    /// Whether the coding session profile is selected.
    pub fn is_coding(&self) -> bool {
        self.session_profile.as_deref() == Some(super::profiles::CODING_SESSION_PROFILE)
    }
}

/// The directory used for project mapping: an explicit override, else the hook
/// payload's `cwd`, else the process working directory.
pub fn mapping_dir(override_dir: Option<&Path>, event_cwd: &str) -> PathBuf {
    if let Some(dir) = override_dir {
        return dir.to_path_buf();
    }
    if !event_cwd.is_empty() {
        return PathBuf::from(event_cwd);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Load the settings for one harness, plus whether any source was configured.
pub fn load_harness_settings(harness: Harness, directory: &Path) -> (Settings, bool) {
    let home = home_dir();
    match harness {
        Harness::Claude => {
            let user_dir = claude_config_dir(home.as_deref());
            load_claude_settings(directory, home.as_deref(), &user_dir)
        }
        Harness::Codex => load_codex_settings(directory, home.as_deref()),
        Harness::Pi => load_pi_settings(directory),
    }
}

// --- Codex ---

fn load_codex_settings(directory: &Path, home: Option<&Path>) -> (Settings, bool) {
    let profile = Harness::Codex.profile();
    let mut settings = Settings::defaults(profile, codex_capture_folder(directory));
    // Codex lifecycle capture and checkpoint prompting are enabled when omitted.
    settings.checkpoint_on_compact = true;
    settings.capture_events = true;

    let mut sources: Vec<PathBuf> = Vec::new();
    if let Some(home) = home {
        sources.push(home.join(".codex").join("basic-memory.json"));
    }
    let project = project_dir(directory, |dir| {
        dir.join(".codex").join("basic-memory.json").is_file()
    });
    let project_path = project.join(".codex").join("basic-memory.json");
    if sources.first() != Some(&project_path) {
        sources.push(project_path);
    }

    let mut found = false;
    for path in sources {
        match read_json_object(&path) {
            None => continue,
            Some(None) => {
                // Malformed configured source: fail closed for the whole evaluation.
                settings.checkpoint_on_compact = false;
                settings.capture_events = false;
                return (settings, true);
            }
            Some(Some(data)) => {
                found = true;
                if let Some(block) = codex_block(&data) {
                    apply_block(&mut settings, block);
                }
            }
        }
    }
    (settings, found)
}

/// Codex reads `basicMemory` when present, else the whole document; a non-object
/// `basicMemory` is malformed and disables recall.
fn codex_block(data: &serde_json::Map<String, Value>) -> Option<&serde_json::Map<String, Value>> {
    match data.get("basicMemory") {
        Some(Value::Object(block)) => Some(block),
        Some(_) => None,
        None => Some(data),
    }
}

/// Namespace the Codex capture folder by the current repository directory, so
/// one user-level config serves many checkouts.
fn codex_capture_folder(directory: &Path) -> String {
    let repo_root = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(directory)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty());
    let Some(root) = repo_root else {
        return Harness::Codex.profile().default_capture_folder.to_owned();
    };
    match Path::new(&root)
        .file_name()
        .map(|name| name.to_string_lossy())
    {
        Some(name) if !name.is_empty() => format!("codex/{name}"),
        _ => Harness::Codex.profile().default_capture_folder.to_owned(),
    }
}

// --- Claude ---

fn load_claude_settings(
    directory: &Path,
    home: Option<&Path>,
    user_dir: &Path,
) -> (Settings, bool) {
    let profile = Harness::Claude.profile();
    let mut settings = Settings::defaults(profile, profile.default_capture_folder.to_owned());
    settings.capture_events = true;

    let mut sources: Vec<PathBuf> = vec![user_dir.join("settings.json")];
    let project = claude_project_dir(directory);
    // `~/.claude` is user-level config, not a project mapping: never re-enter it
    // as a higher-precedence project source.
    if home != Some(project.as_path()) {
        let seen: Vec<PathBuf> = sources
            .iter()
            .filter_map(|path| path.canonicalize().ok())
            .collect();
        for name in ["settings.json", "settings.local.json"] {
            let path = project.join(".claude").join(name);
            if path
                .canonicalize()
                .ok()
                .is_some_and(|canonical| seen.contains(&canonical))
            {
                continue;
            }
            sources.push(path);
        }
    }

    let mut found = false;
    for path in sources {
        match read_claude_block(&path) {
            ClaudeBlock::Absent => continue,
            ClaudeBlock::Malformed => return (settings, true),
            ClaudeBlock::Block(block) => {
                found = true;
                apply_block(&mut settings, &block);
            }
        }
    }
    (settings, found)
}

enum ClaudeBlock {
    /// File missing, or present without a `basicMemory` key.
    Absent,
    /// File present but unreadable, or `basicMemory` is not an object.
    Malformed,
    /// A usable `basicMemory` object.
    Block(serde_json::Map<String, Value>),
}

fn read_claude_block(path: &Path) -> ClaudeBlock {
    match read_json_object(path) {
        None => ClaudeBlock::Absent,
        Some(None) => ClaudeBlock::Malformed,
        Some(Some(data)) => match data.get("basicMemory") {
            None => ClaudeBlock::Absent,
            Some(Value::Object(block)) => ClaudeBlock::Block(block.clone()),
            Some(_) => ClaudeBlock::Malformed,
        },
    }
}

/// Claude Code treats `CLAUDE_CONFIG_DIR` as a literal replacement for
/// `~/.claude`, including relative and whitespace values.
fn claude_config_dir(home: Option<&Path>) -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(value) => PathBuf::from(value),
        None => home.map_or_else(|| PathBuf::from(".claude"), |home| home.join(".claude")),
    }
}

// --- Pi ---

fn load_pi_settings(directory: &Path) -> (Settings, bool) {
    let profile = Harness::Pi.profile();
    // Pi package automation defaults off for privacy.
    let mut settings = Settings::defaults(profile, profile.default_capture_folder.to_owned());
    settings.recall_timeframe = profile.default_recall_timeframe.to_owned();

    let project = project_dir(directory, |dir| {
        dir.join(".pi").join("basic-memory.json").is_file()
    });
    let path = project.join(".pi").join("basic-memory.json");
    let Some(data) = read_json_object(&path) else {
        return (settings, false);
    };
    let Some(data) = data else {
        return (settings, true);
    };
    // The Pi config names routes `project` / `projectId`; the engine keeps the
    // `primaryProject` shape internally.
    for key in ["projectId", "project_id", "project"] {
        if let Some(value) = data.get(key).and_then(Value::as_str)
            && !value.is_empty()
        {
            settings.primary_project = Some(value.to_owned());
            break;
        }
    }
    if let Some(value) = data.get("captureFolder").and_then(Value::as_str) {
        settings.capture_folder = value.to_owned();
    }
    if let Some(value) = data.get("recallTimeframe").and_then(Value::as_str) {
        settings.recall_timeframe = value.to_owned();
    }
    if let Some(value) = data.get("captureEvents").and_then(Value::as_bool) {
        settings.capture_events = value;
    }
    (settings, true)
}

// --- Shared ---

/// Apply one `basicMemory` block over the current settings. Unknown keys are
/// ignored; a key of the wrong type leaves the default in place.
fn apply_block(settings: &mut Settings, block: &serde_json::Map<String, Value>) {
    if let Some(value) = block.get("primaryProject").and_then(Value::as_str)
        && !value.is_empty()
    {
        settings.primary_project = Some(value.to_owned());
    }
    if let Some(value) = block.get("captureFolder").and_then(Value::as_str)
        && !value.trim().is_empty()
    {
        settings.capture_folder = value.to_owned();
    }
    if let Some(value) = block.get("recallTimeframe").and_then(Value::as_str)
        && !value.is_empty()
    {
        settings.recall_timeframe = value.to_owned();
    }
    if let Some(value) = block.get("recallPrompt").and_then(Value::as_str) {
        settings.recall_prompt = Some(value.to_owned());
    }
    if let Some(value) = block.get("focus").and_then(Value::as_str) {
        settings.focus = Some(value.to_owned());
    }
    if let Some(value) = block.get("placementConventions").and_then(Value::as_str) {
        settings.placement_conventions = Some(value.to_owned());
    }
    if let Some(value) = block.get("sessionProfile").and_then(Value::as_str) {
        settings.session_profile = Some(value.to_owned());
    }
    if let Some(value) = block.get("repository").and_then(Value::as_str) {
        settings.repository = Some(value.to_owned());
    }
    if let Some(value) = block.get("checkpointOnCompact").and_then(Value::as_bool) {
        settings.checkpoint_on_compact = value;
    }
    if let Some(value) = block.get("captureEvents").and_then(Value::as_bool) {
        settings.capture_events = value;
    }
}

/// Nearest ancestor (including `directory`) satisfying `predicate`; falls back to
/// `directory` itself when none does.
fn project_dir(directory: &Path, predicate: impl Fn(&Path) -> bool) -> PathBuf {
    let start = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    let mut current = start.clone();
    loop {
        if predicate(&current) {
            return current;
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return start,
        }
    }
}

fn claude_project_dir(directory: &Path) -> PathBuf {
    project_dir(directory, |dir| {
        dir.join(".claude").join("settings.json").is_file()
            || dir.join(".claude").join("settings.local.json").is_file()
    })
}

/// Read a JSON object from `path`.
///
/// `None` = the file is absent. `Some(None)` = present but unusable (read error,
/// invalid JSON, or not an object). `Some(Some(value))` = a parsed object.
fn read_json_object(path: &Path) -> Option<Option<serde_json::Map<String, Value>>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(None),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(object)) => Some(Some(object)),
        _ => Some(None),
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn codex_defaults_enable_capture_and_checkpointing() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let (settings, found) = load_codex_settings(project.path(), Some(home.path()));
        assert!(!found);
        assert!(settings.checkpoint_on_compact);
        assert!(settings.capture_events);
        assert_eq!(settings.capture_folder, "codex");
        assert_eq!(settings.recall_timeframe, "7d");
        assert_eq!(settings.primary_project, None);
    }

    #[test]
    fn codex_project_file_overrides_user_file() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        write(
            &home.path().join(".codex/basic-memory.json"),
            r#"{"primaryProject": "user", "checkpointOnCompact": false}"#,
        );
        write(
            &project.path().join(".codex/basic-memory.json"),
            r#"{"primaryProject": "project"}"#,
        );
        let (settings, found) = load_codex_settings(project.path(), Some(home.path()));
        assert!(found);
        assert_eq!(settings.primary_project.as_deref(), Some("project"));
        // The project file did not mention it, so the user value survives.
        assert!(!settings.checkpoint_on_compact);
    }

    #[test]
    fn codex_malformed_source_fails_closed() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        write(
            &project.path().join(".codex/basic-memory.json"),
            "{ not json",
        );
        let (settings, found) = load_codex_settings(project.path(), Some(home.path()));
        assert!(found);
        assert!(!settings.capture_events);
        assert!(!settings.checkpoint_on_compact);
    }

    #[test]
    fn claude_reads_basic_memory_block_and_ignores_missing_it() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        write(
            &project.path().join(".claude/settings.json"),
            r#"{"basicMemory": {"primaryProject": "demo", "recallTimeframe": "3d"}}"#,
        );
        let user_dir = home.path().join(".claude");
        let (settings, found) = load_claude_settings(project.path(), Some(home.path()), &user_dir);
        assert!(found);
        assert_eq!(settings.primary_project.as_deref(), Some("demo"));
        assert_eq!(settings.capture_folder, "sessions");
        assert_eq!(settings.recall_timeframe, "3d");

        // A settings file without a basicMemory block is "absent", not malformed.
        let other = tempfile::tempdir().expect("other");
        write(
            &other.path().join(".claude/settings.json"),
            r#"{"permissions": {}}"#,
        );
        let (settings, found) = load_claude_settings(other.path(), Some(home.path()), &user_dir);
        assert!(!found);
        assert_eq!(settings.primary_project, None);
    }

    #[test]
    fn mapping_dir_prefers_override_then_event_cwd() {
        let override_dir = PathBuf::from("/override");
        assert_eq!(
            mapping_dir(Some(&override_dir), "/event"),
            PathBuf::from("/override")
        );
        assert_eq!(mapping_dir(None, "/event"), PathBuf::from("/event"));
        assert!(!mapping_dir(None, "").as_os_str().is_empty());
    }
}
