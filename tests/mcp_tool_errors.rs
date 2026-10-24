//! Refused mutations are MCP tool errors (`isError: true`).
//!
//! Upstream 0.24.0 changed this in three linked fixes:
//!
//! - **#1683** — a refused `write_note` (an existing note without `overwrite`, a
//!   revision conflict, a note that moved, a directory outside the project);
//! - **#1688** — a refused `edit_note` write;
//! - **#1641** — a failed `delete_note` or `move_note`.
//!
//! All of them used to come back as ordinary results whose *text* said "Edit Failed"
//! or "Move Failed", so a client that checks `isError` counted a refusal as a write and
//! never retried it. The payloads are unchanged; only the flag moves. `delete_note` on
//! a note that does not exist still answers `false` rather than an error, which is the
//! reference's own carve-out and is pinned below.

mod common;

use common::{Scratch, Session, copy_dir, fixtures_vault, frame_of};
use serde_json::{Value, json};

/// A session over a fresh copy of the fixture vault.
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
fn a_write_conflict_is_a_tool_error() {
    let (dir, vault, mut requests) = session("tool-error-write");
    // `notes/simple` already exists, so this is a conflict unless `overwrite` is set.
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "write_note",
                   "arguments": {"title": "simple", "content": "body",
                                 "directory": "notes", "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = &frame_of(&output.frames, 2)["result"];
    assert_eq!(result["isError"], json!(true), "a refusal must set isError");
    // The payload is unchanged: the same `action`/`error` pair as before the fix.
    assert_eq!(
        result["structuredContent"]["result"]["action"],
        json!("conflict")
    );
    assert_eq!(
        result["structuredContent"]["result"]["error"],
        json!("NOTE_ALREADY_EXISTS")
    );
    // The text surface carries the same refusal.
    assert!(
        result["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("NOTE_ALREADY_EXISTS"))
    );
}

#[test]
fn a_write_conflict_in_text_mode_is_a_tool_error() {
    let (dir, vault, mut requests) = session("tool-error-write-text");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "write_note",
                   "arguments": {"title": "simple", "content": "body", "directory": "notes"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = &frame_of(&output.frames, 2)["result"];
    assert_eq!(result["isError"], json!(true));
    let text = result["content"][0]["text"].as_str().expect("text");
    assert!(
        text.contains("already exists") || text.contains("Note Already Exists"),
        "{text}"
    );
}

#[test]
fn a_successful_write_is_not_a_tool_error() {
    let (dir, vault, mut requests) = session("tool-error-write-ok");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "write_note",
                   "arguments": {"title": "Fresh Note", "content": "- [fact] something",
                                 "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = &frame_of(&output.frames, 2)["result"];
    assert_eq!(result["isError"], json!(false));
    assert_eq!(
        result["structuredContent"]["result"]["action"],
        json!("created")
    );
    assert!(vault.join("Fresh Note.md").is_file());
}

#[test]
fn a_failed_move_is_a_tool_error() {
    let (dir, vault, mut requests) = session("tool-error-move");
    // The second move of the same note into the same path is refused (`moved: false`),
    // which is the captured reference failure case in `mcp/note-tools.json` (ids 27/28).
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "move_note",
                   "arguments": {"identifier": "notes/simple",
                                 "destination_path": "archive/simple.md",
                                 "output_format": "json"}}
    }));
    requests.push(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "move_note",
                   "arguments": {"identifier": "notes/simple",
                                 "destination_path": "archive/simple.md",
                                 "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    // The first move succeeds…
    let first = &frame_of(&output.frames, 2)["result"];
    assert_eq!(first["isError"], json!(false));
    assert_eq!(first["structuredContent"]["result"]["moved"], json!(true));
    // …and the repeat is a tool error, not a silent success.
    let second = &frame_of(&output.frames, 3)["result"];
    assert_eq!(second["isError"], json!(true), "{second}");
    assert_eq!(second["structuredContent"]["result"]["moved"], json!(false));
    assert!(vault.join("archive/simple.md").is_file());
}

#[test]
fn a_missing_delete_target_is_false_not_an_error() {
    // The reference's carve-out: `delete_note` answers `false` for a note that is not
    // there, and that is not a tool error.
    let (dir, vault, mut requests) = session("tool-error-delete-missing");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "delete_note",
                   "arguments": {"identifier": "notes/does-not-exist", "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = &frame_of(&output.frames, 2)["result"];
    assert_eq!(result["isError"], json!(false));
    assert_eq!(
        result["structuredContent"]["result"]["deleted"],
        json!(false)
    );
}

#[test]
fn a_successful_delete_is_not_a_tool_error() {
    let (dir, vault, mut requests) = session("tool-error-delete-ok");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "delete_note",
                   "arguments": {"identifier": "notes/simple", "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = &frame_of(&output.frames, 2)["result"];
    assert_eq!(result["isError"], json!(false));
    assert_eq!(
        result["structuredContent"]["result"]["deleted"],
        json!(true)
    );
    assert!(!vault.join("notes/simple.md").exists());
}

#[test]
fn a_directory_escape_is_a_tool_error() {
    let (dir, vault, mut requests) = session("tool-error-escape");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "write_note",
                   "arguments": {"title": "Escape", "content": "x",
                                 "directory": "../outside", "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = &frame_of(&output.frames, 2)["result"];
    assert_eq!(result["isError"], json!(true));
    assert_eq!(
        result["structuredContent"]["result"]["error"],
        json!("SECURITY_VALIDATION_ERROR")
    );
}
