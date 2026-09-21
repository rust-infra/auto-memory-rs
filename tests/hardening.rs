//! Phase 15: hardening — path containment, malformed input, and round-trip invariants.
//!
//! The MCP server takes identifiers and paths straight from a model, so the vault root has
//! to be a real boundary: nothing a caller spells may read, write, move, or delete a file
//! outside it. The indexer additionally has to survive files it cannot decode.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use auto_memory::application::note::NoteMetadata;

mod common;
use common::{Scratch, Session, copy_dir, repo_root};

/// Run one scripted MCP session against `vault` and return its frames.
fn run_session(vault: &Path, requests: &[Value]) -> Vec<Value> {
    let index = vault.parent().expect("parent").join("memory.db");
    Session::new(vault, index).run(requests).frames
}

/// The user-visible outcome of one call: its text payload, or the error message.
///
/// `read_note` answers a miss with guidance text, while the mutation tools surface an
/// unresolvable identifier as a JSON-RPC error. Both are fine for a containment test —
/// what matters is that neither leaks an outside file.
fn outcome(frames: &[Value], id: u64) -> String {
    let frame = frames
        .iter()
        .find(|frame| frame["id"] == id)
        .expect("frame");
    if let Some(error) = frame.get("error") {
        return error["message"].as_str().unwrap_or_default().to_owned();
    }
    frame["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
           "params": {"name": name, "arguments": arguments}})
}

/// Phase 15: identifiers and paths may never leave the vault.
#[tokio::test(flavor = "multi_thread")]
async fn mcp_paths_cannot_escape_the_vault() {
    let dir = Scratch::new("traversal");
    let vault = dir.join("vault");
    copy_dir(&repo_root().join("tests/fixtures/vault"), &vault);
    // A file outside the vault that a traversal would reach.
    let outside = dir.join("outside");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(
        outside.join("secret.md"),
        "# Secret\n\n- [fact] not yours\n",
    )
    .expect("secret");

    let frames = run_session(
        &vault,
        &[
            call(1, "read_content", json!({"path": "../outside/secret.md"})),
            call(
                2,
                "read_note",
                json!({"identifier": "../outside/secret.md"}),
            ),
            call(
                3,
                "edit_note",
                json!({"identifier": "../outside/secret.md", "operation": "append",
                       "content": "- [fact] rewritten"}),
            ),
            call(
                4,
                "delete_note",
                json!({"identifier": "../outside/secret.md"}),
            ),
            call(
                5,
                "write_note",
                json!({"title": "escaped", "content": "# Escaped", "directory": "../outside"}),
            ),
            call(
                6,
                "move_note",
                json!({"identifier": "notes/simple", "destination_path": "../outside/moved.md"}),
            ),
        ],
    );

    // Reads must not leak the file, and the write paths must fail.
    for id in 1..=6 {
        let text = outcome(&frames, id);
        assert!(
            !text.contains("not yours"),
            "request {id} leaked content outside the vault: {text}"
        );
    }
    for id in [5, 6] {
        let text = outcome(&frames, id);
        assert!(
            text.starts_with("# Error") || text.contains("invalid path"),
            "request {id} must refuse the traversal: {text}"
        );
    }
    // `write_note` uses the reference's structured refusal, not a JSON-RPC error.
    assert!(
        outcome(&frames, 5).contains("paths must stay within project boundaries"),
        "{}",
        outcome(&frames, 5)
    );

    assert!(
        outside.join("secret.md").exists(),
        "an outside file must survive every call"
    );
    assert_eq!(
        fs::read_to_string(outside.join("secret.md")).expect("read"),
        "# Secret\n\n- [fact] not yours\n",
        "an outside file must not be edited"
    );
    assert!(
        !outside.join("escaped.md").exists(),
        "write_note must not create files outside the vault"
    );
    assert!(
        !outside.join("moved.md").exists(),
        "move_note must not move files outside the vault"
    );
    assert!(
        vault.join("notes/simple.md").exists(),
        "the refused move leaves the note in place"
    );
}

/// An absolute-looking directory is refused, not silently re-rooted.
///
/// The reference validates `directory` against the project boundary and answers with its
/// `SECURITY_VALIDATION_ERROR` payload; `/etc/cron.d` must not create anything, inside the
/// vault or outside it.
#[tokio::test(flavor = "multi_thread")]
async fn mcp_absolute_directory_paths_are_refused() {
    let dir = Scratch::new("absolute");
    let vault = dir.join("vault");
    copy_dir(&repo_root().join("tests/fixtures/vault"), &vault);

    let frames = run_session(
        &vault,
        &[
            call(
                1,
                "write_note",
                json!({"title": "Absolute", "content": "# Absolute", "directory": "/etc/cron.d"}),
            ),
            call(
                2,
                "write_note",
                json!({"title": "Absolute", "content": "# Absolute",
                       "directory": "/etc/cron.d", "output_format": "json"}),
            ),
            // `"/"` is the project root, and that one is allowed.
            call(
                3,
                "write_note",
                json!({"title": "At Root", "content": "# At Root", "directory": "/"}),
            ),
        ],
    );

    let refusal = outcome(&frames, 1);
    assert!(
        refusal.starts_with("# Error") && refusal.contains("is not allowed"),
        "{refusal}"
    );
    let payload: Value = serde_json::from_str(&outcome(&frames, 2)).expect("payload");
    assert_eq!(payload["error"], "SECURITY_VALIDATION_ERROR");
    assert_eq!(payload["file_path"], Value::Null);

    assert!(
        !vault.join("etc").exists(),
        "nothing may be written under an absolute directory"
    );
    assert!(!Path::new("/etc/cron.d/Absolute.md").exists());
    assert!(
        vault.join("At Root.md").exists(),
        "`/` means the project root"
    );
}

/// Phase 15: a file the indexer cannot decode must not take the rest of the vault down.
#[tokio::test(flavor = "multi_thread")]
async fn malformed_utf8_file_is_skipped_without_losing_the_vault() {
    let dir = Scratch::new("utf8");
    let vault = dir.join("vault");
    copy_dir(&repo_root().join("tests/fixtures/vault"), &vault);
    fs::write(
        vault.join("notes/binary.md"),
        [0xff, 0xfe, 0x00, 0x9f, 0x92, 0x96],
    )
    .expect("write");

    let index = dir.join("memory.db");
    let output = Command::new(env!("CARGO_BIN_EXE_auto-memory"))
        .args(["reindex", "--vault"])
        .arg(&vault)
        .args(["--index"])
        .arg(&index)
        .args(["--project", "oracle"])
        .output()
        .expect("reindex");
    assert!(
        output.status.success(),
        "reindex must not fail the whole run"
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("report");
    // The fixture vault already ships one malformed-frontmatter note, so the undecodable
    // file is the second skip.
    assert_eq!(report["skipped"], 2, "{report}");
    assert!(report["added"].as_u64().unwrap_or(0) > 0, "{report}");

    let store = auto_memory::storage::Store::open(&index)
        .await
        .expect("store");
    let project = store
        .project_by_permalink("oracle")
        .await
        .expect("lookup")
        .expect("project");
    assert!(
        store
            .entity_by_file_path(project.id, "notes/binary.md")
            .await
            .expect("lookup")
            .is_none(),
        "an undecodable file is not indexed"
    );
    assert!(
        store
            .entity_by_file_path(project.id, "notes/simple.md")
            .await
            .expect("lookup")
            .is_some(),
        "its neighbours are indexed normally"
    );
}

/// Phase 15: writing a note twice is a fixed point, and reading it back is stable.
///
/// This is the property the whole edit path leans on: the frontmatter writer must emit
/// something the parser reads back identically, for arbitrary bodies and metadata.
#[tokio::test(flavor = "multi_thread")]
async fn writing_and_reparsing_notes_is_a_fixed_point() {
    let dir = Scratch::new("roundtrip");
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    let index = dir.join("memory.db");
    let mut store = auto_memory::storage::Store::open(&index)
        .await
        .expect("store");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .await
        .expect("project");

    // Deterministic pseudo-random bodies: a small LCG keeps this dependency-free.
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as usize
    };
    let alphabet = [
        "a",
        "note",
        "文字",
        "  ",
        "\n",
        "- [fact] x",
        "# H",
        ":",
        "'",
        "\"",
        "[",
        "]",
        "\\",
        "%",
        "  - nested",
        "\t",
    ];

    for round in 0..24 {
        let mut body = String::from("# Generated\n\n");
        for _ in 0..next() % 12 {
            body.push_str(alphabet[next() % alphabet.len()]);
        }
        let note =
            auto_memory::markdown::parse_document("notes/generated.md", &body).expect("parse");
        {
            let mut service = auto_memory::application::note::NoteService::new(
                &mut store,
                project_id,
                &vault,
                auto_memory::indexing::IndexOptions::new("oracle"),
            );
            let written = service
                .write_note(
                    "notes/generated.md",
                    &note.content,
                    &NoteMetadata::default(),
                    true,
                )
                .await;
            assert!(written.is_ok(), "round {round}: {written:?}");
        }

        let once = fs::read_to_string(vault.join("notes/generated.md")).expect("read");
        let reparsed =
            auto_memory::markdown::parse_document("notes/generated.md", &once).expect("reparse");
        {
            let mut service = auto_memory::application::note::NoteService::new(
                &mut store,
                project_id,
                &vault,
                auto_memory::indexing::IndexOptions::new("oracle"),
            );
            let rewritten = service
                .write_note(
                    "notes/generated.md",
                    &reparsed.content,
                    &NoteMetadata::default(),
                    true,
                )
                .await
                .expect("rewrite");
            assert_eq!(rewritten.file_path, "notes/generated.md");
        }
        let twice = fs::read_to_string(vault.join("notes/generated.md")).expect("read");
        assert_eq!(
            once, twice,
            "round {round}: writing twice changed the bytes"
        );
    }
}
