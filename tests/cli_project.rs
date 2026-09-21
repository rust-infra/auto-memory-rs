//! `auto-memory project` and `auto-memory doctor`.
//!
//! The project lifecycle was the gap the MCP surface pointed at but the CLI did not
//! have: `create_memory_project` / `delete_project` refuse on a constrained server and
//! name a CLI command, and those commands now exist. `doctor` is the only place that
//! reports what the run-time discovery of the *optional* semantic-search pieces chose.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Scratch, copy_dir, fixtures_vault};
use serde_json::Value;

/// Run the binary with `args` and return the parsed stdout JSON.
fn run_json(args: &[&str]) -> (Value, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(args)
        .output()
        .expect("run auto-memory");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not JSON: {error}\n{stdout}"));
    (parsed, output.status.success())
}

fn run(args: &[&str]) -> (String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(args)
        .output()
        .expect("run auto-memory");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        output.status.success(),
    )
}

/// A scratch vault copied from the fixtures, plus an index path beside it.
fn scratch() -> (Scratch, PathBuf, String) {
    let dir = Scratch::new("cli-project");
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let index = dir.join("memory.db");
    (dir, vault, index.to_string_lossy().into_owned())
}

/// `project add` must register *and* index: a registered project with nothing in it
/// reads as a bug, and it is what the MCP refusal tells the caller to run.
#[test]
fn project_add_registers_and_indexes() {
    let (_dir, vault, index) = scratch();
    let (payload, ok) = run_json(&[
        "project",
        "add",
        "oracle",
        &vault.to_string_lossy(),
        "--index",
        &index,
        "--json",
    ]);
    assert!(ok);
    assert_eq!(payload["action"], "added");
    assert_eq!(payload["permalink"], "oracle");
    assert_eq!(payload["indexed"]["entities"], 15);
    assert_eq!(payload["indexed"]["observations"], 16);
    assert_eq!(payload["indexed"]["relations"], 16);

    // Re-adding is an update, not a duplicate, and the counts do not drift.
    let (payload, ok) = run_json(&[
        "project",
        "add",
        "oracle",
        &vault.to_string_lossy(),
        "--index",
        &index,
        "--json",
    ]);
    assert!(ok);
    assert_eq!(payload["action"], "updated");
    assert_eq!(payload["indexed"]["entities"], 15);
}

/// `--no-index` is the escape hatch for registering a vault that is indexed separately.
#[test]
fn project_add_can_skip_indexing() {
    let (_dir, vault, index) = scratch();
    let (payload, ok) = run_json(&[
        "project",
        "add",
        "oracle",
        &vault.to_string_lossy(),
        "--index",
        &index,
        "--no-index",
        "--json",
    ]);
    assert!(ok);
    assert!(payload["indexed"].is_null());

    let (payload, _) = run_json(&["project", "list", "--index", &index, "--json"]);
    assert_eq!(payload["projects"][0]["entities"], 0);
}

/// `--permalink` decouples the generated-permalink prefix from the display name.
#[test]
fn project_add_honours_an_explicit_permalink() {
    let (_dir, vault, index) = scratch();
    let (payload, ok) = run_json(&[
        "project",
        "add",
        "My Notes",
        &vault.to_string_lossy(),
        "--index",
        &index,
        "--permalink",
        "my-notes",
        "--no-index",
        "--json",
    ]);
    assert!(ok);
    assert_eq!(payload["name"], "My Notes");
    assert_eq!(payload["permalink"], "my-notes");
}

/// A vault path that is not a directory must be refused before anything is written.
#[test]
fn project_add_refuses_a_missing_vault() {
    let (dir, _vault, index) = scratch();
    let missing = dir.join("nope");
    let (_stdout, ok) = run(&[
        "project",
        "add",
        "oracle",
        &missing.to_string_lossy(),
        "--index",
        &index,
    ]);
    assert!(!ok);
}

/// Removing a project drops its derived rows — including the FTS rows, which have no
/// foreign key to cascade them — and leaves the markdown vault alone.
#[tokio::test(flavor = "multi_thread")]
async fn project_remove_drops_the_index_and_keeps_the_vault() {
    let (_dir, vault, index) = scratch();
    let (payload, ok) = run_json(&[
        "project",
        "add",
        "oracle",
        &vault.to_string_lossy(),
        "--index",
        &index,
        "--json",
    ]);
    assert!(ok);
    assert_eq!(payload["indexed"]["entities"], 15);
    let markdown_before = std::fs::read_dir(vault.join("notes"))
        .expect("notes")
        .count();

    let (payload, ok) = run_json(&["project", "remove", "oracle", "--index", &index, "--json"]);
    assert!(ok);
    assert_eq!(payload["deleted"], true);

    let (payload, _) = run_json(&["project", "list", "--index", &index, "--json"]);
    assert_eq!(payload["projects"].as_array().expect("projects").len(), 0);

    // The search rows must be gone too, or a removed project would keep answering
    // searches whose target rows no longer exist.
    let store = auto_memory::storage::Store::open(Path::new(&index))
        .await
        .expect("store");
    assert_eq!(
        store.search_index_count().await.expect("search rows"),
        0,
        "FTS rows survived the project deletion"
    );
    assert_eq!(store.projects().await.expect("projects").len(), 0);

    // The vault is the source of truth and must be untouched.
    assert_eq!(
        std::fs::read_dir(vault.join("notes"))
            .expect("notes")
            .count(),
        markdown_before
    );
}

/// `project remove` on an unknown name is an error, not a silent success.
#[test]
fn project_remove_rejects_an_unknown_project() {
    let (_dir, _vault, index) = scratch();
    let (_stdout, ok) = run(&["project", "remove", "ghost", "--index", &index]);
    assert!(!ok);
}

/// `doctor` exits 0 on a machine where nothing named is broken, whatever the optional
/// semantic-search pieces look like.
#[test]
fn doctor_succeeds_with_an_index_and_no_named_vault() {
    let (_dir, vault, index) = scratch();
    let (_stdout, ok) = run(&[
        "project",
        "add",
        "oracle",
        &vault.to_string_lossy(),
        "--index",
        &index,
        "--no-index",
    ]);
    assert!(ok);

    let (payload, ok) = run_json(&["doctor", "--index", &index, "--json"]);
    assert!(ok, "doctor reported a failure: {payload}");
    assert_eq!(payload["ok"], true);
    let names: Vec<&str> = payload["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| check["name"].as_str().expect("name"))
        .collect();
    // Every check the report promises, in order.
    assert_eq!(
        names,
        [
            "index",
            "schema",
            "projects",
            "onnx_runtime",
            "model_cache",
            "reranker_model"
        ]
    );
    // Missing semantic search is a warning, never a failure: the rest of the tool does
    // not need it.
    for check in payload["checks"].as_array().expect("checks") {
        if check["name"] == "onnx_runtime" || check["name"] == "model_cache" {
            assert!(
                check["status"] == "ok" || check["status"] == "warn",
                "{check}"
            );
        }
    }
}

/// A diagnostic must not create what it is diagnosing: `Store::open` would create an
/// empty index, so `doctor` has to check for the file instead of opening it.
#[test]
fn doctor_does_not_create_the_index_it_reports_on() {
    let (dir, _vault, _unused) = scratch();
    let index = dir.join("absent.db");
    assert!(!index.exists());

    let (payload, ok) = run_json(&["doctor", "--index", &index.to_string_lossy(), "--json"]);
    assert!(ok, "a missing index is a warning, not a failure: {payload}");
    assert_eq!(payload["ok"], true);
    assert!(
        !index.exists(),
        "doctor created the index file it was asked to inspect"
    );
    let index_check = payload["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"] == "index")
        .expect("an index check");
    assert_eq!(index_check["status"], "warn");
    // Nothing to report about a database that is not there.
    let names: Vec<&str> = payload["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| check["name"].as_str().expect("name"))
        .collect();
    assert!(!names.contains(&"schema"), "{names:?}");
    assert!(!names.contains(&"projects"), "{names:?}");
}

/// A named vault or project that does not exist *is* a failure, and it must be visible
/// in the exit code so a script can act on it.
#[test]
fn doctor_fails_when_something_named_is_missing() {
    let (_dir, vault, index) = scratch();
    let missing = vault.parent().expect("parent").join("nope");
    let (payload, ok) = run_json(&[
        "doctor",
        "--index",
        &index,
        "--vault",
        &missing.to_string_lossy(),
        "--json",
    ]);
    assert!(!ok);
    assert_eq!(payload["ok"], false);
    let vault_check = payload["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"] == "vault")
        .expect("a vault check");
    assert_eq!(vault_check["status"], "fail");

    // A project can only be judged against an index that exists, so create one first —
    // otherwise the project check is skipped along with the missing index.
    let (_stdout, ok) = run(&[
        "project",
        "add",
        "oracle",
        &vault.to_string_lossy(),
        "--index",
        &index,
        "--no-index",
    ]);
    assert!(ok);
    let (payload, ok) = run_json(&["doctor", "--index", &index, "--project", "ghost", "--json"]);
    assert!(!ok);
    assert_eq!(payload["ok"], false);
    let project_check = payload["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["name"] == "project")
        .expect("a project check");
    assert_eq!(project_check["status"], "fail");
}
