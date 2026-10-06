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

fn tact_settings() -> Settings {
    Settings {
        capture_folder: "tact".to_owned(),
        ..codex_settings()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn brief_reports_project_and_stays_bounded() {
    let (_dir, store, project_id) = common::indexed_store("hook-brief");
    let profile = Harness::Codex.profile();
    let brief =
        build_session_brief(&store, project_id, profile, &codex_settings(), true, None).await;

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

#[tokio::test(flavor = "multi_thread")]
async fn checkpoint_prompt_prefixes_the_brief() {
    let (_dir, store, project_id) = common::indexed_store("hook-checkpoint");
    let brief = build_session_brief(
        &store,
        project_id,
        Harness::Codex.profile(),
        &codex_settings(),
        true,
        Some("CHECKPOINT NOW"),
    )
    .await;
    assert!(brief.starts_with("CHECKPOINT NOW\n\n---\n\n# Auto Memory — session context"));
}

#[tokio::test(flavor = "multi_thread")]
async fn coding_profile_without_repository_is_an_error_brief() {
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
    )
    .await;
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
    common::block_on(async {
        let mut store = Store::open(&index).await.expect("open index");
        let project_id = store
            .upsert_project("oracle", "oracle", &vault.to_string_lossy())
            .await
            .expect("project");
        rebuild_vault(
            &mut store,
            project_id,
            &vault,
            &RebuildOptions::new("oracle"),
        )
        .await
        .expect("rebuild");
    });
    (scratch, index)
}

/// Run `auto-memory hook …` with `HOME` pointed at an empty dir so the developer's
/// real `~/.codex/basic-memory.json` cannot leak into the test.
///
/// `XDG_CONFIG_HOME` is redirected too, so the user config file is resolved inside
/// the scratch home rather than wherever the developer's environment points.
fn run_hook(args: &[&str], stdin: &str, home: &Path) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
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

/// Tact's `additionalContext` is collected the same way Codex's is, so the CLI
/// contract is identical: a brief for the project, on stdout, exit 0.
#[test]
fn cli_session_start_prints_a_brief_for_a_tact_project() {
    let (scratch, index) = file_index("hook-cli-tact");
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).expect("home");
    // The real Tact payload (`build_payload` in `crates/tact/src/plugin/hooks.rs`).
    let stdin = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}","session_id":"s1","source":"startup","turn_id":"0","model":"gpt-5","permission_mode":"default","transcript_path":null}}"#,
        scratch.path().display()
    );
    let (code, stdout, _stderr) = run_hook(
        &[
            "hook",
            "session-start",
            "--harness",
            "tact",
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
    // The Tact profile's phrasing, not Codex's.
    assert!(
        stdout.contains("keep required repo rules in AGENTS.md"),
        "{stdout}"
    );
    assert!(!stdout.contains(".codex/"), "{stdout}");
}

#[test]
fn cli_pre_compact_prints_nothing_for_tact() {
    let home = tempfile::tempdir().expect("home");
    let (code, stdout, _stderr) = run_hook(
        &["hook", "pre-compact", "--harness", "tact"],
        r#"{"hook_event_name":"PreCompact","cwd":"/tmp","trigger":"auto"}"#,
        home.path(),
    );
    assert_eq!(code, 0);
    assert!(
        stdout.is_empty(),
        "Tact keeps only the hook's control, not its stdout: {stdout}"
    );
}

/// A host that asks for a harness this build does not know gets "no brief", not
/// a usage error: the documented contract is that a hook never fails its caller,
/// and `--harness` used to be the one path that exited 2.
#[test]
fn cli_unknown_harness_fails_open() {
    let home = tempfile::tempdir().expect("home");
    let (code, stdout, _stderr) = run_hook(
        &["hook", "session-start", "--harness", "vscode"],
        r#"{"hook_event_name":"SessionStart","cwd":"/tmp"}"#,
        home.path(),
    );
    assert_eq!(code, 0, "a hook must never fail its caller");
    assert!(
        stdout.is_empty(),
        "an unknown harness must not print a brief: {stdout}"
    );
}

/// The first-run nudge names the file Tact actually reads — auto-memory's own
/// `auto-memory.json`, never the upstream `basic-memory.json`.
#[tokio::test(flavor = "multi_thread")]
async fn tact_brief_points_at_the_tact_config_file() {
    let (_dir, store, project_id) = common::indexed_store("hook-tact-nudge");
    let brief = build_session_brief(
        &store,
        project_id,
        Harness::Tact.profile(),
        &tact_settings(),
        false,
        None,
    )
    .await;
    assert!(brief.contains(".tact/auto-memory.json"), "{brief}");
    assert!(!brief.contains("basic-memory.json"), "{brief}");
    assert!(!brief.contains(".codex/"), "{brief}");
}

/// Write the user config file under `home`, the way a user would.
fn write_user_config(home: &Path, json: &str) {
    let dir = home.join(".config").join("auto-memory");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.json"), json).expect("write config");
}

/// With a user config file, a hook needs nothing but the payload: no `--index`, no
/// `--project`, no environment variable (`specs/config-discovery-spec.md` §3).
#[test]
fn cli_hook_reads_the_index_and_project_from_the_user_config() {
    let (scratch, index) = file_index("hook-user-config");
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).expect("home");
    write_user_config(
        &home,
        &format!(
            r#"{{"index": "{}", "default_project": "oracle"}}"#,
            index.display()
        ),
    );

    let stdin = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}","source":"startup"}}"#,
        scratch.path().display()
    );
    let (code, stdout, stderr) = run_hook(
        &["hook", "session-start", "--harness", "tact"],
        &stdin,
        &home,
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("**Project:** oracle"), "{stdout}");
    assert!(stdout.contains("Auto Memory — session context"), "{stdout}");
}

/// An unusable config file must not break a session: the hook says so on stderr and
/// carries on with the rest of the chain (fail-open — the CLI errors instead).
#[test]
fn cli_hook_survives_an_unusable_user_config() {
    let (scratch, index) = file_index("hook-broken-config");
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).expect("home");
    write_user_config(&home, "{ not json");

    let stdin = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}","source":"startup"}}"#,
        scratch.path().display()
    );
    // `--index` still wins over the broken file, so the brief is produced anyway.
    let (code, stdout, stderr) = run_hook(
        &[
            "hook",
            "session-start",
            "--harness",
            "tact",
            "--index",
            index.to_str().expect("utf8"),
            "--project",
            "oracle",
        ],
        &stdin,
        &home,
    );
    assert_eq!(code, 0, "a hook must never fail its caller");
    assert!(stdout.contains("**Project:** oracle"), "{stdout}");
    assert!(
        stderr.contains("unusable config"),
        "the hook must say why it ignored the file: {stderr}"
    );
}

/// The first-run nudge names the projects the index actually has, so the user learns the
/// exact value to put in the mapping file without running another command.
#[test]
fn cli_hook_nudge_lists_the_registered_projects() {
    let (scratch, index) = file_index("hook-nudge");
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).expect("home");

    // No mapping file, no `--project`: nothing resolves, which is the nudge's case.
    let stdin = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}","source":"startup"}}"#,
        scratch.path().display()
    );
    let (code, stdout, stderr) = run_hook(
        &[
            "hook",
            "session-start",
            "--harness",
            "tact",
            "--index",
            index.to_str().expect("utf8"),
        ],
        &stdin,
        &home,
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("not configured"), "{stdout}");
    assert!(
        stdout.contains("Registered projects in this index: oracle"),
        "the nudge must name what could be used: {stdout}"
    );
}

/// With no index to read, the nudge keeps its generic wording — and the hook does not
/// create the index just to look for candidates.
#[test]
fn cli_hook_nudge_stays_generic_without_an_index() {
    let home = tempfile::tempdir().expect("home");
    let index = home.path().join("memory.db");
    let stdin = format!(
        r#"{{"hook_event_name":"SessionStart","cwd":"{}","source":"startup"}}"#,
        home.path().display()
    );
    let (code, stdout, _stderr) = run_hook(
        &[
            "hook",
            "session-start",
            "--harness",
            "tact",
            "--index",
            index.to_str().expect("utf8"),
        ],
        &stdin,
        home.path(),
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("not configured"), "{stdout}");
    assert!(!stdout.contains("Registered projects"), "{stdout}");
    assert!(
        !index.exists(),
        "a nudge must not create the index it looked for"
    );
}
