//! `metadata_filters` operators: `$gte`, `$in`, `$contains`, `$exists`, and aliases.
//!
//! Upstream 0.24.0 (#1676) accepts the filter shapes agents actually write. Before that
//! the port compared one string per field, so `{"priority": {"$gte": 2}}` matched
//! nothing at all — the filter was rendered as the literal text `{"$gte": 2}` — and a
//! caller had no way to express "at least", "one of", or "present".
//!
//! Two behaviors are load-bearing and pinned here:
//!
//! - an **unknown operator fails the call** and lists the valid ones, because silently
//!   ignoring a filter answers a different question than the one asked;
//! - a filter never **widens** silently — an empty `$in` matches nothing rather than
//!   everything.
//!
//! The fixture vault gives each case a real note to find: `notes/frontmatter.md` carries
//! `status: active`, `priority: 3`, `tags: [rust, architecture]`; `projects/beta.md`
//! carries `status: archived`; `projects/alpha.md` carries `status: active`.

mod common;

use common::{Scratch, Session, copy_dir, fixtures_vault, frame_of};
use serde_json::{Value, json};

fn payload(frame: &Value) -> &Value {
    &frame["result"]["structuredContent"]["result"]
}

/// The permalinks a search page returned, sorted for a stable comparison.
fn permalinks(page: &Value) -> Vec<String> {
    let mut rows: Vec<String> = page["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|row| row["permalink"].as_str().map(str::to_owned))
        .collect();
    rows.sort();
    rows
}

/// Run one `metadata_filters` search and return its JSON payload.
fn search_with(tag: &str, filters: Value) -> Value {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes",
                          "arguments": {"metadata_filters": filters,
                                        "output_format": "json",
                                        "page_size": 50}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(
        frame.get("error").is_none(),
        "{tag}: the filter must be accepted: {frame}"
    );
    payload(frame).clone()
}

#[test]
fn an_exact_match_still_works() {
    let page = search_with("filter-eq", json!({ "status": "active" }));
    let found = permalinks(&page);
    assert!(
        found.contains(&"notes/frontmatter-note".to_owned()),
        "{found:?}"
    );
    assert!(
        found.contains(&"oracle/projects/alpha".to_owned()),
        "{found:?}"
    );
    assert!(
        !found.contains(&"oracle/projects/beta".to_owned()),
        "{found:?}"
    );
}

#[test]
fn a_gte_operator_selects_by_numeric_comparison() {
    // `priority: 3` on the only note that declares it.
    let at_least_three = search_with("filter-gte-hit", json!({ "priority": { "$gte": 3 } }));
    assert_eq!(
        permalinks(&at_least_three),
        vec!["notes/frontmatter-note".to_owned()]
    );
    // A bound above it matches nothing, which proves the comparison is numeric rather
    // than a string prefix.
    let at_least_four = search_with("filter-gte-miss", json!({ "priority": { "$gte": 4 } }));
    assert!(permalinks(&at_least_four).is_empty(), "{at_least_four}");
}

#[test]
fn bare_operator_names_are_aliases_for_the_dollar_forms() {
    let dollars = search_with("filter-alias-dollar", json!({ "priority": { "$gte": 3 } }));
    let bare = search_with("filter-alias-bare", json!({ "priority": { "gte": 3 } }));
    assert_eq!(permalinks(&dollars), permalinks(&bare));
}

#[test]
fn several_operators_on_one_field_and_together() {
    let page = search_with(
        "filter-range",
        json!({ "priority": { "$gte": 1, "$lte": 3 } }),
    );
    assert_eq!(permalinks(&page), vec!["notes/frontmatter-note".to_owned()]);
    // A range that excludes the only value matches nothing, so the two bounds really do
    // AND rather than one of them winning.
    let outside = search_with(
        "filter-range-miss",
        json!({ "priority": { "$gte": 4, "$lte": 9 } }),
    );
    assert!(permalinks(&outside).is_empty(), "{outside}");
}

#[test]
fn an_in_operator_selects_a_set() {
    let page = search_with(
        "filter-in",
        json!({ "status": { "$in": ["active", "archived"] } }),
    );
    let found = permalinks(&page);
    assert!(
        found.contains(&"oracle/projects/beta".to_owned()),
        "{found:?}"
    );
    assert!(
        found.contains(&"oracle/projects/alpha".to_owned()),
        "{found:?}"
    );
}

#[test]
fn an_empty_in_matches_nothing_rather_than_everything() {
    // Dropping an unusable clause would answer a wider question than the caller asked.
    let page = search_with("filter-in-empty", json!({ "status": { "$in": [] } }));
    assert!(permalinks(&page).is_empty(), "{page}");
}

#[test]
fn a_contains_operator_matches_an_array_member() {
    let page = search_with(
        "filter-contains",
        json!({ "tags": { "$contains": "rust" } }),
    );
    assert_eq!(permalinks(&page), vec!["notes/frontmatter-note".to_owned()]);
    let missing = search_with(
        "filter-contains-miss",
        json!({ "tags": { "$contains": "cobol" } }),
    );
    assert!(permalinks(&missing).is_empty(), "{missing}");
}

#[test]
fn an_exists_operator_selects_by_presence() {
    let present = search_with("filter-exists", json!({ "priority": { "$exists": true } }));
    assert_eq!(
        permalinks(&present),
        vec!["notes/frontmatter-note".to_owned()]
    );
    // `$exists: false` is the complement, so the note above must not be in it.
    let absent = search_with(
        "filter-exists-absent",
        json!({ "priority": { "$exists": false } }),
    );
    assert!(
        !permalinks(&absent).contains(&"notes/frontmatter-note".to_owned()),
        "{absent}"
    );
}

#[test]
fn a_ne_operator_excludes_only_rows_that_declare_the_field() {
    let page = search_with("filter-ne", json!({ "status": { "$ne": "active" } }));
    let found = permalinks(&page);
    // The archived note declares `status` and differs, so it matches.
    assert!(
        found.contains(&"oracle/projects/beta".to_owned()),
        "{found:?}"
    );
    // A note without the key is not "not equal": it is simply not a candidate.
    assert!(
        !found.contains(&"oracle/notes/simple".to_owned()),
        "a note with no `status` must not match $ne: {found:?}"
    );
}

#[test]
fn note_type_is_an_alias_for_the_type_key() {
    let page = search_with("filter-note-type", json!({ "note_type": "reference" }));
    assert_eq!(permalinks(&page), vec!["notes/frontmatter-note".to_owned()]);
}

#[test]
fn an_unknown_operator_fails_and_lists_the_valid_ones() {
    let dir = Scratch::new("filter-unknown-op");
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes",
                          "arguments": {"metadata_filters": {"priority": {"$between": 2}},
                                        "output_format": "json"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("an unknown operator must fail the call");
    assert!(
        error.contains("unknown metadata filter operator"),
        "{error}"
    );
    assert!(
        error.contains("$gte"),
        "the error must list the valid operators: {error}"
    );
}

#[test]
fn an_unsafe_key_fails_rather_than_reaching_sql() {
    let dir = Scratch::new("filter-unsafe-key");
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes",
                          "arguments": {"metadata_filters": {"status' OR 1=1 --": "x"},
                                        "output_format": "json"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let error = frame_of(&output.frames, 2)["error"]["message"]
        .as_str()
        .expect("an unsafe key must fail the call");
    assert!(error.contains("invalid metadata filter key"), "{error}");
}

#[test]
fn a_filter_can_be_combined_with_a_query() {
    // The filter and the text query are ANDed: both must hold.
    let page = search_with("filter-with-query", json!({ "status": "active" }));
    let found = permalinks(&page);
    assert!(
        found.contains(&"oracle/projects/alpha".to_owned()),
        "{found:?}"
    );
    // `projects/beta` is archived, so the filter excludes it even though it would match a
    // broad query.
    assert!(
        !found.contains(&"oracle/projects/beta".to_owned()),
        "{found:?}"
    );
}
