//! Revision-guarded writes: `expected_checksum` on `write_note` and `edit_note`.
//!
//! Upstream 0.24.0 (#1552, #1642) lets a caller write only the revision it read, which
//! is what makes concurrent multi-agent writes safe: a second writer that read the old
//! note gets a conflict instead of silently clobbering the first one's work. The guard
//! is only useful if the read surface hands out the value to send back, so these tests
//! also pin `read_note`'s `checksum`/`updated_at`/`include_content` fields.
//!
//! A refusal must be an MCP tool error (#1683, #1688) and must not have written
//! anything — a refused append is never in the note.

mod common;

use common::{Scratch, Session, copy_dir, fixtures_vault, frame_of};
use serde_json::{Value, json};

/// The JSON payload of one `tools/call` result.
fn payload(frame: &Value) -> &Value {
    &frame["result"]["structuredContent"]["result"]
}

/// `true` when the tool result is an MCP error.
fn is_error(frame: &Value) -> bool {
    frame["result"]["isError"] == json!(true)
}

/// A session over a fresh copy of the fixture vault, plus its request script.
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
fn a_json_read_carries_the_revision_and_timestamp() {
    let (dir, vault, mut requests) = session("revision-read");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/simple", "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = payload(frame_of(&output.frames, 2));
    let checksum = result["checksum"].as_str().expect("checksum");
    // The reference's checksum is a full SHA-256, not a prefix or a placeholder.
    assert_eq!(checksum.len(), 64, "{checksum}");
    assert!(checksum.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!checksum.contains("unknown"));
    assert!(
        result["updated_at"].is_string(),
        "updated_at must be present: {result}"
    );
    // The body is still there by default.
    assert!(result["content"].is_string());
}

#[test]
fn a_json_read_keeps_the_frontmatter_object() {
    let (dir, vault, mut requests) = session("revision-read-frontmatter");
    // `notes/frontmatter.md` is the fixture that actually declares a frontmatter block,
    // so the payload's `frontmatter` object is exercised rather than the empty case.
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/frontmatter", "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let result = payload(frame_of(&output.frames, 2));
    assert!(result["frontmatter"].is_object(), "{result}");
    assert_eq!(result["frontmatter"]["status"], json!("active"));
    // The revision is a property of the whole file, frontmatter included: editing only
    // the metadata still moves it.
    assert_eq!(result["checksum"].as_str().expect("checksum").len(), 64);
}

#[test]
fn include_content_false_drops_the_body_but_keeps_the_revision() {
    let (dir, vault, mut requests) = session("revision-metadata-only");
    // Two reads of the same note: one full, one metadata-only.
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/simple", "output_format": "json"}}
    }));
    requests.push(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/simple", "output_format": "json",
                                 "include_content": false}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let full = payload(frame_of(&output.frames, 2));
    let meta = payload(frame_of(&output.frames, 3));

    assert!(meta["content"].is_null(), "no body: {meta}");
    assert!(meta["frontmatter"].is_null(), "no frontmatter: {meta}");
    // The cheap read still answers what a guarded write needs, and agrees with the
    // full read so a caller can swap one for the other.
    assert_eq!(meta["checksum"], full["checksum"]);
    assert_eq!(meta["title"], full["title"]);
    assert_eq!(meta["permalink"], full["permalink"]);
    assert_eq!(meta["file_path"], full["file_path"]);
    assert_eq!(meta["updated_at"], full["updated_at"]);
}

#[test]
fn a_guarded_write_with_the_current_checksum_succeeds() {
    let (dir, vault, mut requests) = session("revision-write-ok");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/simple", "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let checksum = payload(frame_of(&output.frames, 2))["checksum"]
        .as_str()
        .expect("checksum")
        .to_owned();

    // Re-run with the checksum from the first session: a second process editing the
    // file between the read and the write is exactly the race the guard protects.
    let mut requests = vec![json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {}}
    })];
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "write_note",
                   "arguments": {"title": "simple", "directory": "notes",
                                 "content": "# Simple Note\n\nrewritten\n",
                                 "overwrite": true,
                                 "expected_checksum": checksum,
                                 "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(!is_error(frame), "{frame}");
    let result = payload(frame);
    assert_eq!(result["action"], json!("updated"));
    // The write reports the new revision, so a caller can chain another guarded write.
    let new_checksum = result["checksum"].as_str().expect("checksum");
    assert_eq!(new_checksum.len(), 64);
    assert_ne!(new_checksum, checksum, "the revision must have changed");
    assert!(vault.join("notes/simple.md").is_file());
}

#[test]
fn a_stale_checksum_writes_nothing_and_reports_the_current_revision() {
    let (dir, vault, mut requests) = session("revision-write-stale");
    let before = std::fs::read_to_string(vault.join("notes/simple.md")).expect("fixture note");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "write_note",
                   "arguments": {"title": "simple", "directory": "notes",
                                 "content": "# Clobbered\n",
                                 "overwrite": true,
                                 "expected_checksum": "0000000000000000000000000000000000000000000000000000000000000000",
                                 "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(is_error(frame), "a stale revision is a tool error: {frame}");
    let result = payload(frame);
    assert_eq!(result["error"], json!("NOTE_REVISION_CONFLICT"));
    assert_eq!(result["action"], json!("conflict"));
    // `checksum` is null because nothing was written; `currentChecksum` names the
    // revision that actually exists so the caller can re-read and retry.
    assert!(result["checksum"].is_null());
    let current = result["currentChecksum"].as_str().expect("currentChecksum");
    assert_eq!(current.len(), 64);
    // Nothing changed on disk.
    assert_eq!(
        std::fs::read_to_string(vault.join("notes/simple.md")).expect("note"),
        before
    );
}

#[test]
fn a_guarded_write_does_not_recreate_a_deleted_note() {
    let (dir, vault, mut requests) = session("revision-write-deleted");
    // A checksum for a path no note owns is a conflict too, so a note deleted since it
    // was read is not brought back by a stale writer.
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "write_note",
                   "arguments": {"title": "Gone Note", "content": "resurrected",
                                 "overwrite": true,
                                 "expected_checksum": "1111111111111111111111111111111111111111111111111111111111111111",
                                 "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(is_error(frame), "{frame}");
    assert_eq!(payload(frame)["error"], json!("NOTE_REVISION_CONFLICT"));
    assert!(
        payload(frame)["currentChecksum"].is_null(),
        "nothing owns the path, so there is no current revision"
    );
    assert!(
        !vault.join("Gone Note.md").exists(),
        "must not be recreated"
    );
}

#[test]
fn a_guarded_edit_with_the_current_checksum_appends() {
    let (dir, vault, mut requests) = session("revision-edit-ok");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/simple", "output_format": "json",
                                 "include_content": false}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let checksum = payload(frame_of(&output.frames, 2))["checksum"]
        .as_str()
        .expect("checksum")
        .to_owned();

    let mut requests = vec![json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2024-11-05", "capabilities": {}}
    })];
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "edit_note",
                   "arguments": {"identifier": "notes/simple", "operation": "append",
                                 "content": "\n\n- [fact] appended by a guarded edit",
                                 "expected_checksum": checksum,
                                 "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(!is_error(frame), "{frame}");
    assert_eq!(payload(frame)["operation"], json!("append"));
    // The edit reports the new revision too.
    assert_eq!(
        payload(frame)["checksum"].as_str().expect("checksum").len(),
        64
    );
    let note = std::fs::read_to_string(vault.join("notes/simple.md")).expect("note");
    assert!(note.contains("appended by a guarded edit"), "{note}");
}

#[test]
fn a_stale_checksum_edit_is_a_tool_error_and_leaves_the_note_alone() {
    let (dir, vault, mut requests) = session("revision-edit-stale");
    let before = std::fs::read_to_string(vault.join("notes/simple.md")).expect("fixture note");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "edit_note",
                   "arguments": {"identifier": "notes/simple", "operation": "append",
                                 "content": "\n\n- [fact] must not land",
                                 "expected_checksum": "2222222222222222222222222222222222222222222222222222222222222222",
                                 "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(is_error(frame), "{frame}");
    assert_eq!(payload(frame)["error"], json!("NOTE_REVISION_CONFLICT"));
    let after = std::fs::read_to_string(vault.join("notes/simple.md")).expect("note");
    assert_eq!(before, after, "a refused append is never in the note");
    assert!(!after.contains("must not land"));
}

#[test]
fn a_guarded_edit_of_a_missing_note_is_a_conflict_not_an_autocreate() {
    let (dir, vault, mut requests) = session("revision-edit-missing");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "edit_note",
                   "arguments": {"identifier": "notes/never-existed", "operation": "append",
                                 "content": "text",
                                 "expected_checksum": "3333333333333333333333333333333333333333333333333333333333333333",
                                 "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(is_error(frame), "{frame}");
    assert_eq!(payload(frame)["error"], json!("NOTE_REVISION_CONFLICT"));
    // The unguarded behavior creates the note; the guarded one must not.
    assert!(!vault.join("notes/never-existed.md").exists());
}

#[test]
fn the_conflict_is_also_a_tool_error_on_the_text_surface() {
    let (dir, vault, mut requests) = session("revision-text-surface");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "write_note",
                   "arguments": {"title": "simple", "directory": "notes",
                                 "content": "x", "overwrite": true,
                                 "expected_checksum": "4444444444444444444444444444444444444444444444444444444444444444"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(is_error(frame), "{frame}");
    let text = frame["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("Conflict"), "{text}");
    assert!(text.contains("Re-read"), "{text}");
}
