//! Document ingestion: `import document`.
//!
//! Upstream 0.24.0 (#1006, #1512) turns a non-Markdown file into a searchable sidecar note
//! plus a provenance run note. Three properties the reference promises and these tests
//! pin:
//!
//! - the extracted text is **searchable** but **cannot mint observations or relations**,
//!   because the sidecar marks itself `bm_parse_semantics: false`;
//! - re-running is a **no-op** while the source is unchanged;
//! - a hand-written note at the sidecar path is **never overwritten**.
//!
//! A format the build cannot extract (PDF, Office) records that plainly instead of writing
//! an empty sidecar — "this document has no text" is a lie, and a model reading it would
//! conclude the source was empty.

mod common;

use std::fs;
use std::process::Command;

use common::Scratch;

/// Register a project over a fresh vault and return `(dir, vault, index)`.
fn project(tag: &str) -> (Scratch, std::path::PathBuf, std::path::PathBuf) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
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

/// Run `import document <path>`.
fn import(
    vault: &std::path::Path,
    index: &std::path::Path,
    args: &[&str],
) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["import", "document"])
        .args(args)
        .arg("--index")
        .arg(index)
        .arg("--project")
        .arg("oracle")
        .current_dir(vault)
        .output()
        .expect("import document");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn a_csv_becomes_a_searchable_sidecar_and_a_run_note() {
    let (_dir, vault, index) = project("ingest-csv");
    fs::write(vault.join("people.csv"), "name,age\nada,36\ngrace,45\n").expect("csv");
    let (ok, stdout, stderr) = import(&vault, &index, &["people.csv"]);
    assert!(ok, "import failed: {stderr}");
    assert!(stdout.contains("people.csv.md"), "{stdout}");

    let sidecar = fs::read_to_string(vault.join("people.csv.md")).expect("sidecar");
    // The extraction is a Markdown table, so the content is readable as well as searchable.
    assert!(sidecar.contains("| name | age |"), "{sidecar}");
    assert!(sidecar.contains("| ada | 36 |"), "{sidecar}");
    // The marker that stops the body minting knowledge.
    assert!(sidecar.contains("bm_parse_semantics: false"), "{sidecar}");
    assert!(sidecar.contains("type: extracted_text"), "{sidecar}");
    assert!(sidecar.contains("source: people.csv"), "{sidecar}");

    // The run note records provenance under the folder upstream uses.
    let runs: Vec<_> = fs::read_dir(vault.join("document-ingestion-runs"))
        .expect("runs dir")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(runs.len(), 1, "one run note per import");
    let run = fs::read_to_string(runs[0].path()).expect("run note");
    assert!(run.contains("type: document_ingestion_run"), "{run}");
    assert!(run.contains("status: extracted"), "{run}");
}

#[test]
fn the_sidecar_is_searchable() {
    let (_dir, vault, index) = project("ingest-search");
    fs::write(vault.join("people.csv"), "name,age\nada,36\n").expect("csv");
    let (ok, _stdout, stderr) = import(&vault, &index, &["people.csv"]);
    assert!(ok, "{stderr}");

    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["search", "ada"])
        .arg("--index")
        .arg(&index)
        .arg("--project")
        .arg("oracle")
        .output()
        .expect("search");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("people.csv"),
        "the extracted text must be searchable: {stdout}"
    );
}

#[test]
fn extracted_text_cannot_mint_observations_or_relations() {
    // The whole point of `bm_parse_semantics: false`: a document's contents are text, not
    // authored knowledge. A CSV cell that looks like an observation must not become one.
    let (_dir, vault, index) = project("ingest-no-mint");
    fs::write(
        vault.join("data.csv"),
        "kind,note\n\"- [fact] this looks like an observation\",x\n\"- relates_to [[somewhere]]\",y\n",
    )
    .expect("csv");
    let (ok, _stdout, stderr) = import(&vault, &index, &["data.csv"]);
    assert!(ok, "{stderr}");

    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["search", "observation", "--index"])
        .arg(&index)
        .args(["--project", "oracle", "--entity-type", "observation"])
        .output()
        .expect("search");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The search returns no observation row for the sidecar; the text is only a body.
    assert!(
        !stdout.contains("\"type\": \"observation\""),
        "extracted text must not mint observations: {stdout}"
    );
}

#[test]
fn re_running_an_unchanged_source_is_a_no_op() {
    let (_dir, vault, index) = project("ingest-idempotent");
    fs::write(vault.join("people.csv"), "name,age\nada,36\n").expect("csv");
    let (ok, _stdout, stderr) = import(&vault, &index, &["people.csv"]);
    assert!(ok, "{stderr}");
    let before = fs::read_to_string(vault.join("people.csv.md")).expect("sidecar");

    let (ok, stdout, stderr) = import(&vault, &index, &["people.csv"]);
    assert!(ok, "a re-run must not fail: {stderr}");
    assert!(stdout.contains("unchanged"), "{stdout}");
    let after = fs::read_to_string(vault.join("people.csv.md")).expect("sidecar");
    assert_eq!(
        before, after,
        "an unchanged source must not rewrite the sidecar"
    );
    // And it does not create a second run note either.
    let runs = fs::read_dir(vault.join("document-ingestion-runs"))
        .expect("runs dir")
        .count();
    assert_eq!(runs, 1, "a no-op must not add a run note");
}

#[test]
fn a_changed_source_rewrites_the_sidecar() {
    let (_dir, vault, index) = project("ingest-changed");
    fs::write(vault.join("people.csv"), "name,age\nada,36\n").expect("csv");
    let (ok, _stdout, stderr) = import(&vault, &index, &["people.csv"]);
    assert!(ok, "{stderr}");

    fs::write(vault.join("people.csv"), "name,age\nada,37\n").expect("csv");
    let (ok, _stdout, stderr) = import(&vault, &index, &["people.csv", "--replace"]);
    assert!(ok, "a changed source needs --replace to rewrite: {stderr}");
    let sidecar = fs::read_to_string(vault.join("people.csv.md")).expect("sidecar");
    assert!(sidecar.contains("| ada | 37 |"), "{sidecar}");
}

#[test]
fn a_hand_written_note_at_the_sidecar_path_is_never_overwritten() {
    let (_dir, vault, index) = project("ingest-protect");
    fs::write(vault.join("people.csv"), "name,age\nada,36\n").expect("csv");
    // Someone wrote their own note at the sidecar path before importing.
    fs::write(
        vault.join("people.csv.md"),
        "---\ntitle: My own notes about people\ntype: note\n---\n\nHand written.\n",
    )
    .expect("hand note");
    let (ok, _stdout, stderr) = import(&vault, &index, &["people.csv"]);
    assert!(!ok, "a hand-written note must block the import");
    assert!(stderr.contains("--replace"), "{stderr}");
    let note = fs::read_to_string(vault.join("people.csv.md")).expect("note");
    assert!(note.contains("Hand written"), "{note}");
}

#[test]
fn an_unextractable_format_records_why_instead_of_an_empty_sidecar() {
    let (_dir, vault, index) = project("ingest-pdf");
    // A minimal file with a .pdf extension: the point is the format's handling, not the
    // PDF's validity, since this build cannot parse one either way.
    fs::write(vault.join("report.pdf"), b"%PDF-1.4\n").expect("pdf");
    let (ok, _stdout, stderr) = import(&vault, &index, &["report.pdf"]);
    assert!(
        ok,
        "an unsupported extractor is not an import failure: {stderr}"
    );
    let sidecar = fs::read_to_string(vault.join("report.pdf.md")).expect("sidecar");
    assert!(sidecar.contains("Not extracted"), "{sidecar}");
    assert!(
        sidecar.contains("pdf-inspector"),
        "the sidecar must name the missing extractor: {sidecar}"
    );
    assert!(
        sidecar.contains("its text is not in this"),
        "an empty body would read as 'no text', which is a lie: {sidecar}"
    );
}

#[test]
fn an_unsupported_extension_is_refused() {
    let (_dir, vault, index) = project("ingest-unsupported");
    fs::write(vault.join("image.png"), b"\x89PNG").expect("png");
    let (ok, _stdout, stderr) = import(&vault, &index, &["image.png"]);
    assert!(!ok);
    assert!(stderr.contains("unsupported document type"), "{stderr}");
}

#[test]
fn a_missing_file_is_refused_with_the_resolved_path() {
    // Upstream 0.24.0 (#1704): a relative path resolves against the project root, and the
    // error names that resolution rather than the caller's cwd.
    let (_dir, vault, index) = project("ingest-missing");
    let (ok, _stdout, stderr) = import(&vault, &index, &["nope.csv"]);
    assert!(!ok);
    assert!(stderr.contains("file not found"), "{stderr}");
    assert!(stderr.contains("project root"), "{stderr}");
}

#[test]
fn a_text_file_is_copied_through() {
    let (_dir, vault, index) = project("ingest-text");
    fs::write(vault.join("notes.txt"), "plain text content\nsecond line\n").expect("txt");
    let (ok, _stdout, stderr) = import(&vault, &index, &["notes.txt"]);
    assert!(ok, "{stderr}");
    let sidecar = fs::read_to_string(vault.join("notes.txt.md")).expect("sidecar");
    assert!(sidecar.contains("plain text content"), "{sidecar}");
    assert!(sidecar.contains("second line"), "{sidecar}");
}

#[test]
fn a_dry_run_writes_nothing() {
    let (_dir, vault, index) = project("ingest-dry-run");
    fs::write(vault.join("people.csv"), "name,age\nada,36\n").expect("csv");
    let (ok, stdout, stderr) = import(&vault, &index, &["people.csv", "--dry-run"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("would write"), "{stdout}");
    assert!(
        !vault.join("people.csv.md").exists(),
        "a dry run writes nothing"
    );
    assert!(
        !vault.join("document-ingestion-runs").exists(),
        "a dry run writes no run note"
    );
}

#[test]
fn a_csv_with_a_quoted_comma_keeps_the_field_intact() {
    let (_dir, vault, index) = project("ingest-quoted");
    fs::write(
        vault.join("data.csv"),
        "name,note\nada,\"likes rust, sqlite\"\n",
    )
    .expect("csv");
    let (ok, _stdout, stderr) = import(&vault, &index, &["data.csv"]);
    assert!(ok, "{stderr}");
    let sidecar = fs::read_to_string(vault.join("data.csv.md")).expect("sidecar");
    assert!(
        sidecar.contains("likes rust, sqlite"),
        "a quoted comma must not split the field: {sidecar}"
    );
}
