//! Phase 12–13: the `schema_*` MCP tools against the captured reference frames.
//!
//! `tools/dump_reference_schema_mcp.py` drives a real `basic-memory mcp` session over
//! three vaults and records every response. The reference's tool text is compact JSON
//! or markdown guidance/report text, and both are byte-comparable — there are no
//! paths or UUIDs in these payloads — so this test compares the `content[0].text` of
//! every frame character for character. That covers the report field order, the
//! dropped `null`s, the percent rounding, and the guidance wording in one place.

use std::fs;
use std::path::Path;

use serde_json::Value;
mod common;
use common::{
    Scratch, Session, SessionOutput, canonicalize_renames, copy_dir, load_golden_json, repo_root,
};

fn golden() -> Value {
    load_golden_json("mcp/schema.json")
}

/// Replay one suite's session against its fixture vault.
fn replay(suite: &Value) -> SessionOutput {
    let dir = Scratch::new(suite["name"].as_str().expect("suite name"));
    let vault = dir.join("vault");
    copy_dir(
        &repo_root()
            .join("tests/fixtures")
            .join(suite["fixture"].as_str().expect("fixture")),
        &vault,
    );
    for relative in suite["overrides"].as_array().expect("overrides") {
        // Overridden files are rewritten by the harness from an inline template; the
        // golden records only which paths differ, and the test rebuilds them from the
        // same fixture content it starts with.
        let _ = relative;
    }
    apply_overrides(suite, &vault);

    let requests = suite["responses"]
        .as_array()
        .expect("responses")
        .iter()
        .map(|case| case["request"].clone())
        .collect::<Vec<_>>();
    let SessionOutput { frames, stderr } =
        Session::new(&vault, dir.join("memory.db")).run(&requests);
    SessionOutput { frames, stderr }
}

/// Rewrite the fixture files the oracle harness overrode before indexing.
fn apply_overrides(suite: &Value, vault: &Path) {
    for relative in suite["overrides"].as_array().expect("overrides") {
        let relative = relative.as_str().expect("override path");
        if relative == "schema/person.md" && suite["name"] == "broken-schema-vault" {
            let target = vault.join(relative);
            fs::write(
                &target,
                "---\ntitle: Person\ntype: schema\nentity: person\nversion: 1\nschema:\n  \
                 name: string, full name\nsettings:\n  validation: nonsense\n---\n\n# Person \
                 Schema\n",
            )
            .expect("write override");
        }
    }
}

fn frame_of(frames: &[Value], id: u64) -> &Value {
    frames
        .iter()
        .find(|frame| frame["id"] == id)
        .unwrap_or_else(|| panic!("missing frame {id}"))
}

fn text_payload(frame: &Value) -> &str {
    frame["result"]["content"][0]["text"]
        .as_str()
        .expect("content text")
}

#[test]
fn mcp_schema_tools_replay_the_reference_frames() {
    let golden = golden();
    let suites = golden["suites"].as_array().expect("suites");
    assert_eq!(suites.len(), 3, "one suite per vault");

    let mut checked = 0;
    for suite in suites {
        let SessionOutput { frames, stderr } = replay(suite);
        assert!(
            !stderr.contains("\"jsonrpc\""),
            "protocol frames must not leak to stderr"
        );

        for case in suite["responses"].as_array().expect("responses") {
            if case["id"] == "initialize" {
                continue;
            }
            let request_id = case["request"]["id"].as_u64().expect("id");
            let ours = canonicalize_renames(text_payload(frame_of(&frames, request_id)));
            let expected = canonicalize_renames(text_payload(&case["frame"]));
            assert_eq!(
                ours,
                expected,
                "{} :: {} where {} differs",
                suite["name"].as_str().unwrap_or("?"),
                case["id"],
                case["request"]["params"]["name"].as_str().unwrap_or("?")
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 30, "every captured frame is replayed");
}

/// The all-types report shape: per-type summaries plus the flattened results.
#[test]
fn mcp_schema_validate_all_types_reports_per_type_summaries() {
    let golden = golden();
    let suite = golden["suites"]
        .as_array()
        .expect("suites")
        .iter()
        .find(|suite| suite["name"] == "schema-vault")
        .expect("schema-vault suite");
    let SessionOutput { frames, .. } = replay(suite);

    let case = suite["responses"]
        .as_array()
        .expect("responses")
        .iter()
        .find(|case| case["id"] == "8-schema_validate-output_format")
        .expect("all-types json case");
    let payload: Value = serde_json::from_str(text_payload(frame_of(
        &frames,
        case["request"]["id"].as_u64().expect("id"),
    )))
    .expect("payload");

    assert!(
        payload.get("note_type").is_none(),
        "null note_type is dropped"
    );
    assert_eq!(payload["total_entities"], 3);
    let summaries = payload["type_summaries"].as_array().expect("summaries");
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0]["note_type"], "person");
    assert_eq!(summaries[1]["note_type"], "project");
    // A strict-mode schema reports its enum mismatch as an error, so the project note
    // fails while both person notes only warn.
    assert_eq!(summaries[1]["valid_count"], 0);
    assert_eq!(summaries[1]["error_count"], 1);
}
