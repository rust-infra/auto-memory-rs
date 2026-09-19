//! Phase 11: the stdio MCP server speaks only protocol on stdout.
//!
//! The session is scripted end to end: `initialize`, `tools/list`, note write/read/
//! edit, a search, `build_context`, diagnostics, and the error paths. Every stdout
//! line must be a JSON-RPC frame with a matching id, and diagnostics/articles must
//! stay off stdout.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
mod common;
use common::{
    Scratch, Session, SessionOutput, canonicalize_uuids, copy_dir_with_mtimes, fixtures_vault,
    frame_of, repo_root, text_payload,
};

/// Run one scripted MCP session and return the parsed stdout frames plus stderr.
fn run_session(requests: &[Value]) -> (Vec<Value>, String, Scratch, PathBuf) {
    run_session_in(&fixtures_vault(), requests)
}

/// Run one scripted MCP session against an explicit vault source tree.
fn run_session_in(
    source_vault: &Path,
    requests: &[Value],
) -> (Vec<Value>, String, Scratch, PathBuf) {
    let dir = Scratch::new("session");
    let vault = dir.join("vault");
    copy_dir_with_mtimes(source_vault, &vault);
    let SessionOutput { frames, stderr } =
        Session::new(&vault, dir.join("memory.db")).run(requests);
    (frames, stderr, dir, vault)
}

/// Run one scripted session against a vault whose file mtimes are age-shifted to one
/// minute before "now", preserving the relative order they were captured with.
///
/// The captured `recent_activity` cases were taken from a vault written moments before
/// they ran, and one of them uses a `1d` window: replaying them against the checked-in
/// fixture mtimes stops matching the day after the corpus is generated. Shifting the
/// whole tree keeps the recency order (and the listing tie-breaks) exact.
fn run_recent_session(requests: &[Value]) -> (Vec<Value>, String, Scratch, PathBuf) {
    let dir = Scratch::new("session-recent");
    let vault = dir.join("vault");
    copy_dir_with_mtimes(&fixtures_vault(), &vault);
    shift_mtimes(&vault, Duration::from_secs(60));
    let SessionOutput { frames, stderr } =
        Session::new(&vault, dir.join("memory.db")).run(requests);
    (frames, stderr, dir, vault)
}

/// Move every file mtime under `root` so the newest one is `age` old, keeping the
/// relative offsets intact.
fn shift_mtimes(root: &Path, age: Duration) {
    let mut files = Vec::new();
    collect_files(root, &mut files);
    let newest = files
        .iter()
        .filter_map(|path| fs::metadata(path).ok()?.modified().ok())
        .max()
        .expect("fixture mtimes");
    let target = SystemTime::now()
        .checked_sub(age)
        .expect("now minus a minute");
    let (forward, delta) = match target.duration_since(newest) {
        Ok(delta) => (true, delta),
        Err(error) => (false, error.duration()),
    };
    for file in files {
        let Ok(metadata) = fs::metadata(&file) else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let shifted = if forward {
            modified.checked_add(delta)
        } else {
            modified.checked_sub(delta)
        };
        if let Some(shifted) = shifted
            && let Ok(handle) = fs::OpenOptions::new().write(true).open(&file)
        {
            let _ = handle.set_modified(shifted);
        }
    }
}

fn collect_files(directory: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// The reference golden captured from `basic-memory mcp` (`tools/dump_reference_mcp.py`).
fn reference_session() -> Vec<Value> {
    let raw = fs::read_to_string(repo_root().join("tests/golden/mcp/responses.json"))
        .expect("reference MCP golden");
    let document: Value = serde_json::from_str(&raw).expect("golden JSON");
    document["responses"].as_array().expect("responses").clone()
}

fn golden_case<'a>(session: &'a [Value], id: &str) -> &'a Value {
    session
        .iter()
        .find(|entry| entry["id"] == json!(id))
        .unwrap_or_else(|| panic!("no golden case {id}"))
}

fn request_of(case: &Value) -> Value {
    case["request"].clone()
}

#[test]
fn mcp_session_exposes_tools_and_keeps_stdout_clean() {
    let (frames, stderr, _dir, vault) = run_session(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        // `output_format="json"`: `search_notes` defaults to text markdown, like the
        // reference, so the JSON payload has to be asked for.
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "search_notes",
                          "arguments": {"query": "rust", "output_format": "json"}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
               "params": {"name": "build_context",
                          "arguments": {"url": "memory://notes/simple", "depth": 1}}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
               "params": {"name": "basic_memory_diagnostics", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
               "params": {"name": "read_note",
                          "arguments": {"identifier": "oracle/notes/simple",
                                        "output_format": "json"}}}),
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
               "params": {"name": "nope", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 8, "method": "unknown/method"}),
    ]);

    // Notifications produce no frame: 8 responses for 9 requests.
    assert_eq!(
        frames.len(),
        8,
        "one frame per request with an id: {frames:?}"
    );
    for frame in &frames {
        assert_eq!(frame["jsonrpc"], "2.0");
    }

    let initialize = &frame_of(&frames, 1)["result"];
    assert_eq!(initialize["protocolVersion"], "2024-11-05");
    assert_eq!(initialize["serverInfo"]["name"], "auto-memory-rs");
    assert!(initialize["capabilities"]["tools"].is_object());

    let tools = frame_of(&frames, 2)["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    for expected in [
        "write_note",
        "read_note",
        "view_note",
        "read_content",
        "edit_note",
        "move_note",
        "delete_note",
        "search_notes",
        "build_context",
        "list_directory",
        "recent_activity",
        "list_memory_projects",
        "create_memory_project",
        "delete_project",
        "basic_memory_diagnostics",
    ] {
        assert!(tools.contains(&expected.to_owned()), "missing {expected}");
    }

    // Tool results arrive as text content holding the JSON payload.
    let search = &frame_of(&frames, 3)["result"];
    assert_eq!(search["isError"], false);
    let payload: Value =
        serde_json::from_str(search["content"][0]["text"].as_str().expect("text content"))
            .expect("payload");
    assert!(!payload["results"].as_array().expect("results").is_empty());

    let context: Value = serde_json::from_str(
        frame_of(&frames, 4)["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("context payload");
    assert_eq!(context["metadata"]["uri"], "oracle/notes/simple");
    assert_eq!(context["metadata"]["depth"], 1);

    // The reference declares this tool with `output_schema=None`: the payload is a
    // markdown report and the frame carries no `structuredContent`.
    let diagnostics = frame_of(&frames, 5)["result"].clone();
    let report = diagnostics["content"][0]["text"].as_str().expect("text");
    assert!(
        report.starts_with("# Basic Memory Diagnostics\n"),
        "{report}"
    );
    assert!(
        report.contains("\n## Version\n- auto-memory-rs: "),
        "{report}"
    );
    assert!(report.contains("\n## System\n- OS: "), "{report}");
    assert!(
        report.contains("\n## Configuration\n- Project: oracle ("),
        "{report}"
    );
    assert!(
        report.contains("\"entities\"") && report.contains("\"tools\""),
        "{report}"
    );
    assert!(
        diagnostics.get("structuredContent").is_none(),
        "output_schema=None means no structured payload: {diagnostics}"
    );

    let note: Value = serde_json::from_str(
        frame_of(&frames, 6)["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("note payload");
    assert_eq!(note["file_path"], "notes/simple.md");
    assert_eq!(note["permalink"], "oracle/notes/simple");

    // Unknown tool / method are JSON-RPC errors, not panics.
    assert_eq!(
        frame_of(&frames, 7)["error"]["message"],
        "unknown tool: nope"
    );
    assert!(
        frame_of(&frames, 8)["error"]["message"]
            .as_str()
            .expect("message")
            .contains("unsupported method")
    );

    // stdout is protocol-only; diagnostics stay on stderr.
    assert!(
        !stderr.contains("\"jsonrpc\""),
        "protocol frames must not leak to stderr"
    );
    assert!(vault.join("notes/simple.md").exists());
}

/// Phase 11: `list_directory`'s text surface, character for character.
///
/// The reference goldens were captured from a live `basic-memory mcp` session by
/// `tools/dump_reference_mcp.py`. Every case below is deterministic *except* the
/// ordering of files whose `updated_at` ties, which is why the tie-break rule
/// (stable Python `sort(reverse=True)`, so equal keys keep identity order) matters
/// and is exercised here. The vault is the fixture tree rather than
/// `tests/golden/vault` because the `updated_*` order is derived from file mtimes,
/// and only the fixture tree still carries the mtimes the golden was captured with.
#[test]
fn mcp_list_directory_replays_the_reference_listing_text() {
    let reference = reference_session();
    let cases = [
        "list-directory-root",
        "list-directory-notes-depth2",
        "list-directory-notes-page2-sorted",
        "list-directory-glob",
        "list-directory-page-beyond",
    ];
    let requests: Vec<Value> = cases
        .iter()
        .map(|id| request_of(golden_case(&reference, id)))
        .collect();
    let (frames, stderr, _dir, _vault) = run_session(&requests);

    for (offset, id) in cases.iter().enumerate() {
        let request_id = requests[offset]["id"].as_u64().expect("id");
        let ours = canonicalize_uuids(&text_payload(frame_of(&frames, request_id)));
        let expected = canonicalize_uuids(&text_payload(golden_case(&reference, id)));
        assert_eq!(ours, expected, "list_directory text differs for {id}");
    }
    assert!(!stderr.contains("\"jsonrpc\""));
}

/// Phase 11: the `list_directory` JSON page and `read_content` payload shapes.
#[test]
fn mcp_directory_json_and_read_content_match_the_reference_shapes() {
    let reference = reference_session();

    // `list_directory(output_format="json")` is a `str | dict` tool, so FastMCP
    // nests its payload under `structuredContent.result`.
    let json_case = golden_case(&reference, "list-directory-json");
    let (frames, _stderr, _dir, _vault) = run_session(&[request_of(json_case)]);
    let frame = frame_of(&frames, json_case["request"]["id"].as_u64().expect("id"));
    let payload: Value = serde_json::from_str(&text_payload(frame)).expect("listing payload");
    assert_eq!(frame["result"]["structuredContent"]["result"], payload);

    let expected: Value = serde_json::from_str(&text_payload(&json_case["frame"])).expect("golden");
    assert_eq!(payload["page"], expected["page"]);
    assert_eq!(payload["page_size"], expected["page_size"]);
    assert_eq!(payload["total"], expected["total"]);
    assert_eq!(payload["has_more"], expected["has_more"]);
    let names = |value: &Value| {
        value["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .map(|node| node["name"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&payload), names(&expected));
    let field = |value: &Value, name: &str, key: &str| {
        value["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|node| node["name"] == json!(name))
            .unwrap_or_else(|| panic!("missing node {name}"))[key]
            .clone()
    };
    assert_eq!(field(&payload, "nested", "type"), json!("directory"));
    assert_eq!(field(&payload, "nested", "children"), json!([]));
    for name in ["cjk.md", "frontmatter.md", "simple.md"] {
        assert_eq!(field(&payload, name, "type"), json!("file"));
        assert_eq!(
            field(&payload, name, "content_type"),
            json!("text/markdown")
        );
        assert_eq!(field(&payload, name, "children"), json!([]));
    }
    assert_eq!(
        field(&payload, "frontmatter.md", "permalink"),
        field(&expected, "frontmatter.md", "permalink"),
        "an explicit frontmatter permalink survives indexing"
    );
    assert_eq!(
        field(&payload, "frontmatter.md", "note_type"),
        json!("reference")
    );
}

/// Phase 11: `recent_activity` and `list_memory_projects`.
///
/// `recent_activity` is the orientation call a model makes at session start, so its
/// recency order and its empty-window guidance both matter. The order is
/// `updated_at DESC` over *file mtimes*, which is why this runs against the fixture
/// tree (see the directory-list test for the same reason).
#[test]
fn mcp_recent_activity_and_project_list_replay_the_reference() {
    let reference = reference_session();
    let text_cases = [
        "recent-activity-text",
        "recent-activity-empty-window",
        "list-memory-projects",
        "create-memory-project-constrained",
        "delete-project-constrained",
    ];
    let json_cases = [
        "recent-activity-json",
        "list-memory-projects-json",
        "create-memory-project-constrained-json",
    ];
    let cases: Vec<&str> = text_cases
        .iter()
        .chain(json_cases.iter())
        .copied()
        .collect();
    let requests: Vec<Value> = cases
        .iter()
        .map(|id| request_of(golden_case(&reference, id)))
        .collect();
    let (frames, stderr, _dir, vault) = run_recent_session(&requests);

    for id in text_cases {
        let request_id = golden_case(&reference, id)["request"]["id"]
            .as_u64()
            .expect("id");
        let ours = canonicalize_uuids(&text_payload(frame_of(&frames, request_id)));
        let expected = canonicalize_uuids(&text_payload(golden_case(&reference, id)));
        assert_eq!(ours, expected, "activity/project text differs for {id}");
    }

    for id in json_cases {
        let request_id = golden_case(&reference, id)["request"]["id"]
            .as_u64()
            .expect("id");
        let ours: Value =
            serde_json::from_str(&text_payload(frame_of(&frames, request_id))).expect("payload");
        let expected: Value =
            serde_json::from_str(&text_payload(golden_case(&reference, id))).expect("golden");
        assert_eq!(
            canonical_payload(&ours, &vault),
            canonical_payload(&expected, &vault),
            "activity/project payload differs for {id}"
        );
    }

    // The recency list is ordered by `updated_at DESC`, i.e. most recently modified
    // first — not by index order and not by title.
    let rows: Value = serde_json::from_str(&text_payload(frame_of(
        &frames,
        golden_case(&reference, "recent-activity-json")["request"]["id"]
            .as_u64()
            .expect("id"),
    )))
    .expect("rows");
    let paths: Vec<&str> = rows
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["file_path"].as_str().expect("file_path"))
        .collect();
    assert_eq!(
        paths,
        vec![
            "duplicates/dup-b/same-title.md",
            "duplicates/dup-a/same-title.md",
            "notes/unresolved.md",
            "projects/beta.md",
            "projects/alpha.md",
        ]
    );
    assert!(!stderr.contains("\"jsonrpc\""));
}

/// Drop the machine- and run-specific fields before comparing two JSON payloads.
///
/// `vault` is substituted for the golden's `<work>/vault` placeholder, `created_at`
/// is dropped (the reference stores the row's insert time, this port the `st_ctime`
/// fallback — both wall-clock dependent), and UUIDs become `<uuid>`.
fn canonical_payload(value: &Value, vault: &Path) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| key.as_str() != "created_at")
                .map(|(key, value)| (key.clone(), canonical_payload(value, vault)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| canonical_payload(item, vault))
                .collect(),
        ),
        Value::String(text) => {
            let normalized = canonicalize_uuids(text);
            let normalized =
                normalized.replace(&vault.to_string_lossy().into_owned(), "<work>/vault");
            Value::String(normalized)
        }
        other => other.clone(),
    }
}

/// Phase 11: `read_content` and `view_note` against the normalized golden vault.
#[test]
fn mcp_read_content_and_view_note_replay_the_reference_payloads() {
    let reference = reference_session();
    let normalized = repo_root().join("tests/golden/vault");
    let cases = [
        "read-content-markdown",
        "read-content-permalink",
        "view-note",
        "view-note-missing",
        "read-note-missing-text",
        "read-note-text",
        "read-note-json",
    ];
    let requests: Vec<Value> = cases
        .iter()
        .map(|id| request_of(golden_case(&reference, id)))
        .collect();
    let (frames, _stderr, _dir, _vault) = run_session_in(&normalized, &requests);

    for (offset, id) in cases.iter().enumerate() {
        let request_id = requests[offset]["id"].as_u64().expect("id");
        let ours = text_payload(frame_of(&frames, request_id));
        let expected = text_payload(golden_case(&reference, id));
        if id.starts_with("view-note") || id.ends_with("-text") {
            // `view_note` and `read_note(output_format="text")` return `str`, so their
            // content text is byte-comparable.
            assert_eq!(ours, expected, "{id} text differs");
            continue;
        }
        // `read_content` returns a `dict`: the reference emits compact JSON with
        // FastMCP's key order, so compare the decoded payload field by field.
        let ours: Value = serde_json::from_str(&ours).expect("read_content payload");
        let expected: Value = serde_json::from_str(&expected).expect("golden payload");
        assert_eq!(ours, expected, "read_content payload differs for {id}");
        if id.starts_with("read-content") {
            assert_eq!(
                frame_of(&frames, request_id)["result"]["structuredContent"],
                ours,
                "read_content is the one tool the reference does not nest under `result`"
            );
        } else {
            // `read_note` returns `str | dict`, so FastMCP nests it under `result`.
            assert_eq!(
                frame_of(&frames, request_id)["result"]["structuredContent"]["result"],
                ours,
                "read_note json payload should be nested under structuredContent.result"
            );
        }
    }

    // The raw file content is served verbatim, frontmatter included.
    let markdown = text_payload(frame_of(&frames, requests[0]["id"].as_u64().expect("id")));
    let markdown: Value = serde_json::from_str(&markdown).expect("payload");
    assert_eq!(markdown["content_type"], "text/markdown; charset=utf-8");
    assert_eq!(markdown["encoding"], "utf-8");
    assert!(
        markdown["text"]
            .as_str()
            .expect("text")
            .starts_with("---\ntitle: simple\ntype: note\npermalink: oracle/notes/simple\n---")
    );

    // Documented divergence: `memory://notes/frontmatter` resolves here but not in the
    // reference, because this port's `resolve_entity_path` also tries `<path>.md` while
    // the reference only accepts the permalink, an exact file path, a title, or an
    // external id. `mcp-spec.md` §1c records the deviation; the captured frame stays in
    // the golden as evidence.
    let (frames, _stderr, _dir, _vault) = run_session_in(
        &normalized,
        &[request_of(golden_case(
            &reference,
            "read-note-json-frontmatter",
        ))],
    );
    let resolved: Value = serde_json::from_str(&text_payload(frame_of(&frames, 22))).expect("json");
    assert_eq!(resolved["file_path"], "notes/frontmatter.md");
    assert_eq!(
        text_payload(golden_case(&reference, "read-note-json-frontmatter")),
        "{\"title\":null,\"permalink\":null,\"file_path\":null,\"content\":null,\"frontmatter\":null}"
    );
}

#[test]
fn mcp_note_tools_write_read_and_edit_the_vault() {
    // Session 1: create and read back; the file is inspected before anything edits it.
    let (frames, _stderr, _dir, vault) = run_session(&[
        // `output_format="json"`: the write surface defaults to a text summary.
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "write_note",
                          "arguments": {"title": "mcp-note", "directory": "notes",
                                        "content": "Body from MCP\n",
                                        "metadata": {"status": "active"},
                                        "output_format": "json"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "read_note",
                          "arguments": {"identifier": "notes/mcp-note",
                                        "output_format": "json"}}}),
    ]);

    let written: Value = serde_json::from_str(
        frame_of(&frames, 1)["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("write payload");
    assert_eq!(written["file_path"], "notes/mcp-note.md");
    assert_eq!(written["permalink"], "oracle/notes/mcp-note");
    let text = fs::read_to_string(vault.join("notes/mcp-note.md")).expect("file");
    assert!(text.contains("status: active"), "{text}");
    // A plain body is written verbatim, so its trailing newline survives
    // (the reference trims only a body that arrived with its own frontmatter).
    assert!(text.ends_with("Body from MCP\n"), "{text}");

    let read: Value = serde_json::from_str(
        frame_of(&frames, 2)["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("read payload");
    // The reference's `parse_opening_frontmatter` returns everything after the closing
    // fence, which includes the blank separator line the writer emits.
    assert_eq!(read["content"], "\nBody from MCP\n");

    // Session 2: section replacement rewrites the body and leaves the frontmatter alone.
    let (frames, _stderr, _dir, vault) = run_session(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "write_note",
                          "arguments": {"title": "mcp-edit", "directory": "notes",
                                        "content": "## Section\n\nOld body.\n",
                                        "output_format": "json"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "edit_note",
                          "arguments": {"identifier": "notes/mcp-edit",
                                        "operation": "replace_section",
                                        "section": "Section",
                                        "content": "New body.",
                                        "output_format": "json"}}}),
    ]);

    let edited: Value = serde_json::from_str(
        frame_of(&frames, 2)["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("edit payload");
    // The edit surface reports what it did; the body lives in the file (checked below).
    assert_eq!(edited["operation"], "replace_section");
    assert_eq!(edited["fileCreated"], false);
    assert_eq!(edited["file_path"], "notes/mcp-edit.md");
    let text = fs::read_to_string(vault.join("notes/mcp-edit.md")).expect("file");
    assert!(text.contains("permalink: oracle/notes/mcp-edit"), "{text}");
    assert!(text.contains("## Section\nNew body."), "{text}");
    assert!(!text.contains("Old body."));

    // Session 3: move; the permalink survives the rename.
    let (frames, _stderr, _dir, vault) = run_session(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "write_note",
                          "arguments": {"title": "mcp-move", "directory": "notes",
                                        "content": "Move me\n",
                                        "output_format": "json"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "move_note",
                          "arguments": {"identifier": "notes/mcp-move",
                                        "destination_path": "notes/mcp-move-target.md",
                                        "output_format": "json"}}}),
    ]);

    let moved: Value = serde_json::from_str(
        frame_of(&frames, 2)["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("move payload");
    assert_eq!(moved["file_path"], "notes/mcp-move-target.md");
    assert_eq!(moved["permalink"], "oracle/notes/mcp-move");
    assert!(!vault.join("notes/mcp-move.md").exists());
    assert!(vault.join("notes/mcp-move-target.md").exists());

    // Session 4: delete removes the file and its index row.
    let (frames, _stderr, _dir, vault) = run_session(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "write_note",
                          "arguments": {"title": "mcp-delete", "directory": "notes",
                                        "content": "Delete me\n"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "delete_note",
                          "arguments": {"identifier": "notes/mcp-delete",
                                        "output_format": "json"}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "read_note",
                          "arguments": {"identifier": "notes/mcp-delete"}}}),
    ]);

    let deleted: Value = serde_json::from_str(
        frame_of(&frames, 2)["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("delete payload");
    // The JSON surface names what was deleted; the text surface would be `true`.
    assert_eq!(deleted["deleted"], true);
    assert_eq!(deleted["file_path"], "notes/mcp-delete.md");
    assert_eq!(deleted["title"], "mcp-delete");
    assert!(!vault.join("notes/mcp-delete.md").exists());
    // `read_note` defaults to the reference's text surface, so a miss returns the
    // "Note Not Found" guidance rather than a protocol error.
    let missing = text_payload(frame_of(&frames, 3));
    assert!(missing.contains("# Note Not Found"), "{missing}");
    assert!(missing.contains("mcp-delete"), "{:?}", frame_of(&frames, 3));
}
