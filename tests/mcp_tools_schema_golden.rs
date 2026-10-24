//! `tools/list` schemas against the reference capture.
//!
//! The coverage test in `adapters::mcp::server` pins that every `ToolName` is
//! advertised exactly once; this one pins *what* is advertised: the property names and
//! the required list of every tool must match the reference capture
//! (`tests/golden/mcp/responses.json`), apart from the exceptions below.

mod common;

use common::canonicalize_renames;

use std::collections::BTreeSet;

use serde_json::Value;

/// Reference arguments this port deliberately does not advertise, plus the arguments
/// this port advertises and the 0.23.2 corpus predates.
///
/// `project`/`project_id`/`workspace` are the reference's routing arguments; this server
/// is pinned to one project and ignores them. `search_notes`' three field aliases are
/// covered by `search_type` here, and are stripped again by the reference's own schema.
///
/// The `NEW_IN_0_24` block below is the other direction: upstream 0.24.0 added these and
/// the captured 0.23.2 `tools/list` cannot show them. They are pinned by
/// `tests/mcp_revision_guard.rs` and `tests/mcp_line_reads.rs` instead of by the corpus.
const PROPERTY_EXCEPTIONS: &[(&str, &str)] = &[
    ("*", "project"),
    ("*", "project_id"),
    ("*", "workspace"),
    ("search_notes", "permalink"),
    ("search_notes", "permalink_match"),
    ("search_notes", "title"),
    // NEW_IN_0_24: revision guards (#1552, #1642).
    ("write_note", "expected_checksum"),
    ("edit_note", "expected_checksum"),
    // NEW_IN_0_24: metadata-only reads (#1552).
    ("read_note", "include_content"),
    // NEW_IN_0_24: bounded line reads (#1501).
    ("read_note", "start_line"),
    ("read_note", "end_line"),
    // NEW_IN_0_24: content-free pages (#686).
    ("search_notes", "compact"),
    ("build_context", "compact"),
    // NEW_IN_0_24: valid-time filters (#1426).
    ("search_notes", "valid_at"),
    ("search_notes", "valid_overlaps"),
    ("search_notes", "time_kind"),
    // NEW_IN_0_24: cross-project scoping (#1558).
    ("search_notes", "projects"),
];

/// Reference-required arguments this port treats as optional.
const REQUIRED_EXCEPTIONS: &[(&str, &str)] = &[
    // A missing directory means the project root here.
    ("write_note", "directory"),
    // `edit_note` is an upsert: append/prepend accept an empty body.
    ("edit_note", "content"),
];

fn is_exception(table: &[(&str, &str)], tool: &str, argument: &str) -> bool {
    table
        .iter()
        .any(|(name, key)| (*name == "*" || *name == tool) && *key == argument)
}

fn properties(tool: &Value) -> BTreeSet<String> {
    tool["inputSchema"]["properties"]
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn required(tool: &Value) -> BTreeSet<String> {
    tool["inputSchema"]["required"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn every_tool_advertises_the_reference_parameters() {
    let golden = common::load_golden_json("mcp/responses.json");
    let case = golden["responses"]
        .as_array()
        .expect("responses")
        .iter()
        .find(|case| case["id"] == "tools-list")
        .expect("tools-list case");
    let reference = case["frame"]["result"]["tools"]
        .as_array()
        .expect("reference tools");

    let mut checked = 0;
    for tool in auto_memory::adapters::mcp::tool_definitions() {
        let name = tool["name"].as_str().expect("tool name");
        // `list_workspaces` is reference-only (Web/Cloud stays out of scope).
        let reference_name = canonicalize_renames(name);
        let Some(expected) = reference
            .iter()
            .find(|entry| entry["name"] == reference_name.as_str())
        else {
            continue;
        };
        let (ours, theirs) = (properties(&tool), properties(expected));

        for argument in &theirs {
            if is_exception(PROPERTY_EXCEPTIONS, name, argument) {
                continue;
            }
            assert!(
                ours.contains(argument),
                "{name}: the reference advertises `{argument}`, we do not"
            );
        }
        for argument in &ours {
            if is_exception(PROPERTY_EXCEPTIONS, name, argument) {
                continue;
            }
            assert!(
                theirs.contains(argument),
                "{name}: we advertise `{argument}`, the reference does not"
            );
        }

        let expected_required: BTreeSet<String> = required(expected)
            .into_iter()
            .filter(|argument| !is_exception(REQUIRED_EXCEPTIONS, name, argument))
            .collect();
        assert_eq!(
            required(&tool),
            expected_required,
            "{name}: required arguments differ"
        );
        checked += 1;
    }

    assert!(
        checked >= 20,
        "expected to compare every tool, compared {checked}"
    );
}
