//! Phase 11: the ChatGPT/OpenAI `search` and `fetch` adapters.
//!
//! These two tools are the only ones whose result shape is a *content list* rather than
//! a `str`/`dict`: FastMCP stringifies the returned list once more, so
//! `content[0].text` holds the JSON of the whole list and `structuredContent.result`
//! holds the list itself. They are also the only tools gated on client identity —
//! `tools/dump_reference_chatgpt_mcp.py` captures both sides of that gate (a neutral
//! client and `openai-mcp`) into `tests/golden/mcp/chatgpt.json`.
//!
//! The vault is `tests/golden/vault` (the post-sync normalization of the fixture tree),
//! because `fetch` returns the file *as written on disk* and the reference rewrites
//! frontmatter during indexing.

use serde_json::Value;
mod common;
use common::{
    Scratch, Session, SessionOutput, copy_dir, frame_of, golden_vault, load_golden_json,
    text_payload,
};

fn golden() -> Value {
    load_golden_json("mcp/chatgpt.json")
}

/// Replay one scripted session and return the parsed stdout frames.
fn replay(requests: &[Value]) -> Vec<Value> {
    let dir = Scratch::new("session");
    let vault = dir.join("vault");
    copy_dir(&golden_vault(), &vault);
    let SessionOutput { frames, .. } = Session::new(&vault, dir.join("memory.db")).run(requests);
    frames
}

/// Decode one adapter payload: the content list's first item holds the JSON body.
fn payload_of(frame: &Value) -> Value {
    let items: Value = serde_json::from_str(&text_payload(frame)).expect("content list");
    let text = items[0]["text"].as_str().expect("item text");
    serde_json::from_str(text).expect("payload")
}

#[test]
fn chatgpt_adapters_replay_the_reference_frames() {
    let golden = golden();
    let suites = golden["suites"].as_array().expect("suites");
    assert_eq!(suites.len(), 2, "one suite per client identity");

    let mut checked = 0;
    for suite in suites {
        let cases = suite["responses"].as_array().expect("responses");
        let requests = cases
            .iter()
            .map(|case| case["request"].clone())
            .collect::<Vec<_>>();
        let frames = replay(&requests);

        for case in cases {
            if case["id"] == "initialize" {
                continue;
            }
            let request_id = case["request"]["id"].as_u64().expect("id");
            let ours = frame_of(&frames, request_id);
            let reference = &case["frame"];

            // The text surface is the stringified content list, and the structured
            // payload is that same list — both are compared.
            assert_eq!(
                text_payload(ours),
                text_payload(reference),
                "{} :: {} text differs",
                suite["name"].as_str().unwrap_or("?"),
                case["id"]
            );
            assert_eq!(
                ours["result"]["structuredContent"]["result"],
                reference["result"]["structuredContent"]["result"],
                "{} :: {} structured payload differs",
                suite["name"].as_str().unwrap_or("?"),
                case["id"]
            );
            assert_eq!(ours["result"]["isError"], false);
            checked += 1;
        }
    }
    assert_eq!(checked, 9, "every captured call is replayed");
}

/// The gate itself: the same request answers differently per client identity.
#[test]
fn chatgpt_adapters_are_gated_on_the_openai_client_identity() {
    let golden = golden();
    let suite = |name: &str| {
        golden["suites"]
            .as_array()
            .expect("suites")
            .iter()
            .find(|suite| suite["name"] == name)
            .unwrap_or_else(|| panic!("missing suite {name}"))
            .clone()
    };

    for (name, expected_error) in [
        ("neutral-client", Some("Unsupported MCP client")),
        ("openai-mcp-client", None),
    ] {
        let suite = suite(name);
        let cases = suite["responses"].as_array().expect("responses");
        let requests = cases
            .iter()
            .map(|case| case["request"].clone())
            .collect::<Vec<_>>();
        let frames = replay(&requests);

        // Pick the cases by tool + arguments: the two suites use different ids for the
        // same edges (the neutral client only sends two calls).
        let case_id = |name: &str, identifier: Option<&str>| -> u64 {
            cases
                .iter()
                .find(|case| {
                    let params = &case["request"]["params"];
                    params["name"] == name
                        && identifier.is_none_or(|expected| params["arguments"]["id"] == expected)
                })
                .unwrap_or_else(|| panic!("missing {name} case"))["request"]["id"]
                .as_u64()
                .expect("id")
        };

        let search = payload_of(frame_of(&frames, case_id("search", None)));
        match expected_error {
            Some(error) => {
                assert_eq!(search["error"], error);
                assert_eq!(search["results"], serde_json::json!([]));
                assert_eq!(search["total_count"], Value::Null);
            }
            None => {
                assert!(
                    search["total_count"].as_u64().unwrap_or(0) > 0,
                    "the OpenAI client gets real results"
                );
            }
        }

        // The neutral suite only asks for one fetch; the OpenAI suite asks for several,
        // including a miss.
        let fetch_id = if expected_error.is_some() {
            case_id("fetch", None)
        } else {
            case_id("fetch", Some("no-such-note-anywhere"))
        };
        let fetch = payload_of(frame_of(&frames, fetch_id));
        match expected_error {
            Some(_) => {
                assert_eq!(fetch["title"], "Unsupported MCP Client");
                assert_eq!(fetch["metadata"]["error"], "Unsupported MCP client");
            }
            None => {
                // A missing document is still a document, flagged in metadata.
                assert_eq!(fetch["id"], "no-such-note-anywhere");
                assert_eq!(fetch["metadata"]["error"], "Document not found");
            }
        }
    }
}
