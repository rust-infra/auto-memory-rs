//! `search_notes` across every `search_type`, replayed against the reference capture.
//!
//! This port used to answer a `search_type="vector"` request with a text search — a wrong
//! answer rather than a missing feature. `tools/dump_reference_search_mcp.py` captures
//! the reference with semantic search disabled, which pins all four branches: the JSON
//! payload for `text`/`title`/`permalink`, the "Semantic Search Disabled" guidance for
//! `vector`/`hybrid`/`semantic`, the generic "Search Failed" guidance for an unknown
//! type, and the no-results markdown for a miss.

use std::fs;
use std::process::Command;

use serde_json::{Value, json};
mod common;
use common::{
    Scratch, Session, SessionOutput, canonicalize_uuids, copy_dir_with_mtimes, fixtures_vault,
    frame_of, load_golden_json, repo_root, text_payload,
};

fn golden() -> Value {
    load_golden_json("mcp/search-types.json")
}

/// Replay the captured session and return the frames.
fn replay() -> Vec<Value> {
    let reference = golden();
    let requests = reference["responses"]
        .as_array()
        .expect("responses")
        .iter()
        .map(|case| case["request"].clone())
        .collect::<Vec<_>>();

    let dir = Scratch::new("session");
    let vault = dir.join("vault");
    copy_dir_with_mtimes(&fixtures_vault(), &vault);
    let SessionOutput { frames, .. } = Session::new(&vault, dir.join("memory.db")).run(&requests);
    frames
}

#[test]
fn mcp_search_types_replay_the_reference() {
    let reference = golden();
    let frames = replay();

    for case in reference["responses"].as_array().expect("responses") {
        if case["id"] == "initialize" {
            continue;
        }
        let id = case["request"]["id"].as_u64().expect("id");
        let arguments = &case["request"]["params"]["arguments"];
        let search_type = arguments["search_type"].as_str().unwrap_or("text");
        let ours = frame_of(&frames, id);
        let expected = text_payload(&case["frame"]);
        let ours_text = canonicalize_uuids(&text_payload(ours));
        let expected_text = canonicalize_uuids(&expected);

        if matches!(search_type, "vector" | "hybrid" | "semantic" | "nonsense") {
            // Guidance is a plain string, whatever `output_format` asked for.
            assert_eq!(ours_text, expected_text, "{}: guidance differs", case["id"]);
            continue;
        }

        // `output_format` defaults to text: a miss renders guidance, not JSON.
        if expected.starts_with('#') || expected.starts_with("No results found") {
            assert_eq!(
                ours_text, expected_text,
                "{}: text surface differs",
                case["id"]
            );
            continue;
        }

        let payload: Value = serde_json::from_str(&text_payload(ours)).expect("payload");
        let reference_payload: Value = serde_json::from_str(&expected).expect("reference payload");
        assert_eq!(
            payload["total"], reference_payload["total"],
            "{}: total",
            case["id"]
        );
        assert_eq!(
            payload["total_is_exact"], reference_payload["total_is_exact"],
            "{}: total_is_exact",
            case["id"]
        );
        let permalinks = |value: &Value| -> Vec<String> {
            value["results"]
                .as_array()
                .expect("results")
                .iter()
                .map(|row| row["permalink"].as_str().unwrap_or_default().to_owned())
                .collect()
        };
        assert_eq!(
            permalinks(&payload),
            permalinks(&reference_payload),
            "{}: results",
            case["id"]
        );
    }
}

/// The branch this port got wrong: a semantic request must not become a text search.
#[test]
fn semantic_search_types_without_a_runtime_are_refused() {
    let reference = golden();
    let frames = replay();

    for (search_type, id) in [("vector", 6u64), ("hybrid", 7), ("semantic", 8)] {
        let text = text_payload(frame_of(&frames, id));
        assert!(
            text.starts_with("# Search Failed - Semantic Search Disabled"),
            "{search_type}: {text}"
        );
        assert!(
            text.contains(&format!(
                "You requested `{search_type}` search for query 'rust'"
            )),
            "{search_type}: {text}"
        );
        // The reference's wording, captured verbatim.
        let expected = reference["responses"]
            .as_array()
            .expect("responses")
            .iter()
            .find(|case| case["request"]["id"] == serde_json::json!(id))
            .map(|case| case["frame"]["result"]["content"][0]["text"].clone())
            .expect("reference guidance");
        assert_eq!(serde_json::json!(text), expected, "{search_type}");
    }
}

/// A `title` search matches on the title field, a `permalink` search on permalinks.
#[test]
fn title_and_permalink_search_types_use_their_own_field() {
    let frames = replay();

    let title: Value =
        serde_json::from_str(&text_payload(frame_of(&frames, 3))).expect("title payload");
    assert_eq!(title["total"], 1);
    assert_eq!(title["results"][0]["permalink"], "oracle/projects/alpha");

    // The stored permalinks carry the project prefix, so a bare `projects/*` pattern
    // finds nothing — exactly what the reference returned.
    let permalink: Value =
        serde_json::from_str(&text_payload(frame_of(&frames, 4))).expect("permalink payload");
    assert_eq!(permalink["total"], 0);
    let exact: Value =
        serde_json::from_str(&text_payload(frame_of(&frames, 5))).expect("permalink payload");
    assert_eq!(exact["total"], 0);

    // A `title` search for a word that is not a title renders the no-results guidance.
    let miss = text_payload(frame_of(&frames, 10));
    assert!(
        miss.starts_with("No results found for 'rust' in project 'oracle'."),
        "{miss}"
    );
}

/// With an embedding runtime attached, the semantic types run for real.
///
/// The vectors come from the captured reference embeddings (an offline stand-in for the
/// ONNX runtime), so the result must match the reference's own vector search.
#[test]
fn semantic_search_types_run_when_a_runtime_is_configured() {
    let dir = Scratch::new("semantic");
    let vault = dir.join("vault");
    copy_dir_with_mtimes(&repo_root().join("tests/fixtures/vault"), &vault);
    let index = dir.join("memory.db");
    let fixture = repo_root().join("tests/golden/vector/embeddings-reference.json");

    // The vector index has to exist before the server can rank against it.
    let reindex = Command::new(env!("CARGO_BIN_EXE_basic-mem"))
        .args(["reindex", "--vault"])
        .arg(&vault)
        .args(["--index"])
        .arg(&index)
        .args(["--project", "oracle", "--embeddings", "--embedding-fixture"])
        .arg(&fixture)
        .output()
        .expect("reindex");
    assert!(reindex.status.success(), "reindex failed");

    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05", "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "search_notes",
                          "arguments": {"query": "local index", "search_type": "vector",
                                        "output_format": "json"}}}),
    ];
    let SessionOutput { frames, .. } = Session::new(&vault, &index)
        .with_args(["--embedding-fixture", &fixture.to_string_lossy()])
        .run(&requests);

    let payload: Value =
        serde_json::from_str(&text_payload(frame_of(&frames, 2))).expect("payload");

    let reference: Value = serde_json::from_str(
        &fs::read_to_string(repo_root().join("tests/golden/search/vector-local-index.json"))
            .expect("golden"),
    )
    .expect("golden json");
    let permalinks = |value: &Value| -> Vec<String> {
        value["results"]
            .as_array()
            .expect("results")
            .iter()
            .map(|row| row["permalink"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    assert_eq!(
        permalinks(&payload),
        permalinks(&reference),
        "vector ranking"
    );
    assert_eq!(payload["total"], 0, "semantic totals stay inexact");
    assert_eq!(payload["total_is_exact"], false);
    for (index, (ours, theirs)) in payload["results"]
        .as_array()
        .expect("results")
        .iter()
        .zip(reference["results"].as_array().expect("results"))
        .enumerate()
    {
        let difference = (ours["score"].as_f64().unwrap_or_default()
            - theirs["score"].as_f64().unwrap_or_default())
        .abs();
        assert!(
            difference < 5e-4,
            "rank {index} score drift {difference} (the reference runtime is not bit-reproducible)"
        );
    }
}
