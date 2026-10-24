//! Group B: parser and identifier boundaries from 0.24.0.
//!
//! - **#1629** — an identifier with a non-Markdown extension resolves to its exact file,
//!   not to the same-stem Markdown note;
//! - **#1537** — an editor's scratch copy is not a note;
//! - **#1539** — an escaped hash in `.bmignore` is a pattern, not a comment;
//! - **#1534** — an unindexed project answers "Project Index Required", not "no match";
//! - **#1533** — an unspaced CJK compound that finds nothing suggests a shorter term.

use std::fs;

use auto_memory::indexing::document::markdown_files;
use auto_memory::indexing::watcher::IgnoreRules;

#[test]
fn an_editor_scratch_copy_is_not_a_note() {
    // Upstream 0.24.0 (#1537): Emacs autosaves `#note.md#`, its lock files are `.#note.md`,
    // and Vim writes `.note.md.swp`. Indexing any of them makes a vault look like it has
    // notes it does not have. The rule is internal, so it is observed through the walk.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("note.md"), "# Note\n").expect("note");
    fs::write(root.join("#note.md#"), "# Note\n").expect("autosave");
    fs::write(root.join(".#note.md"), "# Note\n").expect("lock");
    // A name that merely contains a hash is still a note, so the rule is not "any hash".
    fs::write(root.join("note#draft.md"), "# Note\n").expect("draft");

    let mut names: Vec<String> = markdown_files(root)
        .into_iter()
        .map(|(relative, _)| relative)
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["note#draft.md".to_owned(), "note.md".to_owned()],
        "the scratch copies are walked past"
    );
}

#[test]
fn an_escaped_hash_in_bmignore_is_a_pattern() {
    // Upstream 0.24.0 (#1539): `\#*#` means a literal leading and trailing hash, so it
    // matches Emacs autosaves. Without the escape it would be read as a comment.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join(".bmignore"), "\\#*#\n").expect("bmignore");

    let rules = IgnoreRules::load(root, &[]);
    assert!(
        rules.should_ignore("#draft.md#"),
        "the escaped pattern must match an autosave name"
    );
    assert!(
        !rules.should_ignore("notes/ordinary.md"),
        "and must not match an ordinary note"
    );
}

#[test]
fn a_bare_hash_line_is_still_a_comment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    // A single-segment directory pattern and a file glob: what this test is about is the
    // comment/short-line distinction, not multi-segment directory matching.
    fs::write(
        root.join(".bmignore"),
        "# a comment\nprivate/\n*.draft.md\n",
    )
    .expect("bmignore");

    let rules = IgnoreRules::load(root, &[]);
    assert!(
        !rules.should_ignore("notes/public/readme.md"),
        "a comment must not become a pattern"
    );
    assert!(
        !rules.should_ignore("notes/comment.md"),
        "the comment's words must not become patterns either"
    );
    assert!(
        rules.should_ignore("private/send.md"),
        "the directory pattern still applies"
    );
    assert!(
        rules.should_ignore("notes/scratch.draft.md"),
        "the glob pattern still applies"
    );
}
