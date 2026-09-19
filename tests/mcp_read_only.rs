//! `--read-only` hides and refuses the mutating tools.

mod common;

use common::{Scratch, Session, copy_dir, fixtures_vault, frame_of};
use serde_json::json;

#[test]
fn read_only_sessions_hide_and_refuse_the_write_tools() {
    let dir = Scratch::new("read-only");
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "read_note", "arguments": {"identifier": "notes/simple"}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
               "params": {"name": "write_note",
                          "arguments": {"title": "Nope", "content": "nope"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db"))
        .with_args(["--read-only"])
        .run(&requests);

    let tools: Vec<&str> = frame_of(&output.frames, 2)["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for hidden in ["write_note", "edit_note", "move_note", "delete_note"] {
        assert!(!tools.contains(&hidden), "{hidden} must be hidden");
    }
    for kept in [
        "read_note",
        "search_notes",
        "build_context",
        "list_directory",
    ] {
        assert!(tools.contains(&kept), "{kept} must stay");
    }
    assert_eq!(tools.len(), 20 - 6, "read tools plus the schema trio");

    // Reads still work…
    assert!(
        frame_of(&output.frames, 3)["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("Simple"))
    );
    // …and a write is refused with an explicit message rather than silently ignored.
    let error = frame_of(&output.frames, 4)["error"]["message"]
        .as_str()
        .expect("error message");
    assert_eq!(error, "tool not available in read-only mode: write_note");
    assert!(!vault.join("nope.md").exists());
}
