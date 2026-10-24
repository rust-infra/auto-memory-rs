//! The CLI verbs 0.24.0 added or reshaped: `prune`, `project index`, `status`.
//!
//! - **#1254** — `prune` removes index entries the current ignore rules now exclude, and
//!   indexing does *not* do it on its own (an upgrade that changed the defaults would
//!   otherwise drop notes);
//! - **#1414** — `project index NAME` reindexes one project, and `status` reports
//!   readiness per stage with a phase that tells a never-indexed project from an idle one.

mod common;

use std::fs;
use std::process::Command;

use common::Scratch;

/// A project over a fresh vault, with the given notes written before indexing.
fn project(tag: &str, notes: &[(&str, &str)]) -> (Scratch, std::path::PathBuf, std::path::PathBuf) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    for (name, body) in notes {
        let path = vault.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, body).expect("note");
    }
    let index = dir.join("memory.db");
    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["project", "add", "oracle"])
        .arg(&vault)
        .arg("--index")
        .arg(&index)
        .output()
        .expect("project add");
    assert!(
        output.status.success(),
        "project add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (dir, vault, index)
}

fn run(index: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(args)
        .arg("--index")
        .arg(index)
        .output()
        .expect("verb");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn status_reports_a_phase_and_its_stages() {
    let (_dir, _vault, index) = project(
        "status-phase",
        &[(
            "a.md",
            "---\ntitle: A\ntype: note\n---\n\n# A\n\n- [fact] one\n",
        )],
    );
    let (ok, stdout, stderr) = run(&index, &["status", "--project", "oracle"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("phase:"), "{stdout}");
    assert!(stdout.contains("idle"), "{stdout}");
    assert!(stdout.contains("files:"), "{stdout}");

    let (ok, stdout, stderr) = run(&index, &["status", "--project", "oracle", "--json"]);
    assert!(ok, "{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(report["phase"], serde_json::json!("idle"), "{report}");
    // The stages are named, so a caller can see which one is behind.
    for stage in ["files", "relations", "embeddings"] {
        assert!(report["stages"][stage].is_object(), "{stage}: {report}");
    }
    assert_eq!(report["stages"]["files"]["on_disk"], serde_json::json!(1));
    assert_eq!(report["stages"]["files"]["indexed"], serde_json::json!(1));
}

#[test]
fn status_reports_never_indexed_for_a_project_with_files() {
    // A project with files but no indexed notes is not idle — it has never been indexed,
    // which is the state a caller has to fix.
    let dir = Scratch::new("status-never");
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(
        vault.join("a.md"),
        "---\ntitle: A\ntype: note\n---\n\n# A\n",
    )
    .expect("note");
    let index = dir.join("memory.db");
    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["project", "add", "oracle"])
        .arg(&vault)
        .arg("--index")
        .arg(&index)
        .args(["--no-index"])
        .output()
        .expect("project add");
    assert!(output.status.success());

    let (ok, stdout, stderr) = run(&index, &["status", "--project", "oracle", "--json"]);
    assert!(ok, "{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(
        report["phase"],
        serde_json::json!("never_indexed"),
        "{report}"
    );
    assert_eq!(report["stages"]["files"]["ready"], serde_json::json!(false));
}

#[test]
fn status_reports_empty_for_a_project_with_no_files() {
    let (_dir, _vault, index) = project("status-empty", &[]);
    let (ok, stdout, stderr) = run(&index, &["status", "--project", "oracle", "--json"]);
    assert!(ok, "{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(report["phase"], serde_json::json!("empty"), "{report}");
}

#[test]
fn project_index_reindexes_one_project() {
    let (_dir, vault, index) = project(
        "project-index",
        &[(
            "a.md",
            "---\ntitle: A\ntype: note\n---\n\n# A\n\n- [fact] one\n",
        )],
    );
    // A note written after the initial index is only visible once the project is
    // reindexed, which is exactly the recovery path this verb provides.
    fs::write(
        vault.join("b.md"),
        "---\ntitle: B\ntype: note\n---\n\n# B\n\n- [fact] two\n",
    )
    .expect("note");
    let (ok, stdout, stderr) = run(&index, &["project", "index", "oracle"]);
    assert!(ok, "project index failed: {stdout}{stderr}");
    assert!(
        stdout.contains("reconciled") || stdout.contains("added"),
        "{stdout}"
    );

    let (ok, stdout, _stderr) = run(&index, &["status", "--project", "oracle", "--json"]);
    assert!(ok);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(report["entities"], serde_json::json!(2), "{report}");
}

#[test]
fn project_index_full_rebuilds_everything() {
    let (_dir, _vault, index) = project(
        "project-index-full",
        &[("a.md", "---\ntitle: A\ntype: note\n---\n\n# A\n")],
    );
    let (ok, stdout, stderr) = run(&index, &["project", "index", "oracle", "--full"]);
    assert!(ok, "full rebuild failed: {stdout}{stderr}");
    assert!(stdout.contains("rebuilt"), "{stdout}");
}

#[test]
fn project_index_refuses_an_unknown_project() {
    let (_dir, _vault, index) = project("project-index-unknown", &[]);
    let (ok, _stdout, stderr) = run(&index, &["project", "index", "nope"]);
    assert!(!ok);
    assert!(stderr.contains("project not found"), "{stderr}");
}

#[test]
fn prune_removes_entries_the_ignore_rules_now_exclude() {
    let (_dir, vault, index) = project(
        "prune",
        &[
            ("keep.md", "---\ntitle: Keep\ntype: note\n---\n\n# Keep\n"),
            ("drop.md", "---\ntitle: Drop\ntype: note\n---\n\n# Drop\n"),
        ],
    );
    // Now ignore one of them. Indexing does not remove it on its own — that is the point
    // of `prune` being an explicit verb.
    fs::write(vault.join(".bmignore"), "drop.md\n").expect("bmignore");
    let (ok, stdout, _stderr) = run(&index, &["status", "--project", "oracle", "--json"]);
    assert!(ok);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(
        report["entities"],
        serde_json::json!(2),
        "an ignore rule alone must not drop a note: {report}"
    );

    let (ok, stdout, stderr) = run(&index, &["prune", "--project", "oracle", "--dry-run"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("drop.md"), "{stdout}");
    // A dry run removes nothing.
    let (ok, stdout, _stderr) = run(&index, &["status", "--project", "oracle", "--json"]);
    assert!(ok);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(report["entities"], serde_json::json!(2));

    let (ok, stdout, stderr) = run(&index, &["prune", "--project", "oracle", "--yes"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("pruned"), "{stdout}");
    let (ok, stdout, _stderr) = run(&index, &["status", "--project", "oracle", "--json"]);
    assert!(ok);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(
        report["entities"],
        serde_json::json!(1),
        "the ignored note is gone from the index: {report}"
    );
}

#[test]
fn prune_reports_nothing_when_the_index_matches_the_vault() {
    let (_dir, _vault, index) = project(
        "prune-clean",
        &[("a.md", "---\ntitle: A\ntype: note\n---\n\n# A\n")],
    );
    let (ok, stdout, stderr) = run(&index, &["prune", "--project", "oracle", "--yes"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("nothing to prune"), "{stdout}");
}

#[test]
fn prune_asks_before_removing_without_yes() {
    // A destructive verb asks first, and a non-interactive run gets a refusal rather than
    // a hang.
    let (_dir, vault, index) = project(
        "prune-confirm",
        &[
            ("keep.md", "---\ntitle: Keep\ntype: note\n---\n\n# Keep\n"),
            ("drop.md", "---\ntitle: Drop\ntype: note\n---\n\n# Drop\n"),
        ],
    );
    fs::write(vault.join(".bmignore"), "drop.md\n").expect("bmignore");
    let (ok, stdout, _stderr) = run(&index, &["prune", "--project", "oracle"]);
    assert!(!ok, "without --yes the verb must refuse");
    assert!(stdout.contains("aborted"), "{stdout}");
    // Nothing was removed.
    let (ok, stdout, _stderr) = run(&index, &["status", "--project", "oracle", "--json"]);
    assert!(ok);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(report["entities"], serde_json::json!(2));
}
