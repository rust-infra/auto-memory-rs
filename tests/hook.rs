//! `auto-memory hook` — the harness SessionStart/PreCompact front door.
//!
//! Two layers are pinned here: the library-level brief (header, fenced data,
//! recall prompt, bound) and the CLI contract (stdout carries the brief only,
//! and every failure path stays fail-open with exit 0).

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use auto_memory::hooks::settings::Settings;
use auto_memory::hooks::{Harness, build_session_brief};
use auto_memory::indexing::{RebuildOptions, rebuild_vault};
use auto_memory::storage::Store;

fn codex_settings() -> Settings {
    Settings {
        primary_project: Some("oracle".to_owned()),
        capture_folder: "codex".to_owned(),
        recall_timeframe: "7d".to_owned(),
        recall_prompt: None,
        focus: None,
        placement_conventions: None,
        session_profile: None,
        repository: None,
        checkpoint_on_compact: true,
        capture_events: true,
    }
}

#[test]
fn brief_reports_project_and_stays_bounded() {
    let (_dir, store, project_id) = common::indexed_store("hook-brief");
    let profile = Harness::Codex.profile();
    let brief = build_session_brief(&store, project_id, profile, &codex_settings(), true, None);

    assert!(brief.contains("**Project:** oracle"), "{brief}");
    assert!(brief.contains("`````text"), "{brief}");
    assert!(
        brief.contains("_No active tasks, open decisions, or recent sessions in this project._"),
        "{brief}"
    );
    assert!(brief.contains("## Where to write"), "{brief}");
    assert!(brief.ends_with(profile.default_recall_prompt), "{brief}");
    assert!(brief.chars().count() <= auto_memory::hooks::profiles::MAX_BRIEF_CHARS);
}

#[test]
fn checkpoint_prompt_prefixes_the_brief() {
    let (_dir, store, project_id) = common::indexed_store("hook-checkpoint");
    let brief = build_session_brief(
        &store,
        project_id,
        Harness::Codex.profile(),
        &codex_settings(),
        true,
        Some("CHECKPOINT NOW"),
    );
    assert!(brief.starts_with("CHECKPOINT NOW\n\n---\n\n# Auto Memory — session context"));
}

#[test]
fn coding_profile_without_repository_is_an_error_brief() {
    let (_dir, store, project_id) = common::indexed_store("hook-coding");
    let mut settings = codex_settings();
    settings.session_profile = Some("coding".to_owned());
    let brief = build_session_brief(
        &store,
        project_id,
        Harness::Codex.profile(),
        &settings,
        true,
        None,
    );
    assert!(
        brief.contains("`basicMemory.repository` is missing"),
        "{brief}"
    );
}

/// Build a file-backed index over the fixture vault and return (scratch, index path).
fn file_index(tag: &str) -> (common::Scratch, PathBuf) {
    let scratch = common::Scratch::new(tag);
    let vault = scratch.join("vault");
    common::copy_dir(&common::fixtures_vault(), &vault);
    let index = scratch.join("memory.db");
    let mut store = Store::open(&index).expect("open index");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .expect("project");
    rebuild_vault(
        &mut store,
        project_id,
        &vault,
        &RebuildOptions::new("oracle"),
    )
    .expect("rebuild");
    (scratch, index)
}

/// Run `auto-memory hook …` with `HOME` pointed at an empty dir so the developer's
/// real `~/.codex/basic-memory.json` cannot leak into the test.
fn run_hook(args: &[&str], stdin: &str, home: &Path) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(args)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn auto-memory");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn cli_is_fail_open_when_the_index_is_missing() {
    let home = tempfile::tempdir().expect("home");
    let (code, stdout, _stderr) = run_hook(
        &[
            "hook",
            "session-start",
            "--harness",
            "codex",
            "--index",
            "/nonexistent/memory.db",
            "--project",
            "oracle",
        ],
        r#"{"hook_event_name":"SessionStart","cwd":"/tmp"}"#,
        home.path(),
    );
    assert_eq!(code, 0, "a hook must never fail its caller");
    assert!(
        stdout.is_empty(),
        "stdout must stay clean on failure: {stdout}"
    );
}

#[test]
fn cli_session_start_prints_a_brief_for_the_project() {
    let (scratch, index) = file_index("hook-cli");
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let stdin = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}","session_id":"s1","source":"startup"}}"#,
        scratch.path().display()
    );
    let (code, stdout, _stderr) = run_hook(
        &[
            "hook",
            "session-start",
            "--harness",
            "codex",
            "--index",
            index.to_str().expect("utf8"),
            "--project",
            "oracle",
        ],
        &stdin,
        &home,
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("**Project:** oracle"), "{stdout}");
    assert!(stdout.contains("Auto Memory — session context"), "{stdout}");
}

#[test]
fn cli_pre_compact_prints_nothing_for_codex() {
    let home = tempfile::tempdir().expect("home");
    let (code, stdout, _stderr) = run_hook(
        &["hook", "pre-compact", "--harness", "codex"],
        r#"{"hook_event_name":"PreCompact","cwd":"/tmp","trigger":"auto"}"#,
        home.path(),
    );
    assert_eq!(code, 0);
    assert!(
        stdout.is_empty(),
        "Codex ignores PreCompact stdout: {stdout}"
    );
}
