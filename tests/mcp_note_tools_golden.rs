//! The note-tool surface, replayed against a captured reference session.
//!
//! `write_note`, `delete_note`, and `move_note` all answer differently per
//! `output_format`, and `delete_note`/`move_note` switch between a single note and a whole
//! directory with `is_directory`. `tools/dump_reference_note_mcp.py` drives all of those
//! branches — including the metadata merge (`note_type`, `tags`), the directory guard, the
//! conflict payload, and the two failure codes — and records the resulting vault files so
//! the filesystem side is compared too.

use std::fs;

use serde_json::Value;
mod common;
use common::{
    Scratch, Session, SessionOutput, canonicalize_uuids, copy_dir_with_mtimes, fixtures_vault,
    frame_of, load_golden_json,
};

fn golden() -> Value {
    load_golden_json("mcp/note-tools.json")
}

#[test]
fn mcp_note_tools_replay_the_reference_session() {
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

    for case in reference["responses"].as_array().expect("responses") {
        if case["id"] == "initialize" {
            continue;
        }
        let id = case["request"]["id"].as_u64().expect("id");
        let ours = frame_of(&frames, id);
        let expected = case["frame"]["result"]["content"][0]["text"]
            .as_str()
            .expect("reference text");
        let actual = ours["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{}: no text result ({ours})", case["id"]));
        // One deliberate divergence, pinned here: a note created through the reference's
        // MCP write path gets *duplicate* `search_index` rows (identical type+id, different
        // rowids — visible in its own workdir DB), so its suggestion lists can repeat a
        // note. This port indexes once, which is the behaviour a caller wants; the lists are
        // therefore compared as sets.
        let actual = canonicalize_uuids(actual);
        let expected = canonicalize_uuids(expected);
        match (
            serde_json::from_str::<Value>(&actual),
            serde_json::from_str::<Value>(&expected),
        ) {
            (Ok(mut ours), Ok(mut theirs))
                if ours.get("related_results").is_some()
                    && theirs.get("related_results").is_some() =>
            {
                for payload in [&mut ours, &mut theirs] {
                    if let Some(rows) = payload["related_results"].as_array_mut() {
                        let mut seen: Vec<String> = Vec::new();
                        rows.retain(|row| {
                            let key = row.to_string();
                            let fresh = !seen.contains(&key);
                            seen.push(key);
                            fresh
                        });
                    }
                }
                // The duplicate also consumes a page slot, so the reference's page can
                // hold one fewer distinct note than ours. Compare the shared prefix: the
                // ordering and the members both still have to agree.
                let (ours_rows, theirs_rows) = (
                    ours["related_results"].as_array().expect("rows").clone(),
                    theirs["related_results"].as_array().expect("rows").clone(),
                );
                assert!(
                    theirs_rows.len() <= ours_rows.len(),
                    "{}: our page must not be shorter",
                    case["id"]
                );
                assert_eq!(
                    &ours_rows[..theirs_rows.len()],
                    theirs_rows.as_slice(),
                    "{} differs",
                    case["id"]
                );
                ours["related_results"] = ours["related_results"].clone();
                theirs["related_results"] = theirs["related_results"].clone();
            }
            _ => assert_eq!(actual, expected, "{} differs", case["id"]),
        }
    }

    // The filesystem side: every note the session touched must hold the same bytes.
    for (relative, expected) in reference["files"].as_object().expect("files") {
        let path = vault.join(relative);
        match expected.as_str() {
            None => assert!(
                !path.exists(),
                "{relative} must not exist after the session"
            ),
            Some(expected) => {
                let actual =
                    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{relative}: {error}"));
                assert_eq!(actual, expected, "{relative} differs");
            }
        }
    }
}
