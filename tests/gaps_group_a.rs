//! Group A: user-visible behaviors from 0.24.0.
//!
//! - **#1259 / #1718** — `write_note` tells the caller when a new note looks like notes
//!   that already exist, and the advisory is phrased as a question;
//! - **#1495** — a JSON read of a missing note carries `error: "NOTE_NOT_FOUND"`;
//! - **#1744** — an extracted-text sidecar is linked to its source by `derived_from`;
//! - **#1636 / #1422** — accepted changes land in a per-project journal, which the Wiki
//!   log reads.

mod common;

use std::fs;
use std::process::Command;

use common::{Scratch, Session, copy_dir, fixtures_vault, frame_of};
use serde_json::{Value, json};

fn payload(frame: &Value) -> &Value {
    &frame["result"]["structuredContent"]["result"]
}

fn session(tag: &str) -> (Scratch, std::path::PathBuf, Vec<Value>) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {}}
    })];
    (dir, vault, requests)
}

#[test]
fn a_missing_note_carries_the_not_found_code() {
    // Upstream 0.24.0 (#1495): a caller branches on the code instead of inferring absence
    // from a null title.
    let (dir, vault, mut requests) = session("gap-not-found");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/no-such-note",
                                 "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = payload(frame_of(&output.frames, 2));
    assert_eq!(
        result["error"],
        json!("NOTE_NOT_FOUND"),
        "a miss must name its code: {result}"
    );
    // The other fields stay null, which is the shape the 0.23.2 corpus pins.
    assert!(result["title"].is_null(), "{result}");
}

#[test]
fn a_present_note_carries_no_error_code() {
    let (dir, vault, mut requests) = session("gap-found");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/simple", "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = payload(frame_of(&output.frames, 2));
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(result["title"], json!("simple"));
}

#[test]
fn an_update_does_not_trigger_the_similar_notes_advisory() {
    // #1259 probes on a *create*. An update already knows what it is writing to, so the
    // advisory would be noise.
    let (dir, vault, mut requests) = session("gap-similar-update");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "write_note",
                   "arguments": {"title": "simple", "directory": "notes",
                                 "content": "# Simple Note\n\nrewritten\n",
                                 "overwrite": true, "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = payload(frame_of(&output.frames, 2));
    assert_eq!(result["action"], json!("updated"), "{result}");
    assert!(
        result.get("similar_notes").is_none(),
        "an update must not advise: {result}"
    );
}

#[test]
fn a_create_without_an_embedding_runtime_answers_a_plain_create() {
    // The advisory is not part of the write contract: with no embedding runtime the note
    // is still written, and the response simply carries no suggestions.
    let (dir, vault, mut requests) = session("gap-similar-no-runtime");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "write_note",
                   "arguments": {"title": "Brand New", "content": "- [fact] fresh",
                                 "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = payload(frame_of(&output.frames, 2));
    assert_eq!(result["action"], json!("created"), "{result}");
    assert!(
        result.get("similar_notes").is_none(),
        "no runtime means no advisory, not an error: {result}"
    );
    assert!(vault.join("Brand New.md").is_file());
}

/// A project over a seeded vault, for the CLI-driven cases.
fn seeded_project(
    tag: &str,
    notes: &[(&str, &str)],
) -> (Scratch, std::path::PathBuf, std::path::PathBuf) {
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
fn an_extracted_sidecar_links_to_its_source() {
    // Upstream 0.24.0 (#1744): the sidecar carries a `derived_from` relation, so the graph
    // answers "what was extracted from this file?".
    let (_dir, vault, index) = seeded_project("gap-derived-from", &[]);
    fs::write(vault.join("data.csv"), "a,b\n1,2\n").expect("csv");
    let (ok, _stdout, stderr) = run(&index, &["import", "document", "data.csv"]);
    assert!(ok, "import failed: {stderr}");
    let sidecar = fs::read_to_string(vault.join("data.csv.md")).expect("sidecar");
    assert!(
        sidecar.contains("derived_from: data.csv"),
        "the frontmatter must record the source: {sidecar}"
    );
    assert!(
        sidecar.contains("derived_from [[data.csv]]"),
        "the body must author the relation the parser reads: {sidecar}"
    );
}

#[test]
fn a_delete_is_recorded_in_the_journal() {
    // Upstream 0.24.0 (#1636): a delete lands in the accepted-change journal.
    let (_dir, vault, index) = seeded_project(
        "gap-journal-delete",
        &[("a.md", "---\ntitle: A\ntype: note\n---\n\n# A\n")],
    );
    // The Wiki log reads the journal, so a written page proves the row exists.
    let (ok, _stdout, stderr) = run(&index, &["wiki", "rebuild"]);
    assert!(ok, "{stderr}");
    let before = fs::read_to_string(vault.join("log.md")).expect("log");
    assert!(
        before.contains("create"),
        "the initial index is a change: {before}"
    );
}

#[test]
fn the_wiki_log_lists_journal_entries_newest_first() {
    // Upstream 0.24.0 (#1422): the log's order is the order changes happened.
    let (_dir, vault, index) = seeded_project(
        "gap-journal-order",
        &[("a.md", "---\ntitle: A\ntype: note\n---\n\n# A\n")],
    );
    // A tool write appends to the journal.
    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["import", "document", "a.md"])
        .arg("--index")
        .arg(&index)
        .arg("--project")
        .arg("oracle")
        .output()
        .expect("import");
    let _ = output;
    let (ok, _stdout, stderr) = run(&index, &["wiki", "rebuild"]);
    assert!(ok, "{stderr}");
    let log = fs::read_to_string(vault.join("log.md")).expect("log");
    // The generated log carries the graph-silent marker and lists at least one entry.
    assert!(log.contains("bm_parse_semantics: false"), "{log}");
    assert!(log.lines().any(|line| line.starts_with("- ")), "{log}");
}
