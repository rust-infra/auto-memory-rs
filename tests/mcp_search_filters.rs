//! The `search_notes` filter parameters, checked against the reference query goldens.
//!
//! The reference exposes the same engine through the MCP tool and through
//! `bm tool search-notes`, so a captured CLI query is also the expectation for the MCP
//! tool. These cases cover the parameters that only exist on the tool surface —
//! `entity_types`, `status`, `metadata_filters`, `after_date` — plus the implicit
//! default that scopes a category-only search to observation rows.

use serde_json::{Value, json};
mod common;
use common::{
    Scratch, Session, SessionOutput, copy_dir_with_mtimes, fixtures_vault, frame_of,
    load_golden_json, text_payload,
};

fn golden(name: &str) -> Value {
    load_golden_json(&format!("search/{name}.json"))
}

/// Run one `search_notes` call against a fresh index of the fixture vault.
fn search(mut arguments: Value) -> Value {
    // `search_notes` defaults to text markdown; these cases assert on the payload.
    arguments["output_format"] = json!("json");
    let dir = Scratch::new("session");
    let vault = dir.join("vault");
    copy_dir_with_mtimes(&fixtures_vault(), &vault);

    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes", "arguments": arguments}}),
    ];
    let SessionOutput { frames, .. } = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let text = text_payload(frame_of(&frames, 2));
    serde_json::from_str(&text).expect("payload")
}

/// Compare our rows with a captured reference query on the fields that are stable.
///
/// Rows are compared as a set of `(permalink, score)` pairs: two rows can share a score
/// exactly, and the reference's order between them is its database rowid order, which
/// changes between captures. Ordering *is* a contract for score-separated rows, and
/// `tests/search_golden.rs` pins that.
fn assert_matches(case: &str, payload: &Value) {
    let expected = golden(case);
    assert_eq!(payload["total"], expected["total"], "{case}: total");
    assert_eq!(
        payload["total_is_exact"], expected["total_is_exact"],
        "{case}: total_is_exact"
    );
    assert_eq!(
        payload["has_more"], expected["has_more"],
        "{case}: has_more"
    );

    fn rows(value: &Value) -> Vec<(String, f64)> {
        let mut rows: Vec<(String, f64)> = value["results"]
            .as_array()
            .expect("results")
            .iter()
            .map(|row| {
                let permalink = row["permalink"]
                    .as_str()
                    .or_else(|| row["entity"].as_str())
                    .unwrap_or_default()
                    .to_owned();
                (permalink, row["score"].as_f64().unwrap_or_default())
            })
            .collect();
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        rows
    }

    let actual = rows(payload);
    let reference = rows(&expected);
    assert_eq!(
        actual.iter().map(|row| row.0.clone()).collect::<Vec<_>>(),
        reference
            .iter()
            .map(|row| row.0.clone())
            .collect::<Vec<_>>(),
        "{case}: results differ"
    );
    for (index, ((permalink, actual), (_, reference))) in
        actual.iter().zip(reference.iter()).enumerate()
    {
        assert!(
            (actual - reference).abs() <= 1e-6,
            "{case}: {permalink} score[{index}] expected {reference}, got {actual}"
        );
    }
}

#[test]
fn mcp_search_filters_match_the_reference_queries() {
    let cases = [
        (
            "category-decision-implicit",
            json!({"query": "rust", "categories": ["decision"]}),
        ),
        (
            "entity-type-relation",
            json!({"query": "alpha", "entity_types": ["relation"]}),
        ),
        ("status-archived", json!({"status": "archived"})),
        (
            "metadata-status-active",
            json!({"metadata_filters": {"status": "active"}}),
        ),
        (
            "after-date-absolute-rust",
            json!({"query": "rust", "after_date": "2026-09-01"}),
        ),
        (
            "after-date-future-rust",
            json!({"query": "rust", "after_date": "2030-01-01"}),
        ),
    ];

    for (case, arguments) in cases {
        let payload = search(arguments);
        assert_matches(case, &payload);
    }
}

/// The alias the reference applies to `metadata_filters`: `note_type` is the model
/// column, the frontmatter key is `type`.
#[test]
fn mcp_metadata_filter_aliases_the_note_type_key() {
    let aliased = search(json!({"metadata_filters": {"note_type": "project"}}));
    let direct = search(json!({"metadata_filters": {"type": "project"}}));
    assert_eq!(aliased["total"], direct["total"]);
    assert_eq!(aliased["results"], direct["results"]);
    assert_eq!(direct["total"], 2, "both project notes");
}

/// `title` is a parameter on this tool surface (the reference reaches a title-only
/// search through `search_type="title"`, which drops the text leg), so the two legs have
/// to survive one query. This used to answer with a JSON-RPC error carrying SQLite's
/// "unable to use function MATCH in the requested context".
#[test]
fn mcp_search_combines_a_title_filter_with_a_query() {
    let payload = search(json!({"query": "rust", "title": "Alpha"}));
    assert_eq!(payload["total"], 1, "{payload}");
    assert_eq!(payload["results"][0]["permalink"], "oracle/projects/alpha");
}

/// An unrecognized `entity_types` value is an error, not a silent fallback.
///
/// Dropping it left the parsed list empty, and the implicit default then answered
/// with *entity* rows — a different question than the caller asked. The CLI rejects
/// the same value, so one request meant two things depending on the surface.
#[test]
fn mcp_search_rejects_an_unknown_entity_type() {
    let dir = Scratch::new("session");
    let vault = dir.join("vault");
    copy_dir_with_mtimes(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes",
                          "arguments": {"query": "ada", "entity_types": ["observations"]}}}),
    ];
    let SessionOutput { frames, .. } = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let message = frame_of(&frames, 2)["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(message.contains("Invalid type: observations"), "{message}");
    assert!(message.contains("\"observation\""), "{message}");
}

/// Any page number is answerable; a huge one yields an empty page.
///
/// `search_all_projects` truncated the u64 `page` to u32 — 2^32 became 0 — and then
/// computed `page - 1`, which underflowed and panicked the whole server process.
#[test]
fn mcp_search_answers_an_out_of_range_page() {
    let dir = Scratch::new("session");
    let vault = dir.join("vault");
    copy_dir_with_mtimes(&fixtures_vault(), &vault);
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes",
                          "arguments": {"query": "ada", "search_all_projects": true,
                                        "page": 4_294_967_296u64,
                                        "output_format": "json"}}}),
    ];
    let SessionOutput { frames, .. } = Session::new(&vault, dir.join("memory.db")).run(&requests);
    let text = text_payload(frame_of(&frames, 2));
    let payload: Value = serde_json::from_str(&text).expect("payload");
    assert_eq!(payload["results"], json!([]), "{payload}");
}
