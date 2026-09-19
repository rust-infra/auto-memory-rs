//! CLI wiring: `context` flags, output modes, and reference error surfaces.
//!
//! The traversal order depends on the reference's id assignment (see
//! `tests/context_golden.rs`), so the plain outline is compared as a line set and
//! the JSON payload through its metadata.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use basic_mem::indexing::{RebuildOptions, rebuild_vault};
use basic_mem::storage::Store;
use common::{Scratch, canonicalize_text, copy_dir, fixtures_vault, repo_root};
use serde_json::Value;

/// Build a throwaway vault + index the CLI can point at.
fn fixture_index() -> (Scratch, Scratch, PathBuf) {
    let vault = Scratch::new("cli-vault");
    copy_dir(&fixtures_vault(), vault.path());
    let index_dir = Scratch::new("cli-index");
    let index = index_dir.join("memory.db");
    let mut store = Store::open(&index).expect("store");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.path().to_string_lossy())
        .expect("project");
    rebuild_vault(
        &mut store,
        project_id,
        vault.path(),
        &RebuildOptions::new("oracle"),
    )
    .expect("rebuild");
    (vault, index_dir, index)
}

fn run(index: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_basic-mem"))
        .args(["context"])
        .args(args)
        .args(["--index", &index.to_string_lossy(), "--project", "oracle"])
        .output()
        .expect("run basic-mem")
}

/// Run `basic-mem` with a full argument list.
fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_basic-mem"))
        .args(args)
        // Pin the log filter: assertions on stderr must not depend on the developer's
        // own RUST_LOG.
        .env("RUST_LOG", "basic_mem=info")
        .output()
        .expect("run basic-mem")
}

#[test]
fn watch_once_syncs_a_changed_vault() {
    let (vault, _index_dir, index) = fixture_index();
    let index_arg = index.to_string_lossy().into_owned();
    let vault_arg = vault.path().to_string_lossy().into_owned();

    fs::write(
        vault.join("notes/simple.md"),
        "# Simple Note\n\n- [note] edited while watching\n",
    )
    .expect("write");
    let output = run_cli(&[
        "watch",
        "--vault",
        &vault_arg,
        "--index",
        &index_arg,
        "--project",
        "oracle",
        "--once",
        "--window-ms",
        "150",
    ]);
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    let report: Value = serde_json::from_slice(&output.stdout).expect("report");
    assert_eq!(
        report["reconciled"]["updated"], 1,
        "the edited note must be re-indexed: {report}"
    );
    // Logs are the operator's view of the same run; they must be on stderr, so the
    // JSON report above is the only thing a script has to parse. (The one-shot batch
    // itself is empty here — the initial reconcile already took the edit.)
    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in ["watching the vault", "initial reconcile finished"] {
        assert!(
            stderr.contains(expected),
            "missing {expected:?} in watch stderr: {stderr}"
        );
    }
    let _ = fs::remove_file(&index);
}

#[test]
fn watch_rejects_embeddings_instead_of_ignoring_it() {
    // `--embeddings` lives in the shared switch list, so `watch` parses it and then
    // has nothing to do with it. Exiting 2 with the alternative beats a run that looks
    // like it refreshed vectors and did not. The paths deliberately do not exist: the
    // guard has to fire before anything is opened.
    let scratch = Scratch::new("watch-flag");
    let vault = scratch.path().join("vault");
    let index = scratch.path().join("memory.db");
    let output = run_cli(&[
        "watch",
        "--vault",
        &vault.to_string_lossy(),
        "--index",
        &index.to_string_lossy(),
        "--project",
        "oracle",
        "--embeddings",
        "--once",
    ]);
    assert_eq!(output.status.code(), Some(2), "stderr: {:?}", output.stderr);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--embeddings"), "stderr: {stderr}");
    assert!(stderr.contains("reindex"), "stderr: {stderr}");
    assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
}

#[test]
fn semantic_search_cli_replays_reference_results() {
    let (vault, _index_dir, index) = fixture_index();
    let index_arg = index.to_string_lossy().into_owned();
    let vault_arg = vault.path().to_string_lossy().into_owned();
    let fixture = repo_root().join("tests/golden/vector/embeddings-reference.json");
    let fixture_arg = fixture.to_string_lossy().into_owned();

    // `reindex --embeddings` fills the vector index from the captured reference
    // vectors (an offline stand-in for the ONNX provider).
    let output = run_cli(&[
        "reindex",
        "--vault",
        &vault_arg,
        "--index",
        &index_arg,
        "--project",
        "oracle",
        "--embeddings",
        "--embedding-fixture",
        &fixture_arg,
    ]);
    assert!(output.status.success(), "stderr: {:?}", output.stderr);
    let report: Value = serde_json::from_slice(&output.stdout).expect("report");
    assert_eq!(report["chunks"], 78);
    assert_eq!(report["embedded"], 78);

    // The semantic score envelope. The reference runtime is not bit-reproducible across
    // runs (ONNX Runtime batching/threading): re-capturing the corpus moved the same
    // ranks by up to 1.21e-4, above the 1e-4 the first capture needed. Rankings and
    // `matched_chunk` stay exact — those are the contract; the score is a float that two
    // reference runs do not agree on.
    const SCORE_TOLERANCE: f64 = 5e-4;
    for (flags, query, golden_name) in [
        (vec!["--vector"], "local index", "vector-local-index"),
        (vec!["--hybrid"], "rust", "hybrid-rust"),
        (
            vec!["--vector", "--type", "note"],
            "rust",
            "vector-rust-type-note",
        ),
        (
            vec!["--vector", "--type", "project"],
            "rust",
            "vector-rust-type-project",
        ),
        (
            vec!["--hybrid", "--type", "project"],
            "rust",
            "hybrid-rust-type-project",
        ),
        (
            vec!["--vector", "--entity-type", "observation"],
            "rust",
            "vector-rust-entity-observation",
        ),
        (
            vec!["--vector", "--entity-type", "entity"],
            "local index",
            "vector-local-index-entity-only",
        ),
        (
            vec!["--vector", "--tag", "rust"],
            "rust",
            "vector-rust-tag-rust",
        ),
        (
            vec!["--vector", "--status", "active"],
            "rust",
            "vector-rust-status-active",
        ),
        (
            vec!["--hybrid", "--category", "decision"],
            "rust",
            "hybrid-rust-category-decision",
        ),
    ] {
        let mut args = vec!["search", "--index", &index_arg, "--project", "oracle"];
        args.extend(flags.iter().copied());
        args.extend(["--embedding-fixture", &fixture_arg]);
        args.push(query);
        let output = run_cli(&args);
        assert!(output.status.success(), "stderr: {:?}", output.stderr);
        let page: Value = serde_json::from_slice(&output.stdout).expect("results");
        let golden: Value = serde_json::from_str(
            &fs::read_to_string(
                repo_root()
                    .join("tests/golden/search")
                    .join(format!("{golden_name}.json")),
            )
            .expect("golden"),
        )
        .expect("json");
        // Semantic totals stay inexact, and whether more results exist depends on how
        // many candidates survived the filters, so both come from the capture.
        assert_eq!(page["total"], golden["total"], "{golden_name}: total");
        assert_eq!(
            page["total_is_exact"], golden["total_is_exact"],
            "{golden_name}: total_is_exact"
        );
        assert_eq!(
            page["has_more"], golden["has_more"],
            "{golden_name}: has_more"
        );
        let actual = page["results"].as_array().expect("results");
        let expected = golden["results"].as_array().expect("results");
        assert_eq!(actual.len(), expected.len(), "{golden_name}: result count");
        for (index, (result, entry)) in actual.iter().zip(expected).enumerate() {
            assert_eq!(
                result["permalink"], entry["permalink"],
                "{golden_name}: rank {index} permalink"
            );
            let difference = (result["score"].as_f64().expect("score")
                - entry["score"].as_f64().expect("score"))
            .abs();
            assert!(
                difference < SCORE_TOLERANCE,
                "{golden_name}: rank {index} score drift {difference}"
            );
            if let Some(matched) = entry["matched_chunk"].as_str() {
                assert_eq!(
                    result["matched_chunk"].as_str(),
                    Some(matched),
                    "{golden_name}: rank {index} matched_chunk"
                );
            }
        }
    }
    let _ = fs::remove_file(&index);
}

#[test]
fn search_accepts_a_title_filter_beside_a_query() {
    let (_vault, _index_dir, index) = fixture_index();
    let index_arg = index.to_string_lossy().into_owned();
    let output = run_cli(&[
        "search",
        "--index",
        &index_arg,
        "--project",
        "oracle",
        "--title",
        "Alpha",
        "rust",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let page: Value = serde_json::from_slice(&output.stdout).expect("results");
    assert_eq!(page["total"], 1, "{page}");
    assert_eq!(page["results"][0]["permalink"], "oracle/projects/alpha");
}

#[test]
fn context_plain_outline_matches_the_reference_line_set() {
    let (_vault, _index_dir, index) = fixture_index();
    let output = run(&index, &["memory://notes/simple", "--plain"]);
    assert!(output.status.success(), "exit status: {:?}", output.status);
    assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);

    let mut actual: Vec<String> = canonicalize_text(&String::from_utf8_lossy(&output.stdout))
        .lines()
        .map(str::to_owned)
        .collect();
    let mut expected: Vec<String> = canonicalize_text(
        &fs::read_to_string(repo_root().join("tests/golden/context/simple-depth1.txt"))
            .expect("golden"),
    )
    .lines()
    .map(str::to_owned)
    .collect();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected, "plain outline differs");
    let _ = fs::remove_file(&index);
}

#[test]
fn context_json_matches_the_reference_metadata() {
    let (_vault, _index_dir, index) = fixture_index();
    let output = run(
        &index,
        &["memory://notes/relations", "--json", "--depth", "2"],
    );
    assert!(output.status.success(), "exit status: {:?}", output.status);

    let actual: Value = serde_json::from_slice(&output.stdout).expect("json output");
    let expected: Value = serde_json::from_str(
        &fs::read_to_string(repo_root().join("tests/golden/context/relations-depth2.json"))
            .expect("golden"),
    )
    .expect("json");
    for key in [
        "uri",
        "depth",
        "primary_count",
        "related_count",
        "total_results",
        "total_relations",
        "total_observations",
    ] {
        assert_eq!(
            actual["metadata"][key], expected["metadata"][key],
            "metadata.{key}"
        );
    }
    assert_eq!(actual["page_size"], 10);
    assert_eq!(actual["has_more"], false);
    let _ = fs::remove_file(&index);
}

#[test]
fn context_reports_reference_arguments_errors() {
    let (_vault, _index_dir, index) = fixture_index();
    for (flags, expected) in [
        (
            vec!["memory://notes/simple", "--page-size", "0", "--json"],
            "Error: page_size must be >= 1, got 0\n",
        ),
        (
            vec!["memory://notes/simple", "--page", "0", "--json"],
            "Error: page must be >= 1, got 0\n",
        ),
    ] {
        let output = run(&index, &flags);
        assert_eq!(output.status.code(), Some(1), "flags: {flags:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            expected,
            "flags: {flags:?}"
        );
        assert!(output.stdout.is_empty(), "flags: {flags:?}");
    }
    let _ = fs::remove_file(&index);
}
