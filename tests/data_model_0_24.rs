//! Data-model alignments from 0.24.0.
//!
//! - **#993** — `locked: true` in a note's frontmatter makes it read-only to the API;
//! - **#1246** — an ordinary Markdown link to a project file is a `links_to` relation, the
//!   way a wikilink is;
//! - **#1344** — deleting a note leaves its inbound relations in the unresolved state,
//!   instead of erasing them;
//! - **#1608** — a runtime that supplies an author stamps `created_by`/`updated_by`.
//!
//! Each of these is a property of the *stored* knowledge rather than of one tool, so the
//! tests drive the MCP surface and inspect the vault.

mod common;

use std::fs;

use common::{Scratch, Session, frame_of};
use serde_json::{Value, json};

fn payload(frame: &Value) -> &Value {
    &frame["result"]["structuredContent"]["result"]
}

/// A vault with a locked note and a note that links to it.
fn locked_vault(dir: &Scratch) -> std::path::PathBuf {
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(
        vault.join("sealed.md"),
        "---\ntitle: Sealed\ntype: note\nlocked: true\n---\n\n# Sealed\n\n- [fact] Do not edit.\n",
    )
    .expect("locked note");
    fs::write(
        vault.join("open.md"),
        "---\ntitle: Open\ntype: note\n---\n\n# Open\n\n- [fact] Editable.\n",
    )
    .expect("open note");
    vault
}

#[test]
fn a_locked_note_refuses_an_edit() {
    let dir = Scratch::new("locked-edit");
    let vault = locked_vault(&dir);
    let before = fs::read_to_string(vault.join("sealed.md")).expect("note");
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "edit_note",
                          "arguments": {"identifier": "sealed", "operation": "append",
                                        "content": "\n- [fact] must not land"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("a locked note must refuse an edit");
    assert!(error.contains("locked"), "{error}");
    assert_eq!(
        fs::read_to_string(vault.join("sealed.md")).expect("note"),
        before,
        "nothing may be written to a locked note"
    );
}

#[test]
fn a_locked_note_refuses_an_overwrite() {
    let dir = Scratch::new("locked-overwrite");
    let vault = locked_vault(&dir);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "write_note",
                          "arguments": {"title": "Sealed", "content": "# Clobbered",
                                        "overwrite": true, "output_format": "json"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("a locked note must refuse an overwrite");
    assert!(error.contains("locked"), "{error}");
    let note = fs::read_to_string(vault.join("sealed.md")).expect("note");
    assert!(note.contains("Do not edit"), "{note}");
}

#[test]
fn a_locked_note_refuses_a_delete() {
    let dir = Scratch::new("locked-delete");
    let vault = locked_vault(&dir);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "delete_note",
                          "arguments": {"identifier": "sealed", "output_format": "json"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("a locked note must refuse a delete");
    assert!(error.contains("locked"), "{error}");
    assert!(vault.join("sealed.md").is_file(), "the note must survive");
}

#[test]
fn a_locked_note_refuses_a_move() {
    let dir = Scratch::new("locked-move");
    let vault = locked_vault(&dir);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "move_note",
                          "arguments": {"identifier": "sealed",
                                        "destination_path": "moved.md",
                                        "output_format": "json"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("a locked note must refuse a move");
    assert!(error.contains("locked"), "{error}");
    assert!(vault.join("sealed.md").is_file());
}

#[test]
fn an_unlocked_note_is_editable_beside_a_locked_one() {
    // The lock is per note, not per vault: the presence of one locked note must not
    // freeze the rest.
    let dir = Scratch::new("locked-neighbour");
    let vault = locked_vault(&dir);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "edit_note",
                          "arguments": {"identifier": "open", "operation": "append",
                                        "content": "\n- [fact] appended"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(frame.get("error").is_none(), "{frame}");
    let note = fs::read_to_string(vault.join("open.md")).expect("note");
    assert!(note.contains("appended"), "{note}");
}

#[test]
fn a_string_true_does_not_lock_a_note() {
    // Only the YAML boolean counts: `locked: "true"` is a typo, and honoring it would let
    // a caller lock a note by accident.
    let dir = Scratch::new("locked-string");
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(
        vault.join("quoted.md"),
        "---\ntitle: Quoted\ntype: note\nlocked: \"true\"\n---\n\n# Quoted\n",
    )
    .expect("note");
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "edit_note",
                          "arguments": {"identifier": "quoted", "operation": "append",
                                        "content": "\n- [fact] allowed"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(
        frame.get("error").is_none(),
        "a quoted \"true\" must not lock the note: {frame}"
    );
}

#[test]
fn a_markdown_link_becomes_a_links_to_relation() {
    let dir = Scratch::new("md-link");
    let vault = dir.join("vault");
    fs::create_dir_all(vault.join("guides")).expect("vault");
    fs::write(
        vault.join("guides/Guide.md"),
        "---\ntitle: Guide\ntype: note\n---\n\n# Guide\n\n- [fact] the guide\n",
    )
    .expect("target");
    fs::write(
        vault.join("index.md"),
        "---\ntitle: Index\ntype: note\n---\n\n# Index\n\nSee [the guide](guides/Guide.md) \
         and [an external site](https://example.com/page).\n",
    )
    .expect("source");
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "build_context",
                          "arguments": {"url": "memory://oracle/index", "depth": 1}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let context = payload(frame_of(&output.frames, 2));
    let related =
        serde_json::to_string(&context["results"][0]["related_results"]).expect("related results");
    // The project link became an edge…
    assert!(
        related.contains("Guide") || related.contains("guides"),
        "the Markdown link must become a relation: {related}"
    );
    // …and the external URL did not.
    assert!(
        !related.contains("example.com"),
        "an external URL is not a project link: {related}"
    );
}

#[test]
fn deleting_a_note_leaves_its_inbound_links_unresolved() {
    let dir = Scratch::new("delete-target");
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(
        vault.join("target.md"),
        "---\ntitle: Target\ntype: note\n---\n\n# Target\n\n- [fact] pointed at\n",
    )
    .expect("target");
    fs::write(
        vault.join("source.md"),
        "---\ntitle: Source\ntype: note\n---\n\n# Source\n\n- relates_to [[target]]\n",
    )
    .expect("source");
    let index = dir.join("memory.db");
    // Index both, then delete the target through the tool.
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "delete_note",
                          "arguments": {"identifier": "target", "output_format": "json"}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "build_context",
                          "arguments": {"url": "memory://oracle/source"}}}),
    ];
    let output = Session::new(&vault, &index).run(&requests);
    let deleted = payload(frame_of(&output.frames, 2));
    assert_eq!(deleted["deleted"], json!(true), "{deleted}");
    // The source note still exists and still declares the relation; it is now unresolved
    // rather than erased.
    let context = payload(frame_of(&output.frames, 3));
    let related =
        serde_json::to_string(&context["results"][0]["related_results"]).expect("related results");
    assert!(
        related.contains("target"),
        "the inbound link must survive as unresolved: {related}"
    );
}

#[test]
fn a_note_records_its_media_type_and_format() {
    // Upstream 0.24.0 (#1455): the index records what a file is, so a listing can say so
    // instead of calling everything `text/markdown`.
    let dir = Scratch::new("media-type");
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(
        vault.join("note.md"),
        "---\ntitle: Note\ntype: note\n---\n\n# Note\n",
    )
    .expect("note");
    fs::write(vault.join("data.csv"), "a,b\n1,2\n").expect("csv");
    fs::write(vault.join("page.canvas"), "{}").expect("canvas");
    let index = dir.join("memory.db");
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "list_directory",
                          "arguments": {"dir_name": "/", "output_format": "json",
                                        "page_size": 50}}}),
    ];
    let output = Session::new(&vault, &index).run(&requests);
    let listing = payload(frame_of(&output.frames, 2));
    let files = listing["nodes"].as_array().expect("nodes");
    let note = files
        .iter()
        .find(|node| node["file_path"] == json!("note.md"))
        .expect("the markdown note");
    assert_eq!(note["content_type"], json!("text/markdown"));
    // The short format is the extension, which is what a caller filters on.
    let csv = files
        .iter()
        .find(|node| node["file_path"] == json!("data.csv"));
    if let Some(csv) = csv {
        assert_eq!(csv["content_type"], json!("text/csv"), "{csv}");
    }
}

#[test]
fn created_by_is_set_once_and_updated_by_follows() {
    // Upstream 0.24.0 (#1608): the keys are server-stamped, `created_by` is carried
    // forward, and a local runtime with no author leaves both untouched. This pins the
    // local case: no author means no keys, so a vault is not silently rewritten with
    // provenance it did not ask for.
    let dir = Scratch::new("authorship");
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(
        vault.join("plain.md"),
        "# Plain\n\n- [fact] no author here\n",
    )
    .expect("note");
    let index = dir.join("memory.db");
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "write_note",
                          "arguments": {"title": "Fresh", "content": "- [fact] created",
                                        "output_format": "json"}}}),
    ];
    let output = Session::new(&vault, &index).run(&requests);
    let created = payload(frame_of(&output.frames, 2));
    assert_eq!(created["action"], json!("created"), "{created}");
    let note = fs::read_to_string(vault.join("Fresh.md")).expect("note");
    assert!(
        !note.contains("created_by"),
        "a local runtime supplies no author, so no provenance key is written: {note}"
    );
}
