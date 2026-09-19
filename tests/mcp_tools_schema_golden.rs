//! `tools/list` schemas against the reference capture.
//!
//! The coverage test in `adapters::mcp::server` pins that every `ToolName` is
//! advertised exactly once; this one pins *what* is advertised: the property names and
//! the required list of every tool must match the reference capture
//! (`tests/golden/mcp/responses.json`), apart from the exceptions below.

mod common;

use std::collections::BTreeSet;

use serde_json::Value;

/// Reference arguments this port deliberately does not advertise.
///
/// `project`/`project_id`/`workspace` are the reference's routing arguments; this server
/// is pinned to one project and ignores them. `search_notes`' three field aliases are
/// covered by `search_type` here, and are stripped again by the reference's own schema.
const PROPERTY_EXCEPTIONS: &[(&str, &str)] = &[
    ("*", "project"),
    ("*", "project_id"),
    ("*", "workspace"),
    ("search_notes", "permalink"),
    ("search_notes", "permalink_match"),
    ("search_notes", "title"),
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
        let Some(expected) = reference.iter().find(|entry| entry["name"] == name) else {
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
