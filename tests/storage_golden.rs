//! Storage-layer compatibility: rebuild the index from the fixture vault and
//! compare its projection with the reference artifacts in `tests/golden/parse/`.
//!
//! Compared fields are the ones the storage layer owns today: file path, title,
//! note type, permalink (including the project prefix), observations, and the raw
//! relation target. Checksums and metadata match the reference only after the
//! frontmatter-on-sync rewrite, which is a later phase.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;
mod common;
use common::{Scratch, fixtures_vault, load_golden_json};

fn golden(name: &str) -> Vec<Value> {
    load_golden_json(&format!("parse/{name}"))
        .as_array()
        .cloned()
        .expect("golden array")
}

fn rebuild(
    store: &mut basic_mem::storage::Store,
    project_id: i64,
) -> basic_mem::indexing::RebuildReport {
    let options = basic_mem::indexing::RebuildOptions::new("oracle");
    basic_mem::indexing::rebuild_vault(store, project_id, &fixtures_vault(), &options)
        .expect("rebuild")
}

fn project(store: &basic_mem::storage::Store) -> i64 {
    store
        .upsert_project("oracle", "oracle", &fixtures_vault().to_string_lossy())
        .expect("project")
}

fn entity_key(entity: &basic_mem::storage::EntityRow) -> String {
    format!(
        "{}|{}|{}|{}",
        entity.file_path,
        entity.title,
        entity.note_type,
        entity.permalink.as_deref().unwrap_or("")
    )
}

fn observation_keys(store: &basic_mem::storage::Store, project_id: i64) -> Vec<String> {
    let entities: BTreeMap<i64, String> = store
        .entities(project_id)
        .expect("entities")
        .into_iter()
        .map(|entity| (entity.id, entity.permalink.unwrap_or(entity.file_path)))
        .collect();
    let mut keys: Vec<String> = store
        .observations(project_id)
        .expect("observations")
        .into_iter()
        .map(|observation| {
            let owner = entities
                .get(&observation.entity_id)
                .cloned()
                .unwrap_or_default();
            format!(
                "{}|{}|{}|{}|{}",
                owner,
                observation.category,
                observation.content,
                observation.context.unwrap_or_default(),
                observation.tags.join(",")
            )
        })
        .collect();
    keys.sort();
    keys
}

fn relation_keys(store: &basic_mem::storage::Store, project_id: i64) -> Vec<String> {
    let entities: BTreeMap<i64, String> = store
        .entities(project_id)
        .expect("entities")
        .into_iter()
        .map(|entity| (entity.id, entity.permalink.unwrap_or(entity.file_path)))
        .collect();
    let mut keys: Vec<String> = store
        .relations(project_id)
        .expect("relations")
        .into_iter()
        .map(|relation| {
            let owner = entities.get(&relation.from_id).cloned().unwrap_or_default();
            format!(
                "{}|{}|{}|{}",
                owner,
                relation.relation_type,
                relation.to_name,
                relation.context.unwrap_or_default()
            )
        })
        .collect();
    keys.sort();
    keys
}

#[test]
fn rebuild_matches_reference_projection() {
    let mut store = basic_mem::storage::Store::open_in_memory().expect("store");
    let project_id = project(&store);
    let report = rebuild(&mut store, project_id);

    let expected_entities = golden("entities.json");
    assert_eq!(report.files_seen, 16, "fixture vault has 16 markdown files");
    assert_eq!(
        report.documents_skipped, 1,
        "malformed frontmatter file must be skipped, like the reference indexer"
    );
    assert_eq!(report.documents_indexed, expected_entities.len());

    let mut expected: Vec<String> = expected_entities
        .iter()
        .map(|entity| {
            format!(
                "{}|{}|{}|{}",
                entity["file_path"].as_str().unwrap_or_default(),
                entity["title"].as_str().unwrap_or_default(),
                entity["note_type"].as_str().unwrap_or_default(),
                entity["permalink"].as_str().unwrap_or_default()
            )
        })
        .collect();
    expected.sort();
    let mut actual: Vec<String> = store
        .entities(project_id)
        .expect("entities")
        .iter()
        .map(entity_key)
        .collect();
    actual.sort();
    assert_eq!(actual, expected, "entity projection mismatch");

    let mut expected_observations: Vec<String> = golden("observations.json")
        .iter()
        .map(|observation| {
            let tags: Vec<String> = observation["tags"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            format!(
                "{}|{}|{}|{}|{}",
                observation["entity_permalink"].as_str().unwrap_or_default(),
                observation["category"].as_str().unwrap_or_default(),
                observation["content"].as_str().unwrap_or_default(),
                observation["context"].as_str().unwrap_or_default(),
                tags.join(",")
            )
        })
        .collect();
    expected_observations.sort();
    assert_eq!(
        observation_keys(&store, project_id),
        expected_observations,
        "observation projection mismatch"
    );

    let mut expected_relations: Vec<String> = golden("relations.json")
        .iter()
        .map(|relation| {
            format!(
                "{}|{}|{}|{}",
                relation["from_permalink"].as_str().unwrap_or_default(),
                relation["relation_type"].as_str().unwrap_or_default(),
                relation["to_name"].as_str().unwrap_or_default(),
                relation["context"].as_str().unwrap_or_default()
            )
        })
        .collect();
    expected_relations.sort();
    assert_eq!(
        relation_keys(&store, project_id),
        expected_relations,
        "relation projection mismatch"
    );
}

#[test]
fn rebuild_is_idempotent() {
    let mut store = basic_mem::storage::Store::open_in_memory().expect("store");
    let project_id = project(&store);
    rebuild(&mut store, project_id);
    let first_counts = store.counts(project_id).expect("counts");
    let first_observations = observation_keys(&store, project_id);
    let first_relations = relation_keys(&store, project_id);
    let first_ids: Vec<String> = store
        .entities(project_id)
        .expect("entities")
        .into_iter()
        .map(|entity| entity.external_id)
        .collect();

    rebuild(&mut store, project_id);

    assert_eq!(store.counts(project_id).expect("counts"), first_counts);
    assert_eq!(observation_keys(&store, project_id), first_observations);
    assert_eq!(relation_keys(&store, project_id), first_relations);
    let second_ids: Vec<String> = store
        .entities(project_id)
        .expect("entities")
        .into_iter()
        .map(|entity| entity.external_id)
        .collect();
    assert_eq!(
        second_ids, first_ids,
        "external ids must be stable across rebuilds"
    );
    assert!(first_counts.entities > 0);
}

#[test]
fn rebuild_recovers_after_database_is_deleted() {
    let dir = Scratch::new("rebuild");
    let db = dir.join("index.sqlite3");

    let first_counts = {
        let mut store = basic_mem::storage::Store::open(&db).expect("open");
        let project_id = project(&store);
        rebuild(&mut store, project_id);
        store.counts(project_id).expect("counts")
    };

    drop(std::fs::metadata(&db));
    fs::remove_file(&db).expect("delete derived index");
    assert!(!db.exists());

    let mut store = basic_mem::storage::Store::open(&db).expect("reopen");
    let project_id = project(&store);
    rebuild(&mut store, project_id);
    let second_counts = store.counts(project_id).expect("counts");
    assert_eq!(
        second_counts, first_counts,
        "index must rebuild from markdown"
    );
}

#[test]
fn rebuilt_index_resolves_relation_targets() {
    let mut store = basic_mem::storage::Store::open_in_memory().expect("store");
    let project_id = project(&store);
    let report = rebuild(&mut store, project_id);
    assert!(
        report.relations_resolved > 0,
        "known targets such as oracle/projects/alpha must resolve"
    );
    let resolved = store
        .relations(project_id)
        .expect("relations")
        .into_iter()
        .filter(|relation| relation.to_id.is_some())
        .count();
    assert!(resolved > 0);
    assert!(resolved <= report.relations);
}

#[test]
fn markdown_files_are_discovered_recursively_but_not_dot_dirs() {
    let dir = Scratch::new("scan");
    fs::create_dir_all(dir.join("a")).expect("dir");
    fs::create_dir_all(dir.join(".obsidian")).expect("dir");
    fs::write(dir.join("a/note.md"), "# A\n").expect("write");
    fs::write(dir.join(".obsidian/hidden.md"), "# Hidden\n").expect("write");

    let mut store = basic_mem::storage::Store::open_in_memory().expect("store");
    let project_id = store
        .upsert_project("scan", "scan", &dir.path().to_string_lossy())
        .expect("project");
    let options = basic_mem::indexing::RebuildOptions::new("scan");
    let report = basic_mem::indexing::rebuild_vault(&mut store, project_id, dir.path(), &options)
        .expect("rebuild");

    assert_eq!(report.files_seen, 1);
    assert_eq!(report.documents_indexed, 1);
}

fn _assert_path(_path: &Path) {}
