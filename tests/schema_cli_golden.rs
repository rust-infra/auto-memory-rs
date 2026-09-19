//! Phase 12–13: `auto-memory schema …` against the reference's JSON CLI surface.
//!
//! The reference exposes the same three tools through `bm tool schema-validate`,
//! `bm tool schema-infer`, and `bm tool schema-diff`, which print the tool's
//! `output_format="json"` result through
//! `json.dumps(result, indent=2, ensure_ascii=True, default=str)`.
//! `tools/dump_reference_schema_mcp.py` captures that output for the schema fixture
//! vaults, and this test replays each command through our CLI and compares stdout
//! byte for byte — indentation, key order, and the escaped `\uXXXX` forms included.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
mod common;
use common::{Scratch, copy_dir, load_golden_json, repo_root};

fn golden() -> Value {
    load_golden_json("mcp/schema.json")
}

/// Index one fixture vault and return `(index path, vault path, scratch dir)`.
fn prepare(suite: &Value) -> (PathBuf, PathBuf, Scratch) {
    let dir = Scratch::new(suite["name"].as_str().expect("suite name"));
    let vault = dir.join("vault");
    copy_dir(
        &repo_root()
            .join("tests/fixtures")
            .join(suite["fixture"].as_str().expect("fixture")),
        &vault,
    );
    if suite["name"] == "broken-schema-vault" {
        // The harness overrides this file before indexing; see the MCP golden test.
        fs::write(
            vault.join("schema/person.md"),
            "---\ntitle: Person\ntype: schema\nentity: person\nversion: 1\nschema:\n  \
             name: string, full name\nsettings:\n  validation: nonsense\n---\n\n# Person \
             Schema\n",
        )
        .expect("write override");
    }

    let index = dir.join("memory.db");
    let status = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["reindex", "--vault"])
        .arg(&vault)
        .args(["--index"])
        .arg(&index)
        .args(["--project", "oracle"])
        .status()
        .expect("run reindex");
    assert!(status.success(), "reindex failed");
    (index, vault, dir)
}

#[test]
fn schema_cli_replays_the_reference_json_output() {
    let golden = golden();
    let mut checked = 0;
    for suite in golden["suites"].as_array().expect("suites") {
        let (index, vault, _dir) = prepare(suite);

        for case in suite["cli"].as_array().expect("cli") {
            let argv = case["argv"]
                .as_array()
                .expect("argv")
                .iter()
                .map(|word| word.as_str().expect("argument"))
                .collect::<Vec<_>>();
            let trailing = argv
                .iter()
                .position(|word| *word == "--project")
                .expect("--project");
            // `tool schema-validate <args>` is our `schema validate <args>`.
            let mut command = vec![
                "schema",
                argv[1].strip_prefix("schema-").expect("schema- prefix"),
            ];
            command.extend(&argv[2..trailing]);
            command.push("--index");

            let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
                .args(&command)
                .arg(&index)
                .args(["--project", "oracle", "--vault"])
                .arg(&vault)
                .output()
                .expect("run auto-memory schema");

            let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
            assert_eq!(
                stdout,
                case["stdout"].as_str().expect("golden stdout"),
                "{} :: {} differs",
                suite["name"].as_str().unwrap_or("?"),
                case["id"]
            );
            assert_eq!(
                output.status.code().unwrap_or(-1),
                case["exit_code"].as_i64().expect("exit code") as i32,
                "{} :: {} exit code differs",
                suite["name"].as_str().unwrap_or("?"),
                case["id"]
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 14, "every captured CLI command is replayed");
}
