//! Parse-layer compatibility: compare the Rust parser against the reference
//! parser output captured in `tests/golden/parse/reference-parse.json`.
//!
//! The reference artifact is produced by `tools/dump_reference_parse.py` run with
//! the Basic Memory 0.23.2 interpreter. It captures the parse layer *before* the
//! indexer rewrites files (frontmatter injection, permalink resolution).

use std::collections::BTreeMap;
use std::fs;

use basic_mem::indexing::document::markdown_files;
use serde_json::Value;
mod common;
use common::{fixtures_vault, repo_root};

fn reference_parse() -> BTreeMap<String, Value> {
    let path = repo_root().join("tests/golden/parse/reference-parse.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "missing {}: {err}. Regenerate with `python3 tools/dump_reference_parse.py`.",
            path.display()
        )
    });
    serde_json::from_str(&raw).expect("reference-parse.json must be valid JSON")
}

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn text(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    }
}

#[test]
fn rust_parser_matches_reference_parse_golden() {
    let reference = reference_parse();
    let files = markdown_files(&fixtures_vault());
    assert!(!files.is_empty(), "fixture vault is empty");

    let mut mismatches = Vec::new();
    for (rel, path) in files {
        let Some(expected) = reference.get(&rel) else {
            mismatches.push(format!("{rel}: missing from reference-parse.json"));
            continue;
        };
        let content = fs::read_to_string(&path).expect("read fixture");
        let actual = basic_mem::markdown::parse_document(&rel, &content).expect("parse fixture");
        let actual = serde_json::to_value(&actual).expect("serialize parse result");

        let actual_frontmatter = actual.get("frontmatter");
        compare_field(
            &rel,
            "title",
            expected.get("title"),
            actual_frontmatter.and_then(|f| f.get("title")),
            &mut mismatches,
        );
        compare_field(
            &rel,
            "type",
            expected.get("type"),
            actual_frontmatter.and_then(|f| f.get("type")),
            &mut mismatches,
        );
        compare_field(
            &rel,
            "permalink",
            expected.get("permalink"),
            actual_frontmatter.and_then(|f| f.get("permalink")),
            &mut mismatches,
        );
        compare_strings(
            &rel,
            "tags",
            expected.get("tags"),
            actual_frontmatter.and_then(|f| f.get("tags")),
            &mut mismatches,
        );
        compare_field(
            &rel,
            "metadata",
            expected.get("metadata"),
            actual_frontmatter.and_then(|f| f.get("metadata")),
            &mut mismatches,
        );
        compare_field(
            &rel,
            "content",
            expected.get("content"),
            actual.get("content"),
            &mut mismatches,
        );
        compare_observations(
            &rel,
            expected.get("observations"),
            actual.get("observations"),
            &mut mismatches,
        );
        compare_relations(
            &rel,
            expected.get("relations"),
            actual.get("relations"),
            &mut mismatches,
        );
    }

    assert!(
        mismatches.is_empty(),
        "{} parse mismatch(es):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

fn compare_field(
    rel: &str,
    field: &str,
    expected: Option<&Value>,
    actual: Option<&Value>,
    out: &mut Vec<String>,
) {
    if expected != actual {
        out.push(format!(
            "{rel}: {field}\n  expected: {}\n  actual:   {}",
            expected.map_or("null".to_owned(), Value::to_string),
            actual.map_or("null".to_owned(), Value::to_string)
        ));
    }
}

fn compare_strings(
    rel: &str,
    field: &str,
    expected: Option<&Value>,
    actual: Option<&Value>,
    out: &mut Vec<String>,
) {
    let expected = strings(expected);
    let actual = strings(actual);
    if expected != actual {
        out.push(format!(
            "{rel}: {field} expected {expected:?}, actual {actual:?}"
        ));
    }
}

fn compare_observations(
    rel: &str,
    expected: Option<&Value>,
    actual: Option<&Value>,
    out: &mut Vec<String>,
) {
    let expected = expected
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let actual = actual
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if expected.len() != actual.len() {
        out.push(format!(
            "{rel}: observations count expected {}, actual {}",
            expected.len(),
            actual.len()
        ));
    }
    for (index, (expected, actual)) in expected.iter().zip(actual.iter()).enumerate() {
        compare_field(
            rel,
            &format!("observations[{index}].category"),
            expected.get("category"),
            actual.get("category"),
            out,
        );
        compare_field(
            rel,
            &format!("observations[{index}].content"),
            expected.get("content"),
            actual.get("content"),
            out,
        );
        compare_field(
            rel,
            &format!("observations[{index}].context"),
            expected.get("context"),
            actual.get("context"),
            out,
        );
        compare_strings(
            rel,
            &format!("observations[{index}].tags"),
            expected.get("tags"),
            actual.get("tags"),
            out,
        );
    }
}

fn compare_relations(
    rel: &str,
    expected: Option<&Value>,
    actual: Option<&Value>,
    out: &mut Vec<String>,
) {
    let expected = expected
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let actual = actual
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if expected.len() != actual.len() {
        out.push(format!(
            "{rel}: relations count expected {}, actual {}",
            expected.len(),
            actual.len()
        ));
    }
    for (index, (expected, actual)) in expected.iter().zip(actual.iter()).enumerate() {
        compare_field(
            rel,
            &format!("relations[{index}].type"),
            expected.get("type"),
            actual.get("type"),
            out,
        );
        compare_field(
            rel,
            &format!("relations[{index}].target"),
            expected.get("target"),
            actual.get("target"),
            out,
        );
        compare_field(
            rel,
            &format!("relations[{index}].context"),
            expected.get("context"),
            actual.get("context"),
            out,
        );
    }
}

#[test]
fn reference_parse_golden_covers_all_fixture_files() {
    let reference = reference_parse();
    let files = markdown_files(&fixtures_vault());
    let missing: Vec<_> = files
        .iter()
        .map(|(rel, _)| rel.clone())
        .filter(|rel| !reference.contains_key(rel))
        .collect();
    assert!(
        missing.is_empty(),
        "fixtures missing from golden: {missing:?}"
    );
    assert_eq!(text(None), None);
    assert_eq!(strings(None), Vec::<String>::new());
    let _ = fixtures_vault();
}
