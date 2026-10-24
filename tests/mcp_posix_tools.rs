//! The POSIX-style read tools: `cat`, `grep`, `ls`, `find`, `tail`, `man`.
//!
//! Upstream 0.24.0 (#1398, #1501, #610) adds a second tool surface beside the rich note
//! tools, using a vocabulary an agent already knows. Two properties make it usable rather
//! than merely familiar:
//!
//! - **projects are mount points** — `ls /` lists them, and a path whose first segment names
//!   a project routes there, so every path the tools return can be passed straight back;
//! - **a note is addressable by heading or line range** — the `note_section` index makes
//!   `cat` able to read one section without loading the whole file into the answer.

mod common;

use common::{Scratch, Session, copy_dir, fixtures_vault, frame_of};
use serde_json::{Value, json};

/// The text payload of one tool call.
fn text(frame: &Value) -> String {
    frame["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text payload: {frame}"))
        .to_owned()
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

/// Run one `tools/call` and return its text.
fn call(tag: &str, name: &str, arguments: Value) -> String {
    let (dir, vault, mut requests) = session(tag);
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(frame.get("error").is_none(), "{name} failed: {frame}");
    text(frame)
}

#[test]
fn cat_prints_a_note() {
    let body = call("posix-cat", "cat", json!({ "path": "notes/simple.md" }));
    assert!(body.contains("# Simple Note"), "{body}");
    assert!(body.contains("baseline fixture"), "{body}");
}

#[test]
fn cat_reads_one_section_by_heading() {
    // `notes/relations.md` has one `# Relations` heading and nothing after it, so the
    // section is the whole body — the point is that the heading addresses it.
    let body = call(
        "posix-cat-section",
        "cat",
        json!({ "path": "notes/relations.md", "section": "Relations" }),
    );
    assert!(body.contains("Relations"), "{body}");
    assert!(body.contains("depends_on"), "{body}");
}

#[test]
fn cat_refuses_a_section_that_is_not_there() {
    let (dir, vault, mut requests) = session("posix-cat-bad-section");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "cat",
                   "arguments": {"path": "notes/relations.md", "section": "Missing"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("a missing section must fail the call");
    assert!(error.contains("section 'Missing' not found"), "{error}");
    // The error names the sections that do exist, so the caller can correct itself.
    assert!(error.contains("Relations"), "{error}");
}

#[test]
fn cat_reads_a_line_range() {
    let body = call(
        "posix-cat-lines",
        "cat",
        json!({ "path": "notes/simple.md", "start_line": 1, "end_line": 2 }),
    );
    assert!(body.contains("lines 1-2 of"), "{body}");
    // A range read is numbered, so the caller can cite what it read.
    assert!(body.contains("   1 | # Simple Note"), "{body}");
}

#[test]
fn cat_refuses_a_missing_note() {
    let (dir, vault, mut requests) = session("posix-cat-missing");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "cat", "arguments": {"path": "notes/nope.md"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("a missing note must fail the call");
    assert!(error.contains("no such note"), "{error}");
}

#[test]
fn cat_refuses_a_path_that_climbs_out_of_the_project() {
    let (dir, vault, mut requests) = session("posix-cat-escape");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "cat", "arguments": {"path": "../../etc/passwd"}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("an escaping path must fail the call");
    assert!(error.contains("must stay within a project"), "{error}");
}

#[test]
fn grep_finds_matching_lines_with_numbers() {
    let out = call("posix-grep", "grep", json!({ "pattern": "rust" }));
    assert!(!out.is_empty(), "the fixture mentions rust");
    // `path:line: text` is the shape a shell user expects.
    assert!(
        out.lines()
            .any(|line| line.contains(":") && line.contains("rust")),
        "{out}"
    );
}

#[test]
fn grep_is_case_insensitive() {
    let lower = call("posix-grep-lower", "grep", json!({ "pattern": "rust" }));
    let upper = call("posix-grep-upper", "grep", json!({ "pattern": "RUST" }));
    assert_eq!(lower, upper);
}

#[test]
fn grep_with_no_match_returns_no_results() {
    // Upstream 0.24.0 (#1685): grep matches keywords by default, so a pattern found in no
    // note returns nothing rather than a nearest-neighbour list.
    let out = call(
        "posix-grep-miss",
        "grep",
        json!({ "pattern": "zzzz-not-in-any-note" }),
    );
    assert!(out.contains("No matches"), "{out}");
}

#[test]
fn grep_scopes_to_a_directory() {
    let out = call(
        "posix-grep-scope",
        "grep",
        json!({ "pattern": "rust", "path": "notes" }),
    );
    for line in out.lines() {
        assert!(
            line.starts_with("notes/"),
            "every hit must be under notes/: {line}"
        );
    }
}

#[test]
fn grep_context_lines_do_not_repeat_a_line() {
    let out = call(
        "posix-grep-context",
        "grep",
        json!({ "pattern": "rust", "context_lines": 3 }),
    );
    let mut seen: Vec<String> = Vec::new();
    for line in out.lines() {
        assert!(
            !seen.contains(&line.to_owned()),
            "a line repeated: {line}\n{out}"
        );
        seen.push(line.to_owned());
    }
}

#[test]
fn ls_of_the_root_lists_project_mounts() {
    let out = call("posix-ls-root", "ls", json!({ "path": "/" }));
    assert!(out.contains("oracle"), "{out}");
}

#[test]
fn ls_of_a_directory_lists_its_children() {
    let out = call("posix-ls-dir", "ls", json!({ "path": "notes" }));
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.contains(&"simple.md"), "{out}");
    // A directory child is marked with a trailing slash.
    assert!(lines.iter().any(|line| line.ends_with('/')), "{out}");
}

#[test]
fn find_matches_a_metadata_predicate() {
    let out = call(
        "posix-find",
        "find",
        json!({ "predicate": ["status=active"] }),
    );
    assert!(!out.contains("No notes match"), "{out}");
    assert!(out.contains("frontmatter.md"), "{out}");
}

#[test]
fn find_projects_requested_fields() {
    let out = call(
        "posix-find-fields",
        "find",
        json!({ "predicate": ["status=active"], "fields": ["status"] }),
    );
    assert!(out.contains("status=active"), "{out}");
}

#[test]
fn find_compares_numbers_numerically() {
    // `priority: 3` — a string comparison would put "3" below "20", the numeric one does
    // not.
    let hit = call(
        "posix-find-numeric-hit",
        "find",
        json!({ "predicate": ["priority>2"] }),
    );
    assert!(hit.contains("frontmatter.md"), "{hit}");
    let miss = call(
        "posix-find-numeric-miss",
        "find",
        json!({ "predicate": ["priority>5"] }),
    );
    assert!(miss.contains("No notes match"), "{miss}");
}

#[test]
fn find_refuses_a_malformed_predicate() {
    let (dir, vault, mut requests) = session("posix-find-bad");
    requests.push(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "find", "arguments": {"predicate": ["status"]}}
    }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("a malformed predicate must fail the call");
    assert!(error.contains("must be key<op>value"), "{error}");
}

#[test]
fn tail_prints_the_last_lines_with_a_header() {
    let out = call("posix-tail", "tail", json!({ "count": 2 }));
    assert!(out.contains("==>"), "{out}");
    // Two lines per note means the note's last two lines, not its first two.
    let notes = out.matches("==>").count();
    assert!(notes >= 1, "{out}");
}

#[test]
fn man_lists_every_page() {
    let out = call("posix-man-list", "man", json!({ "list": true }));
    for page in [
        "search-notes",
        "read-note",
        "cat",
        "grep",
        "ls",
        "find",
        "tail",
        "man",
    ] {
        assert!(out.contains(page), "{page} missing from the index: {out}");
    }
}

#[test]
fn man_prints_a_page_by_any_spelling() {
    for spelling in [
        "search-notes",
        "search-notes(3)",
        "3/search-notes",
        "search_notes",
    ] {
        let out = call("posix-man-page", "man", json!({ "topic": spelling }));
        assert!(
            out.contains("# search-notes(3)"),
            "spelling {spelling:?} failed: {out}"
        );
        assert!(out.contains("## SYNOPSIS"), "{out}");
    }
}

#[test]
fn man_of_an_unknown_topic_says_so() {
    let out = call(
        "posix-man-missing",
        "man",
        json!({ "topic": "no-such-tool" }),
    );
    assert!(out.contains("No manual entry"), "{out}");
    // It points at the way out rather than dead-ending.
    assert!(out.contains("man(list=True)"), "{out}");
}

#[test]
fn every_posix_tool_is_advertised() {
    let (dir, vault, mut requests) = session("posix-advertised");
    requests.push(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let tools: Vec<&str> = frame_of(&output.frames, 2)["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for name in ["cat", "grep", "ls", "find", "tail", "man"] {
        assert!(
            tools.contains(&name),
            "{name} must be advertised: {tools:?}"
        );
    }
}
