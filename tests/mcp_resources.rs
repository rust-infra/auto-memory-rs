//! MCP resources, prompts, and tool annotations.
//!
//! Upstream 0.24.0 added three protocol surfaces beside `tools`:
//!
//! - **#610** — every `memory://` URL the server hands out answers `resources/read`, and
//!   the manual is served as concrete resources beside it;
//! - **#1145** — a `getting_started` prompt for a new user;
//! - **#1031/#1036/#1037** — every tool carries behavior hints (`readOnlyHint`,
//!   `destructiveHint`, `idempotentHint`, `openWorldHint`) so an agent picks the right tool
//!   on demand instead of trying them to see what they do.
//!
//! The annotations are load-bearing: a hint that disagrees with the read-only enforcement
//! is worse than no hint, so the last test checks them against the enforcement itself.

mod common;

use common::{Scratch, Session, copy_dir, fixtures_vault, frame_of};
use serde_json::{Value, json};

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

fn run(tag: &str, mut requests: Vec<Value>) -> Vec<Value> {
    let (dir, vault, mut prefix) = session(tag);
    prefix.append(&mut requests);
    Session::new(&vault, dir.join("memory.db"))
        .run(&prefix)
        .frames
}

#[test]
fn initialize_advertises_tools_resources_and_prompts() {
    let frames = run("mcp-capabilities", vec![]);
    let capabilities = &frame_of(&frames, 1)["result"]["capabilities"];
    assert!(capabilities["tools"].is_object(), "{capabilities}");
    assert!(capabilities["resources"].is_object(), "{capabilities}");
    assert!(capabilities["prompts"].is_object(), "{capabilities}");
}

#[test]
fn resources_list_covers_the_project_info_and_the_manual() {
    let frames = run(
        "mcp-resources-list",
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "resources/list"})],
    );
    let resources = frame_of(&frames, 2)["result"]["resources"]
        .as_array()
        .expect("resources");
    let uris: Vec<&str> = resources
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .collect();
    // The project info resource a client asks for first.
    assert!(uris.contains(&"memory://oracle/oracle/info"), "{uris:?}");
    // The manual index…
    assert!(uris.contains(&"memory://man"), "{uris:?}");
    // …and one concrete resource per page, so a browsing client sees each with its
    // summary.
    assert!(uris.contains(&"memory://man/search-notes"), "{uris:?}");
    assert!(uris.contains(&"memory://man/cat"), "{uris:?}");
    for resource in resources {
        assert!(
            resource["description"].is_string(),
            "every resource needs a description: {resource}"
        );
        assert!(
            resource["mimeType"].is_string(),
            "every resource needs a mime type: {resource}"
        );
    }
}

#[test]
fn a_note_is_readable_as_a_resource_by_permalink() {
    let frames = run(
        "mcp-resource-note",
        vec![json!({
            "jsonrpc": "2.0", "id": 2, "method": "resources/read",
            "params": {"uri": "memory://oracle/notes/simple"}
        })],
    );
    let contents = frame_of(&frames, 2)["result"]["contents"]
        .as_array()
        .expect("contents");
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["uri"], json!("memory://oracle/notes/simple"));
    assert_eq!(contents[0]["mimeType"], json!("text/markdown"));
    // The raw markdown, frontmatter included — the point of a resource rather than a
    // tool result.
    let text = contents[0]["text"].as_str().expect("text");
    assert!(text.contains("# Simple Note"), "{text}");
}

#[test]
fn a_note_is_readable_as_a_resource_by_title() {
    let frames = run(
        "mcp-resource-title",
        vec![json!({
            "jsonrpc": "2.0", "id": 2, "method": "resources/read",
            "params": {"uri": "memory://oracle/Frontmatter Demo"}
        })],
    );
    let text = frame_of(&frames, 2)["result"]["contents"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("Frontmatter Demo"), "{text}");
}

#[test]
fn the_project_info_resource_answers_json() {
    let frames = run(
        "mcp-resource-info",
        vec![json!({
            "jsonrpc": "2.0", "id": 2, "method": "resources/read",
            "params": {"uri": "memory://oracle/oracle/info"}
        })],
    );
    let contents = &frame_of(&frames, 2)["result"]["contents"][0];
    assert_eq!(contents["mimeType"], json!("application/json"));
    let info: Value = serde_json::from_str(contents["text"].as_str().expect("text")).expect("json");
    assert_eq!(info["name"], json!("oracle"));
    assert!(info["entities"].is_number(), "{info}");
}

#[test]
fn the_manual_index_and_a_page_are_resources() {
    let frames = run(
        "mcp-resource-manual",
        vec![
            json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read",
                   "params": {"uri": "memory://man"}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read",
                   "params": {"uri": "memory://man/search-notes"}}),
        ],
    );
    let index = frame_of(&frames, 2)["result"]["contents"][0]["text"]
        .as_str()
        .expect("index");
    assert!(index.contains("search-notes"), "{index}");
    let page = frame_of(&frames, 3)["result"]["contents"][0]["text"]
        .as_str()
        .expect("page");
    assert!(page.contains("# search-notes(3)"), "{page}");
}

#[test]
fn a_missing_note_points_at_the_way_to_find_it() {
    // A miss is a resource that explains itself rather than a protocol error: a client
    // browsing resources should not have to catch an exception to learn the note is gone.
    let frames = run(
        "mcp-resource-missing",
        vec![json!({
            "jsonrpc": "2.0", "id": 2, "method": "resources/read",
            "params": {"uri": "memory://oracle/notes/no-such-note"}
        })],
    );
    let text = frame_of(&frames, 2)["result"]["contents"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("Not Found"), "{text}");
    assert!(text.contains("search_notes"), "{text}");
}

#[test]
fn a_non_memory_uri_is_refused() {
    let frames = run(
        "mcp-resource-bad-uri",
        vec![json!({
            "jsonrpc": "2.0", "id": 2, "method": "resources/read",
            "params": {"uri": "file:///etc/passwd"}
        })],
    );
    let error = frame_of(&frames, 2)["error"]["message"]
        .as_str()
        .expect("a foreign scheme must fail");
    assert!(error.contains("Expected memory://"), "{error}");
}

#[test]
fn prompts_list_advertises_getting_started() {
    let frames = run(
        "mcp-prompts-list",
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "prompts/list"})],
    );
    let prompts = frame_of(&frames, 2)["result"]["prompts"]
        .as_array()
        .expect("prompts");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["name"], json!("getting_started"));
    assert!(
        prompts[0]["description"].is_string(),
        "a prompt needs a description"
    );
}

#[test]
fn the_getting_started_prompt_explains_the_workflow() {
    let frames = run(
        "mcp-prompt-get",
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "prompts/get",
                    "params": {"name": "getting_started"}})],
    );
    let messages = frame_of(&frames, 2)["result"]["messages"]
        .as_array()
        .expect("messages");
    assert_eq!(messages.len(), 1);
    let text = messages[0]["content"]["text"].as_str().expect("text");
    // The prompt names the tools and the two patterns that make a note knowledge.
    for expected in [
        "search_notes",
        "write_note",
        "edit_note",
        "expected_checksum",
        "[decision]",
        "relates_to",
    ] {
        assert!(
            text.contains(expected),
            "{expected} missing from the prompt"
        );
    }
}

#[test]
fn an_unknown_prompt_is_refused() {
    let frames = run(
        "mcp-prompt-unknown",
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "prompts/get",
                    "params": {"name": "no_such_prompt"}})],
    );
    let error = frame_of(&frames, 2)["error"]["message"]
        .as_str()
        .expect("an unknown prompt must fail");
    assert!(error.contains("unknown prompt"), "{error}");
    assert!(error.contains("getting_started"), "{error}");
}

#[test]
fn every_tool_carries_behavior_hints() {
    let frames = run(
        "mcp-annotations",
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})],
    );
    let tools = frame_of(&frames, 2)["result"]["tools"]
        .as_array()
        .expect("tools");
    assert!(!tools.is_empty());
    for tool in tools {
        let name = tool["name"].as_str().expect("name");
        let annotations = &tool["annotations"];
        assert!(annotations.is_object(), "{name} has no annotations: {tool}");
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(
                annotations[hint].is_boolean(),
                "{name}: {hint} must be a boolean, got {}",
                annotations[hint]
            );
        }
    }
}

#[test]
fn the_read_only_hint_agrees_with_the_read_only_enforcement() {
    // A hint that disagrees with the enforcement is worse than no hint: a client that
    // trusts `readOnlyHint` would call a mutating tool and be refused.
    let frames = run(
        "mcp-annotations-readonly",
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})],
    );
    let tools = frame_of(&frames, 2)["result"]["tools"]
        .as_array()
        .expect("tools")
        .clone();
    for tool in &tools {
        let name = tool["name"].as_str().expect("name");
        let read_only = tool["annotations"]["readOnlyHint"]
            .as_bool()
            .expect("readOnlyHint");
        let mutating = [
            "write_note",
            "edit_note",
            "move_note",
            "delete_note",
            "create_memory_project",
            "delete_project",
        ]
        .contains(&name);
        assert_eq!(
            read_only, !mutating,
            "{name}: readOnlyHint={read_only} but mutating={mutating}"
        );
    }
}

#[test]
fn the_destructive_hint_marks_the_tools_that_can_lose_work() {
    let frames = run(
        "mcp-annotations-destructive",
        vec![json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})],
    );
    let tools = frame_of(&frames, 2)["result"]["tools"]
        .as_array()
        .expect("tools")
        .clone();
    for tool in &tools {
        let name = tool["name"].as_str().expect("name");
        let destructive = tool["annotations"]["destructiveHint"]
            .as_bool()
            .expect("destructiveHint");
        let loses_work =
            ["delete_note", "delete_project", "write_note", "edit_note"].contains(&name);
        assert_eq!(
            destructive, loses_work,
            "{name}: destructiveHint={destructive} but loses_work={loses_work}"
        );
    }
}

#[test]
fn a_read_only_session_still_advertises_hints() {
    // `--read-only` hides the mutating tools; the remaining ones keep their annotations,
    // so the two surfaces cannot drift.
    let dir = Scratch::new("mcp-annotations-ro");
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ];
    let output = Session::new(&vault, dir.join("memory.db"))
        .with_args(["--read-only"])
        .run(&requests);
    for tool in frame_of(&output.frames, 2)["result"]["tools"]
        .as_array()
        .expect("tools")
    {
        assert_eq!(
            tool["annotations"]["readOnlyHint"],
            json!(true),
            "a read-only session only advertises read tools: {tool}"
        );
    }
}
