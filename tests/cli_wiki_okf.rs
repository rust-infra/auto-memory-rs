//! Wiki projection and OKF export.
//!
//! Two upstream 0.24.0 features that turn a vault into something another tool can read:
//!
//! - **#1422** — `wiki status|validate|rebuild` generates `index.md`/`log.md` navigation
//!   pages per folder, deterministically, never overwriting a hand-written page;
//! - **#1550** — `okf export|check` writes and validates a portable OKF v0.2 bundle, with
//!   wikilinks rewritten as Markdown links and the original relation types preserved.

mod common;

use std::fs;
use std::process::Command;

use common::Scratch;

/// Run one CLI verb against a project.
///
/// `okf check` reads a bundle and takes no project, so the shared flags are only appended
/// for the verbs that accept them.
fn run(index: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_auto-memory"));
    command.args(args);
    if !matches!(args.first(), Some(&"okf")) || !matches!(args.get(1), Some(&"check")) {
        command.arg("--index").arg(index).arg("--project").arg("oracle");
    }
    let output = command.output().expect("verb");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Register a project over a fresh vault, seeded first so `project add` indexes it.
fn seeded_project(tag: &str) -> (Scratch, std::path::PathBuf, std::path::PathBuf) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    seed(&vault);
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

/// A vault with two notes and one nested note.
fn seed(vault: &std::path::Path) {
    fs::create_dir_all(vault.join("deep")).expect("deep");
    fs::write(
        vault.join("simple.md"),
        "---\ntitle: Simple\ntype: note\ntags: [rust]\n---\n\n# Simple\n\n\
         - [decision] Use SQLite.\n- relates_to [[Other]]\n\nProse linking [[Other]].\n",
    )
    .expect("simple");
    fs::write(
        vault.join("other.md"),
        "---\ntitle: Other\ntype: note\n---\n\n# Other\n\n- [fact] the other note\n",
    )
    .expect("other");
    fs::write(
        vault.join("deep/nested.md"),
        "---\ntitle: Nested\ntype: note\n---\n\n# Nested\n",
    )
    .expect("nested");
}

#[test]
fn wiki_status_reports_what_a_rebuild_would_do() {
    let (_dir, vault, index) = seeded_project("wiki-status");
    let (ok, stdout, stderr) = run(&index, &["wiki", "status"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("create"), "{stdout}");
    assert!(stdout.contains("index.md"), "{stdout}");
    assert!(stdout.contains("log.md"), "{stdout}");
    // Status writes nothing.
    assert!(!vault.join("index.md").exists(), "status must not write");
}

#[test]
fn wiki_rebuild_writes_index_and_log_pages_per_folder() {
    let (_dir, vault, index) = seeded_project("wiki-rebuild");
    let (ok, stdout, stderr) = run(&index, &["wiki", "rebuild"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("wrote"), "{stdout}");
    for path in ["index.md", "log.md", "deep/index.md", "deep/log.md"] {
        assert!(vault.join(path).is_file(), "{path} must be written");
    }
    let index_page = fs::read_to_string(vault.join("index.md")).expect("index");
    // The index links its notes and its child folders.
    assert!(index_page.contains("[[simple.md|Simple]]"), "{index_page}");
    assert!(index_page.contains("deep"), "{index_page}");
    assert!(index_page.contains("generated: wiki"), "{index_page}");
}

#[test]
fn wiki_rebuild_is_idempotent() {
    let (_dir, vault, index) = seeded_project("wiki-idempotent");
    let (ok, _stdout, stderr) = run(&index, &["wiki", "rebuild"]);
    assert!(ok, "{stderr}");
    let before = fs::read_to_string(vault.join("index.md")).expect("index");

    let (ok, stdout, stderr) = run(&index, &["wiki", "status"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.lines().all(|line| line.starts_with("unchanged")),
        "a second run over an unchanged vault must change nothing: {stdout}"
    );
    let after = fs::read_to_string(vault.join("index.md")).expect("index");
    assert_eq!(before, after);
}

#[test]
fn a_hand_written_index_is_never_overwritten() {
    let (_dir, vault, index) = seeded_project("wiki-conflict");
    fs::write(
        vault.join("index.md"),
        "---\ntitle: My index\ntype: note\n---\n\nHand written.\n",
    )
    .expect("hand index");
    let (ok, _stdout, stderr) = run(&index, &["wiki", "rebuild"]);
    // The run succeeds, but the hand-written page is reported and left alone.
    assert!(ok, "{stderr}");
    assert!(stderr.contains("skipped"), "{stderr}");
    let page = fs::read_to_string(vault.join("index.md")).expect("index");
    assert!(page.contains("Hand written"), "{page}");
}

#[test]
fn wiki_validate_exits_non_zero_on_a_conflict() {
    let (_dir, vault, index) = seeded_project("wiki-validate");
    let (ok, stdout, stderr) = run(&index, &["wiki", "validate"]);
    assert!(ok, "a clean projection validates: {stdout}{stderr}");

    fs::write(vault.join("log.md"), "---\ntitle: Mine\n---\n\nhand\n").expect("hand log");
    let (ok, stdout, _stderr) = run(&index, &["wiki", "validate"]);
    assert!(!ok, "a conflict must exit non-zero");
    assert!(stdout.contains("log.md"), "{stdout}");
}

#[test]
fn the_wiki_log_is_graph_silent_and_the_index_is_not() {
    // Upstream #1601: an index's wikilinks are meant to feed the graph; a log's entries
    // are prose and must not.
    let (_dir, vault, index) = seeded_project("wiki-semantics");
    let (ok, _stdout, stderr) = run(&index, &["wiki", "rebuild"]);
    assert!(ok, "{stderr}");
    let log = fs::read_to_string(vault.join("log.md")).expect("log");
    assert!(log.contains("bm_parse_semantics: false"), "{log}");
    let index_page = fs::read_to_string(vault.join("index.md")).expect("index");
    assert!(
        !index_page.contains("bm_parse_semantics"),
        "the index's links must reach the graph: {index_page}"
    );
}

#[test]
fn okf_export_writes_a_bundle_with_indexes_and_a_log() {
    let (dir, _vault, index) = seeded_project("okf-export");
    let destination = dir.join("bundle");
    let (ok, stdout, stderr) = run(&index, &["okf", "export", &destination.to_string_lossy()]);
    assert!(ok, "export failed: {stdout}{stderr}");
    for path in [
        "index.md",
        "log.md",
        "simple.md",
        "other.md",
        "deep/index.md",
    ] {
        assert!(
            destination.join(path).is_file(),
            "{path} must be in the bundle"
        );
    }
}

#[test]
fn an_exported_concept_keeps_its_observations_and_relation_types() {
    let (dir, _vault, index) = seeded_project("okf-concept");
    let destination = dir.join("bundle");
    let (ok, _stdout, stderr) = run(&index, &["okf", "export", &destination.to_string_lossy()]);
    assert!(ok, "{stderr}");
    let concept = fs::read_to_string(destination.join("simple.md")).expect("concept");
    // The authored title, not the filename.
    assert!(concept.contains("title: \"Simple\""), "{concept}");
    // The categorized observation, once.
    assert!(concept.contains("## decision"), "{concept}");
    assert_eq!(
        concept.matches("Use SQLite.").count(),
        1,
        "an observation must not appear twice: {concept}"
    );
    // The relation type the Markdown link cannot carry.
    assert!(concept.contains("bm.okf_export"), "{concept}");
    assert!(concept.contains("type: relates_to"), "{concept}");
}

#[test]
fn a_wikilink_becomes_a_markdown_link_in_the_bundle() {
    let (dir, _vault, index) = seeded_project("okf-links");
    let destination = dir.join("bundle");
    let (ok, _stdout, stderr) = run(&index, &["okf", "export", &destination.to_string_lossy()]);
    assert!(ok, "{stderr}");
    let concept = fs::read_to_string(destination.join("simple.md")).expect("concept");
    // `[[Other]]` resolved to a real bundle path.
    assert!(
        concept.contains("[Other](other.md)"),
        "the wikilink must become a resolving Markdown link: {concept}"
    );
    assert!(
        !concept.contains("[[Other]]"),
        "no wikilink may survive into a portable bundle: {concept}"
    );
}

#[test]
fn okf_check_validates_the_exported_bundle() {
    let (dir, _vault, index) = seeded_project("okf-check");
    let destination = dir.join("bundle");
    let (ok, _stdout, stderr) = run(&index, &["okf", "export", &destination.to_string_lossy()]);
    assert!(ok, "{stderr}");
    let (ok, stdout, stderr) = run(&index, &["okf", "check", &destination.to_string_lossy()]);
    assert!(
        ok,
        "a freshly exported bundle must validate: {stdout}{stderr}"
    );
    assert!(stdout.contains("ok:"), "{stdout}");
}

#[test]
fn okf_check_exits_non_zero_on_a_violation() {
    let (dir, _vault, index) = seeded_project("okf-check-bad");
    let destination = dir.join("bundle");
    let (ok, _stdout, stderr) = run(&index, &["okf", "export", &destination.to_string_lossy()]);
    assert!(ok, "{stderr}");
    // A concept with no title is a violation.
    fs::write(destination.join("broken.md"), "type: note\n\nbody\n").expect("broken");
    let (ok, stdout, _stderr) = run(&index, &["okf", "check", &destination.to_string_lossy()]);
    assert!(!ok, "a violation must exit non-zero");
    assert!(stdout.contains("concept-title"), "{stdout}");
}

#[test]
fn okf_export_refuses_to_overwrite_without_replace() {
    let (dir, _vault, index) = seeded_project("okf-replace");
    let destination = dir.join("bundle");
    let (ok, _stdout, stderr) = run(&index, &["okf", "export", &destination.to_string_lossy()]);
    assert!(ok, "{stderr}");
    let (ok, _stdout, stderr) = run(&index, &["okf", "export", &destination.to_string_lossy()]);
    assert!(!ok, "an existing bundle must not be overwritten silently");
    assert!(stderr.contains("--replace"), "{stderr}");
    // With --replace it succeeds.
    let (ok, _stdout, stderr) = run(
        &index,
        &["okf", "export", &destination.to_string_lossy(), "--replace"],
    );
    assert!(ok, "{stderr}");
}

#[test]
fn a_failed_export_leaves_the_previous_bundle_intact() {
    // The staging rule: a bundle is written beside the destination and published by
    // rename, so a failure never leaves a half-written directory.
    let (dir, _vault, index) = seeded_project("okf-atomic");
    let destination = dir.join("bundle");
    let (ok, _stdout, stderr) = run(&index, &["okf", "export", &destination.to_string_lossy()]);
    assert!(ok, "{stderr}");
    let before = fs::read_to_string(destination.join("simple.md")).expect("concept");

    // A refused re-export (no --replace) must not touch the existing bundle.
    let (ok, _stdout, _stderr) = run(&index, &["okf", "export", &destination.to_string_lossy()]);
    assert!(!ok);
    let after = fs::read_to_string(destination.join("simple.md")).expect("concept");
    assert_eq!(before, after);
    // And no staging directory is left behind.
    assert!(
        !dir.join(".bundle.staging").exists(),
        "no staging directory"
    );
}
