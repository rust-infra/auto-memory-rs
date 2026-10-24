//! The POSIX-style CLI verbs: `cat`, `grep`, `ls`, `find`, `tail`, `man`.
//!
//! Upstream 0.24.0 (#1398) gives the CLI the same verbs as the MCP tools, over one shared
//! translation layer. The properties worth pinning are the shell conventions: a grep that
//! finds nothing exits non-zero so a pipeline can branch on it, `ls /` lists mounts, and
//! `man` answers every spelling of a page name.

mod common;

use std::process::Command;

use common::{Scratch, copy_dir, fixtures_vault};

/// Run one CLI verb against a fresh copy of the fixture vault.
fn run(tag: &str, args: &[&str]) -> (bool, String, String) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let index = dir.join("memory.db");
    // Register and index first, so every verb has something to read.
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
    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(args)
        .arg("--index")
        .arg(&index)
        .arg("--project")
        .arg("oracle")
        .output()
        .expect("verb");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn cat_prints_a_note() {
    let (ok, stdout, stderr) = run("cli-cat", &["cat", "notes/simple.md"]);
    assert!(ok, "cat failed: {stderr}");
    assert!(stdout.contains("# Simple Note"), "{stdout}");
}

#[test]
fn cat_reads_a_line_range_with_numbers() {
    let (ok, stdout, stderr) = run(
        "cli-cat-lines",
        &[
            "cat",
            "notes/simple.md",
            "--start-line",
            "1",
            "--end-line",
            "2",
        ],
    );
    assert!(ok, "cat failed: {stderr}");
    assert!(stdout.contains("   1 | # Simple Note"), "{stdout}");
}

#[test]
fn cat_fails_on_a_missing_note() {
    let (ok, _stdout, stderr) = run("cli-cat-missing", &["cat", "notes/nope.md"]);
    assert!(!ok, "a missing note must exit non-zero");
    assert!(stderr.contains("no such note"), "{stderr}");
}

#[test]
fn grep_prints_matching_lines() {
    let (ok, stdout, stderr) = run("cli-grep", &["grep", "rust"]);
    assert!(ok, "grep failed: {stderr}");
    assert!(stdout.contains("rust"), "{stdout}");
    // The shell shape: `path:line: text`.
    assert!(stdout.lines().any(|line| line.contains(":")), "{stdout}");
}

#[test]
fn grep_exits_non_zero_when_nothing_matches() {
    // A shell tool's contract: no output, non-zero status, so a pipeline can branch.
    let (ok, stdout, _stderr) = run("cli-grep-miss", &["grep", "zzzz-not-there"]);
    assert!(!ok, "a grep with no match must exit non-zero");
    assert!(stdout.is_empty(), "it must print nothing: {stdout}");
}

#[test]
fn grep_context_widens_the_match() {
    let (ok, stdout, stderr) = run("cli-grep-context", &["grep", "rust", "-C", "2"]);
    assert!(ok, "grep failed: {stderr}");
    // More than the single matching line came back.
    assert!(stdout.lines().count() > 1, "{stdout}");
}

#[test]
fn ls_of_the_root_lists_mounts() {
    let (ok, stdout, stderr) = run("cli-ls-root", &["ls"]);
    assert!(ok, "ls failed: {stderr}");
    assert!(stdout.contains("oracle"), "{stdout}");
}

#[test]
fn ls_lists_a_directory() {
    let (ok, stdout, stderr) = run("cli-ls-dir", &["ls", "notes"]);
    assert!(ok, "ls failed: {stderr}");
    assert!(stdout.lines().any(|line| line == "simple.md"), "{stdout}");
}

#[test]
fn ls_tree_is_recursive() {
    let (ok, stdout, stderr) = run("cli-ls-tree", &["ls", "notes", "--tree"]);
    assert!(ok, "ls --tree failed: {stderr}");
    // The nested note appears with its path relative to the listed directory.
    assert!(stdout.contains("nested/deep-note.md"), "{stdout}");
}

#[test]
fn find_matches_a_predicate() {
    let (ok, stdout, stderr) = run("cli-find", &["find", "status=active"]);
    assert!(ok, "find failed: {stderr}");
    assert!(stdout.contains("frontmatter.md"), "{stdout}");
}

#[test]
fn find_projects_fields() {
    let (ok, stdout, stderr) = run(
        "cli-find-fields",
        &["find", "status=active", "--fields", "status"],
    );
    assert!(ok, "find failed: {stderr}");
    assert!(stdout.contains("status=active"), "{stdout}");
}

#[test]
fn find_exits_non_zero_when_nothing_matches() {
    let (ok, _stdout, _stderr) = run("cli-find-miss", &["find", "status=nonexistent"]);
    assert!(!ok, "a find with no match must exit non-zero");
}

#[test]
fn find_refuses_a_malformed_predicate() {
    let (ok, _stdout, stderr) = run("cli-find-bad", &["find", "status"]);
    assert!(!ok);
    assert!(stderr.contains("must be key<op>value"), "{stderr}");
}

#[test]
fn tail_prints_headers() {
    let (ok, stdout, stderr) = run("cli-tail", &["tail", "--count", "2"]);
    assert!(ok, "tail failed: {stderr}");
    assert!(stdout.contains("==>"), "{stdout}");
}

#[test]
fn man_lists_pages() {
    let (ok, stdout, stderr) = run("cli-man-list", &["man", "--list"]);
    assert!(ok, "man failed: {stderr}");
    assert!(stdout.contains("search-notes"), "{stdout}");
    assert!(stdout.contains("cat"), "{stdout}");
}

#[test]
fn man_prints_a_page() {
    let (ok, stdout, stderr) = run("cli-man-page", &["man", "search-notes"]);
    assert!(ok, "man failed: {stderr}");
    assert!(stdout.contains("# search-notes(3)"), "{stdout}");
    assert!(stdout.contains("## SYNOPSIS"), "{stdout}");
}

#[test]
fn man_accepts_a_section_qualified_topic() {
    let (ok, stdout, stderr) = run("cli-man-section", &["man", "3/search-notes"]);
    assert!(ok, "man failed: {stderr}");
    assert!(stdout.contains("# search-notes(3)"), "{stdout}");
}

#[test]
fn man_fails_on_an_unknown_topic() {
    let (ok, _stdout, stderr) = run("cli-man-missing", &["man", "no-such-tool"]);
    assert!(!ok);
    assert!(stderr.contains("no manual entry"), "{stderr}");
}
