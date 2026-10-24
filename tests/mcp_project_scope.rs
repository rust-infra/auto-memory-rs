//! Cross-project scoping: `projects`, and the attribution every merged row carries.
//!
//! Upstream 0.24.0 (#1558) lets `search_notes` search a chosen subset of projects by name
//! or external id, and stamps each row on a multi-project page with the project it came
//! from — so the next call can be routed to the right one. An unknown name is an **error**
//! rather than a silently narrower search, because a typo would otherwise read as "that
//! project has nothing".
//!
//! The vault here holds two projects over two directories, registered in one index.

mod common;

use std::fs;

use common::{Scratch, Session, frame_of};
use serde_json::{Value, json};

fn payload(frame: &Value) -> &Value {
    &frame["result"]["structuredContent"]["result"]
}

/// Two projects over two vaults, registered in one index.
fn two_projects(tag: &str) -> (Scratch, std::path::PathBuf, std::path::PathBuf) {
    let dir = Scratch::new(tag);
    let alpha = dir.join("alpha-vault");
    let beta = dir.join("beta-vault");
    for (vault, title, body) in [
        (
            &alpha,
            "Alpha Note",
            "- [fact] Alpha mentions shared keyword.\n",
        ),
        (
            &beta,
            "Beta Note",
            "- [fact] Beta mentions shared keyword.\n",
        ),
    ] {
        fs::create_dir_all(vault).expect("vault");
        fs::write(
            vault.join("note.md"),
            format!("---\ntitle: {title}\ntype: note\n---\n\n# {title}\n\n{body}"),
        )
        .expect("note");
    }
    (dir, alpha, beta)
}

/// Register both projects through the CLI, then run one MCP session.
fn run(
    dir: &Scratch,
    alpha: &std::path::PathBuf,
    beta: &std::path::PathBuf,
    requests: Vec<Value>,
) -> Vec<Value> {
    let index = dir.join("memory.db");
    for (name, vault) in [("oracle", alpha), ("beta", beta)] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_auto-memory"))
            .args(["project", "add", name])
            .arg(vault)
            .arg("--index")
            .arg(&index)
            .output()
            .expect("project add");
        assert!(
            output.status.success(),
            "project add {name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // The session is pinned to the first project so it can start; the cross-project paths
    // reach the other one through the shared index.
    Session::new(alpha, &index).run(&requests).frames
}

#[test]
fn a_projects_subset_returns_only_that_project() {
    let (dir, alpha, beta) = two_projects("projects-subset");
    let frames = run(
        &dir,
        &alpha,
        &beta,
        vec![
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "search_notes",
                              "arguments": {"query": "shared", "projects": ["oracle"],
                                            "output_format": "json", "page_size": 50}}}),
        ],
    );
    let page = payload(frame_of(&frames, 2));
    let rows = page["results"].as_array().expect("results");
    assert!(!rows.is_empty(), "{page}");
    for row in rows {
        assert_eq!(
            row["project"],
            json!("oracle"),
            "every row must come from the requested project: {row}"
        );
    }
}

#[test]
fn a_project_can_be_named_by_its_external_id() {
    let (dir, alpha, beta) = two_projects("projects-external-id");
    let frames = run(
        &dir,
        &alpha,
        &beta,
        vec![
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "list_memory_projects",
                              "arguments": {"output_format": "json"}}}),
        ],
    );
    let projects = payload(frame_of(&frames, 2));
    let beta_id = projects["projects"]
        .as_array()
        .expect("projects")
        .iter()
        .find(|project| project["name"] == json!("beta"))
        .and_then(|project| project["external_id"].as_str())
        .expect("beta external id")
        .to_owned();

    let frames = run(
        &dir,
        &alpha,
        &beta,
        vec![
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "search_notes",
                              "arguments": {"query": "shared", "projects": [beta_id],
                                            "output_format": "json", "page_size": 50}}}),
        ],
    );
    let page = payload(frame_of(&frames, 2));
    for row in page["results"].as_array().expect("results") {
        assert_eq!(row["project"], json!("beta"), "{row}");
    }
}

#[test]
fn an_unknown_project_name_is_an_error_that_lists_the_known_ones() {
    let (dir, alpha, beta) = two_projects("projects-unknown");
    let frames = run(
        &dir,
        &alpha,
        &beta,
        vec![
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "search_notes",
                              "arguments": {"query": "shared", "projects": ["gamma"],
                                            "output_format": "json"}}}),
        ],
    );
    let error = frame_of(&frames, 2)["error"]["message"]
        .as_str()
        .expect("an unknown project must fail the call");
    assert!(error.contains("unknown project 'gamma'"), "{error}");
    // The message names what does exist, so the caller can correct the typo.
    assert!(error.contains("oracle"), "{error}");
    assert!(error.contains("beta"), "{error}");
}

#[test]
fn a_multi_project_page_stamps_every_row_with_its_project() {
    let (dir, alpha, beta) = two_projects("projects-attribution");
    let frames = run(
        &dir,
        &alpha,
        &beta,
        vec![
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "search_notes",
                              "arguments": {"query": "shared", "search_all_projects": true,
                                            "output_format": "json", "page_size": 50}}}),
        ],
    );
    let page = payload(frame_of(&frames, 2));
    let rows = page["results"].as_array().expect("results");
    assert!(!rows.is_empty(), "{page}");
    for row in rows {
        assert!(
            row["project"].is_string(),
            "a merged row must name its project: {row}"
        );
        assert!(
            row["project_id"].is_string(),
            "a merged row must carry the project external id: {row}"
        );
    }
    // Both projects contributed, which is what makes the attribution meaningful.
    let names: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["project"].as_str())
        .collect();
    assert!(names.contains(&"oracle"), "{names:?}");
    assert!(names.contains(&"beta"), "{names:?}");
}

#[test]
fn a_single_project_search_does_not_stamp_rows() {
    // The attribution fields belong to the merged page; a single-project answer keeps the
    // shape the rest of the corpus pins.
    let (dir, alpha, beta) = two_projects("projects-single");
    let frames = run(
        &dir,
        &alpha,
        &beta,
        vec![
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "search_notes",
                              "arguments": {"query": "shared", "output_format": "json"}}}),
        ],
    );
    let page = payload(frame_of(&frames, 2));
    for row in page["results"].as_array().expect("results") {
        assert!(
            row.get("project").is_none(),
            "a single-project row must not carry `project`: {row}"
        );
    }
}
