//! Boolean queries: routing and whole-note evaluation.
//!
//! Upstream 0.24.0 (#1683) fixed two linked defects in one change:
//!
//! 1. **Routing.** A query using `AND`/`OR`/`NOT` without an explicit `search_type` now
//!    runs as full text. The vector half of a hybrid search ignores Boolean operators
//!    entirely, so a `NOT` used to exclude nothing while the response looked like it had
//!    honored the query.
//! 2. **Evaluation.** A boolean query is evaluated with one table-level `MATCH` rather
//!    than a three-column `OR`. SQLite applied `NOT` to each column separately, so
//!    `coffee NOT pour` matched a note whose *title* has "coffee" and whose *body* has
//!    "pour" — the exclusion never applied to the note as a whole.
//!
//! The fixture gives the second case a real shape: `notes/frontmatter.md` has
//! "Frontmatter Demo" as its title, "rust" and "architecture" in its tags, and
//! "Markdown"/"cloud" in its body.

mod common;

use common::{Scratch, Session, copy_dir, fixtures_vault, frame_of};
use serde_json::{Value, json};

fn payload(frame: &Value) -> &Value {
    &frame["result"]["structuredContent"]["result"]
}

fn titles(page: &Value) -> Vec<String> {
    page["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|row| row["title"].as_str().map(str::to_owned))
        .collect()
}

/// Run one search and return its JSON payload.
fn search(tag: &str, arguments: Value) -> Value {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes", "arguments": arguments}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let frame = frame_of(&output.frames, 2);
    assert!(frame.get("error").is_none(), "{tag}: {frame}");
    payload(frame).clone()
}

#[test]
fn an_and_query_finds_a_note_containing_both_terms() {
    let page = search(
        "bool-and",
        json!({ "query": "rust AND architecture", "output_format": "json", "page_size": 20 }),
    );
    let found = titles(&page);
    assert!(
        found.contains(&"Frontmatter Demo".to_owned()),
        "both terms are in the note: {found:?}"
    );
}

#[test]
fn an_and_query_excludes_a_note_with_only_one_term() {
    // `notes/simple.md` mentions rust but never architecture, so the AND must drop it.
    let page = search(
        "bool-and-excludes",
        json!({ "query": "rust AND architecture", "output_format": "json", "page_size": 20 }),
    );
    let found = titles(&page);
    assert!(
        !found.contains(&"simple".to_owned()),
        "an AND must not match a note with only one term: {found:?}"
    );
}

#[test]
fn a_not_query_excludes_a_term_found_anywhere_in_the_note() {
    // The whole-note rule: `rust NOT architecture` must drop a note that has both, no
    // matter which column each term sits in.
    let page = search(
        "bool-not",
        json!({ "query": "rust NOT architecture", "output_format": "json", "page_size": 20 }),
    );
    let found = titles(&page);
    assert!(
        !found.contains(&"Frontmatter Demo".to_owned()),
        "a note with `architecture` must be excluded even when `rust` is elsewhere: {found:?}"
    );
    // `simple` has rust and no architecture, so it survives.
    assert!(
        found.contains(&"simple".to_owned()),
        "a note with rust and no architecture must match: {found:?}"
    );
}

#[test]
fn an_or_query_widens_the_match() {
    let page = search(
        "bool-or",
        json!({ "query": "architecture OR cloud", "output_format": "json", "page_size": 20 }),
    );
    let found = titles(&page);
    // `frontmatter` has both; `simple` has neither. The OR must not narrow to the
    // intersection.
    assert!(found.contains(&"Frontmatter Demo".to_owned()), "{found:?}");
}

#[test]
fn a_boolean_query_runs_as_full_text_even_with_a_semantic_default() {
    // The routing rule: a boolean query without an explicit search_type stays on the
    // full-text path. This server has no embedding runtime, so a vector or hybrid route
    // would answer with the semantic-disabled guidance instead of results — which makes
    // the guidance text itself the signal that routing went wrong.
    let page = search(
        "bool-routing",
        json!({ "query": "rust AND architecture", "output_format": "json", "page_size": 20 }),
    );
    assert!(
        page.get("results").is_some(),
        "a boolean query must run as full text, not fall into semantic guidance: {page}"
    );
    assert!(!titles(&page).is_empty(), "{page}");
}

#[test]
fn an_explicit_search_type_still_wins() {
    // The caller can still ask for a semantic search of a boolean-shaped string; the
    // routing change only applies when no search_type was named.
    let dir = Scratch::new("bool-explicit-type");
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes",
                          "arguments": {"query": "rust AND architecture",
                                        "search_type": "hybrid"}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let text = frame_of(&output.frames, 2)["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    // No embedding runtime is attached, so the explicit semantic request reports that
    // rather than quietly answering with text results.
    assert!(
        text.contains("Semantic Search Disabled"),
        "an explicit search_type must not be overridden: {text}"
    );
}

#[test]
fn a_relaxed_fallback_is_not_treated_as_a_boolean_query() {
    // The relaxed fallback joins its terms with ` OR `, so a naive check on the prepared
    // FTS expression would route every relaxed search through the boolean path and
    // change its scores. The check has to look at the caller's own text.
    let dir = Scratch::new("bool-relaxed");
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        // No note contains this, so it takes the relaxed path.
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes",
                          "arguments": {"query": "no-such-note-anywhere",
                                        "output_format": "json", "page_size": 10}}}),
    ];
    let output = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let page = payload(frame_of(&output.frames, 2));
    let rows = page["results"].as_array().expect("relaxed rows");
    // The relaxed path answers with a ranked list; its bm25 scores are the ordinary
    // magnitude, not the near-zero values a table-level MATCH produces for this shape.
    let first = rows
        .first()
        .and_then(|row| row["score"].as_f64())
        .expect("a relaxed score");
    assert!(
        first < -0.1,
        "the relaxed fallback must keep its own scoring, got {first}"
    );
}
