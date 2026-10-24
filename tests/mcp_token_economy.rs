//! Token-economy surfaces: `compact`, bounded line reads, and content lengths.
//!
//! Upstream 0.24.0 gave an agent three ways to avoid paying for note bodies it does not
//! need yet:
//!
//! - **#686** — `search_notes(compact=True)` and `build_context(compact=True)` return
//!   identifiers, metadata, relation targets and pagination without the bodies;
//! - **#1501** — `read_note(start_line, end_line)` returns numbered lines plus the next
//!   range to read;
//! - **#1455** — a search result carries `content_length` and `content_truncated`, so a
//!   truncated preview is never mistaken for the whole note.
//!
//! The point of all three is the same: let the model decide what to read before it pays
//! for the reading.

mod common;

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
fn a_compact_search_page_keeps_identifiers_and_drops_bodies() {
    let (dir, vault, mut requests) = session("compact-search");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "search_notes",
                   "arguments": {"query": "rust", "output_format": "json"}}
    }));
    requests.push(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "search_notes",
                   "arguments": {"query": "rust", "output_format": "json", "compact": true}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let full = payload(frame_of(&output.frames, 2));
    let compact = payload(frame_of(&output.frames, 3));

    let full_rows = full["results"].as_array().expect("full rows");
    let compact_rows = compact["results"].as_array().expect("compact rows");
    assert!(!full_rows.is_empty(), "the fixture must match `rust`");
    assert_eq!(
        full_rows.len(),
        compact_rows.len(),
        "compact keeps every row"
    );

    for (full, compact) in full_rows.iter().zip(compact_rows) {
        // Identifiers and ranking survive…
        for field in ["title", "permalink", "file_path", "score", "type"] {
            assert_eq!(full[field], compact[field], "compact dropped {field}");
        }
        // …the bodies do not.
        assert!(compact.get("content").is_none(), "compact kept content");
        assert!(
            compact.get("matched_chunk").is_none(),
            "compact kept matched_chunk"
        );
        // A caller still learns how much it is deferring.
        if full.get("content_length").is_some() {
            assert_eq!(full["content_length"], compact["content_length"]);
        }
    }
    // Pagination is untouched, so `page`/`page_size` still walk the same result set.
    assert_eq!(full["total"], compact["total"]);
    assert_eq!(full["has_more"], compact["has_more"]);
}

#[test]
fn a_compact_search_page_renders_as_markdown_without_match_previews() {
    let (dir, vault, mut requests) = session("compact-search-text");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "search_notes",
                   "arguments": {"query": "rust", "compact": true}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let text = frame_of(&output.frames, 2)["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("# Search Results: rust"), "{text}");
    assert!(
        !text.contains("- match: "),
        "compact dropped the preview: {text}"
    );
}

#[test]
fn a_compact_context_keeps_the_graph_and_drops_the_bodies() {
    let (dir, vault, mut requests) = session("compact-context");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "build_context", "arguments": {"url": "memory://notes/simple"}}
    }));
    requests.push(json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "build_context",
                   "arguments": {"url": "memory://notes/simple", "compact": true}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let full = payload(frame_of(&output.frames, 2));
    let compact = payload(frame_of(&output.frames, 3));

    let full_primary = &full["results"][0];
    let compact_primary = &compact["results"][0];

    // The graph is what compact keeps: relation targets are how the caller decides what
    // to read next, so they must survive.
    assert_eq!(
        full_primary["related_results"].as_array().map(Vec::len),
        compact_primary["related_results"].as_array().map(Vec::len),
        "compact must keep every related row"
    );
    // Observations are not loaded in compact mode at all (#1702).
    assert!(
        compact_primary.get("observations").is_none(),
        "compact JSON must carry no observations key: {compact_primary}"
    );
    // The primary body is dropped; the identifiers are not.
    assert!(compact_primary["primary_result"].get("content").is_none());
    assert_eq!(
        full_primary["primary_result"]["permalink"],
        compact_primary["primary_result"]["permalink"]
    );
    assert_eq!(
        full_primary["primary_result"]["title"],
        compact_primary["primary_result"]["title"]
    );
    // Pagination and metadata are unchanged.
    assert_eq!(full["page"], compact["page"]);
    assert_eq!(full["page_size"], compact["page_size"]);
    assert_eq!(full["has_more"], compact["has_more"]);
    assert_eq!(full["metadata"]["depth"], compact["metadata"]["depth"]);
}

#[test]
fn a_bounded_read_numbers_lines_and_names_the_next_range() {
    let (dir, vault, mut requests) = session("line-range");
    // The fixture note is short, so a two-line window leaves a next range.
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/simple", "start_line": 1, "end_line": 2}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let text = frame_of(&output.frames, 2)["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("(lines 1-2 of"), "{text}");
    // Every returned line is numbered, so the caller can cite and continue.
    assert!(text.contains("   1 | "), "{text}");
    assert!(text.contains("   2 | "), "{text}");
    assert!(text.contains("Next: read_note("), "{text}");
    assert!(text.contains("start_line=3"), "{text}");
}

#[test]
fn a_bounded_read_of_the_last_range_says_it_is_the_end() {
    let (dir, vault, mut requests) = session("line-range-end");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/simple", "start_line": 1, "end_line": 999}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let text = frame_of(&output.frames, 2)["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("End of document"), "{text}");
}

#[test]
fn a_start_past_the_end_names_the_length() {
    let (dir, vault, mut requests) = session("line-range-past-end");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "read_note",
                   "arguments": {"identifier": "notes/simple", "start_line": 500}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    // Out-of-range reads are caller errors, and the message says how long the document
    // is so the caller can pick a valid range instead of guessing again.
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("error message");
    assert!(
        error.contains("start_line 500 is past the end of the document"),
        "{error}"
    );
    assert!(error.contains("lines)"), "{error}");
}

#[test]
fn a_search_result_reports_its_content_length() {
    let (dir, vault, mut requests) = session("content-length");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "search_notes",
                   "arguments": {"query": "rust", "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let rows = payload(frame_of(&output.frames, 2))["results"]
        .as_array()
        .expect("rows")
        .clone();
    let with_content = rows
        .iter()
        .find(|row| row.get("content").is_some())
        .expect("a row with a content preview");
    let length = with_content["content_length"]
        .as_u64()
        .expect("content_length must be present");
    assert!(length > 0);
    // The fixture notes are short, so nothing is truncated and the flag says so.
    assert_eq!(with_content["content_truncated"], json!(false));
    let content = with_content["content"].as_str().expect("content");
    assert!(
        content.chars().count() <= length as usize,
        "the preview must not be longer than the reported length"
    );
}

#[test]
fn the_default_search_page_still_carries_bodies() {
    // Compact is opt-in: a caller that does not ask for it keeps today's behavior.
    let (dir, vault, mut requests) = session("compact-default-off");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "search_notes",
                   "arguments": {"query": "rust", "output_format": "json"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let rows = payload(frame_of(&output.frames, 2))["results"]
        .as_array()
        .expect("rows");
    assert!(
        rows.iter().any(|row| row.get("content").is_some()),
        "the default page must still carry content"
    );
}
