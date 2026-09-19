//! Phase 10 compatibility: the frontmatter writer against the reference vault.
//!
//! `tests/golden/vault/` is the fixture vault *after* the reference indexed it with
//! `ensure_frontmatter_on_sync=true`: files without frontmatter gained
//! `title`/`type`/`permalink`, files whose frontmatter lacked the resolved permalink
//! gained that key, and every other byte (including the body and its missing trailing
//! newline) is preserved. Replaying the same update through our writer must reproduce
//! those files byte for byte.

use std::fs;
use std::path::Path;

use basic_mem::markdown::serialize::merge_frontmatter;
use basic_mem::markdown::split_frontmatter;
use basic_mem::markdown::{
    EditOperation, EditOptions, apply_edit_operation, merge_metadata_into_markdown,
};
use serde_yaml_ng::Value;
mod common;
use common::repo_root;

fn markdown_files(root: &Path) -> Vec<String> {
    basic_mem::indexing::document::markdown_files(root)
        .into_iter()
        .map(|(relative, _)| relative)
        .collect()
}

/// The frontmatter updates the reference batch indexer applies to one file.
///
/// `None` means the reference leaves the file alone: it already carries the resolved
/// permalink, or its frontmatter is malformed (the batch indexer skips such files).
fn reference_updates(relative: &str, content: &str) -> Option<Vec<(String, Value)>> {
    let path = relative.trim_end_matches(".md");
    let permalink = format!("oracle/{path}");
    let stem = Path::new(relative)
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    match split_frontmatter(content) {
        // No frontmatter: the indexer injects title, type and the resolved permalink.
        Ok(None) => Some(vec![
            ("title".to_owned(), Value::String(stem)),
            ("type".to_owned(), Value::String("note".to_owned())),
            ("permalink".to_owned(), Value::String(permalink)),
        ]),
        Err(_) => None,
        Ok(Some((mapping, _))) => {
            let existing = mapping
                .get(Value::String("permalink".to_owned()))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if existing.as_deref() == Some(permalink.as_str()) {
                None
            } else {
                // An explicit permalink stays authoritative; only a missing one is added.
                match existing {
                    Some(_) => None,
                    None => Some(vec![("permalink".to_owned(), Value::String(permalink))]),
                }
            }
        }
    }
}

#[test]
fn frontmatter_writer_matches_the_reference_vault() {
    let fixtures = repo_root().join("tests/fixtures/vault");
    let golden = repo_root().join("tests/golden/vault");
    assert!(golden.is_dir(), "run tools/export_reference.py first");

    let mut mismatches = Vec::new();
    let mut compared = 0;
    for relative in markdown_files(&fixtures) {
        let expected = match fs::read_to_string(golden.join(&relative)) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let source = fs::read_to_string(fixtures.join(&relative)).expect("fixture");
        compared += 1;
        let Some(updates) = reference_updates(&relative, &source) else {
            // Nothing to write: the reference leaves these bytes untouched.
            if source != expected {
                mismatches.push(format!("{relative}: reference must not rewrite this file"));
            }
            continue;
        };
        let actual = merge_frontmatter(&source, &updates).expect("merge");
        if actual != expected {
            mismatches.push(format!(
                "{relative}\n  expected tail: {:?}\n  actual   tail: {:?}",
                expected
                    .chars()
                    .rev()
                    .take(60)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>(),
                actual
                    .chars()
                    .rev()
                    .take(60)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>(),
            ));
        }
    }

    assert!(compared > 0, "no golden vault files compared");
    assert!(
        mismatches.is_empty(),
        "{} of {compared} files differ:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn writer_keeps_the_body_bytes_untouched() {
    let source = "---\ntitle: Keep\ntype: note\n---\n\nBody line one.\n\n- [note] keep me\n";
    let merged = merge_frontmatter(
        source,
        &[(
            "permalink".to_owned(),
            Value::String("oracle/keep".to_owned()),
        )],
    )
    .expect("merge");
    assert!(merged.ends_with("- [note] keep me"), "body keeps its bytes");
    assert!(!merged.ends_with('\n'), "trailing newline is stripped");
    assert!(merged.contains("\n---\n\nBody line one."));
}

/// Replay the captured reference edit table (22 cases, including error messages).
#[test]
fn edit_operations_match_the_reference_table() {
    let raw = fs::read_to_string(repo_root().join("tests/golden/note/edit-operations.json"))
        .expect("edit golden (run tools/dump_reference_edits.py)");
    // Parsed as YAML so mapping key order survives (JSON is a YAML subset, and the
    // reference emits merged frontmatter in insertion order).
    let golden: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw).expect("golden yaml");
    let cases = golden["cases"].as_sequence().expect("cases");
    assert!(!cases.is_empty(), "golden table is empty");

    let mut mismatches = Vec::new();
    for case in cases {
        let id = case["id"].as_str().unwrap_or_default();
        let content = case["content"].as_str().unwrap_or_default();
        let operation = case["operation"].as_str().unwrap_or_default();
        let payload = case["payload"].as_str().unwrap_or_default();
        let kwargs = &case["kwargs"];

        let result = if operation == "metadata_merge" {
            let metadata = yaml_metadata(&kwargs["metadata"]);
            merge_metadata_into_markdown(content, &metadata)
        } else {
            EditOperation::parse(operation).and_then(|operation| {
                let mut options = EditOptions::new();
                options.section = kwargs["section"].as_str().map(str::to_owned);
                options.find_text = kwargs["find_text"].as_str().map(str::to_owned);
                if let Some(expected) = kwargs["expected_replacements"].as_u64() {
                    options.expected_replacements = expected as usize;
                }
                if let Some(replace) = kwargs["replace_subsections"].as_bool() {
                    options.replace_subsections = replace;
                }
                apply_edit_operation(content, operation, payload, &options)
            })
        };

        match (case.get("result"), &case.get("error")) {
            (_, Some(expected_error)) => {
                let expected = expected_error
                    .as_str()
                    .unwrap_or_default()
                    .trim_start_matches("ValueError: ");
                match result {
                    Ok(actual) => mismatches
                        .push(format!("{id}: expected error {expected:?}, got {actual:?}")),
                    Err(error) => {
                        if error.to_string() != expected {
                            mismatches.push(format!("{id}: error {error} != {expected}"));
                        }
                    }
                }
            }
            (Some(expected), None) => {
                let expected = expected.as_str().unwrap_or_default();
                match result {
                    Ok(actual) if actual == expected => {}
                    Ok(actual) => mismatches.push(format!(
                        "{id}:\n  expected {expected:?}\n  actual   {actual:?}"
                    )),
                    Err(error) => mismatches.push(format!("{id}: unexpected error {error}")),
                }
            }
            (None, None) => mismatches.push(format!("{id}: golden has no result")),
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} of {} edit cases differ:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n")
    );
}

/// Convert golden metadata into the YAML values the writer consumes.
fn yaml_metadata(value: &serde_yaml_ng::Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    if let Some(map) = value.as_mapping() {
        for (key, value) in map {
            let Some(key) = key.as_str() else {
                continue;
            };
            out.push((key.to_owned(), value.clone()));
        }
    }
    out
}
