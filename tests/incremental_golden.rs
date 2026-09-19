//! Phase 6 compatibility: incremental indexing, reconciliation, moves, deletes,
//! and full-vs-incremental convergence.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use basic_mem::indexing::{
    ChangeKind, Debouncer, FileEvent, IndexOptions, IndexOutcome, IndexService, RebuildOptions,
    rebuild_vault,
};
use basic_mem::storage::Store;
mod common;
use common::{Scratch, copy_dir, fixtures_vault};

/// Fresh temp vault (copy of the tracked fixtures) plus an in-memory store.
fn setup(tag: &str) -> (Scratch, PathBuf, Store, i64) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_dir(&fixtures_vault(), &vault);
    let store = Store::open_in_memory().expect("store");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .expect("project");
    (dir, vault, store, project_id)
}

fn service<'a>(
    store: &'a mut Store,
    project_id: i64,
    vault: &Path,
    update_permalinks_on_move: bool,
) -> IndexService<'a> {
    IndexService::new(
        store,
        project_id,
        vault,
        IndexOptions::new("oracle").with_update_permalinks_on_move(update_permalinks_on_move),
    )
}

fn snapshot(store: &Store, project_id: i64) -> (Vec<String>, Vec<String>, Vec<String>) {
    let entities = store.entities(project_id).expect("entities");
    let name_of: BTreeMap<i64, String> = entities
        .iter()
        .map(|entity| {
            (
                entity.id,
                entity
                    .permalink
                    .clone()
                    .unwrap_or_else(|| entity.file_path.clone()),
            )
        })
        .collect();

    let mut entity_keys: Vec<String> = entities
        .iter()
        .map(|entity| {
            format!(
                "{}|{}|{}|{}",
                entity.file_path,
                entity.title,
                entity.note_type,
                entity.permalink.as_deref().unwrap_or("")
            )
        })
        .collect();
    entity_keys.sort();

    let mut observation_keys: Vec<String> = store
        .observations(project_id)
        .expect("observations")
        .into_iter()
        .map(|observation| {
            format!(
                "{}|{}|{}|{}|{}",
                name_of
                    .get(&observation.entity_id)
                    .cloned()
                    .unwrap_or_default(),
                observation.category,
                observation.content,
                observation.context.unwrap_or_default(),
                observation.tags.join(",")
            )
        })
        .collect();
    observation_keys.sort();

    let mut relation_keys: Vec<String> = store
        .relations(project_id)
        .expect("relations")
        .into_iter()
        .map(|relation| {
            format!(
                "{}|{}|{}|{}",
                name_of.get(&relation.from_id).cloned().unwrap_or_default(),
                relation.relation_type,
                relation.to_name,
                relation.context.unwrap_or_default()
            )
        })
        .collect();
    relation_keys.sort();

    (entity_keys, observation_keys, relation_keys)
}

#[test]
fn reconcile_indexes_new_files() {
    let (_dir, vault, mut store, project_id) = setup("reconcile-new");
    let report = service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("reconcile");

    assert_eq!(
        report.added, 15,
        "15 valid notes; the malformed one is skipped"
    );
    assert_eq!(report.skipped, 1);
    assert_eq!(report.removed, 0);

    let counts = store.counts(project_id).expect("counts");
    assert_eq!(counts.entities, 15);
    assert_eq!(counts.observations, 16);
    assert_eq!(counts.relations, 16);
}

#[test]
fn reconcile_is_idempotent_for_unchanged_files() {
    let (_dir, vault, mut store, project_id) = setup("reconcile-idempotent");
    service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("first");
    let second = service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("second");

    assert_eq!(second.added, 0);
    assert_eq!(second.updated, 0);
    assert_eq!(second.unchanged, 15);
    assert_eq!(store.counts(project_id).expect("counts").entities, 15);
}

#[test]
fn modified_file_updates_without_duplicates() {
    let (_dir, vault, mut store, project_id) = setup("modified");
    service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("initial");
    let entity_id = store
        .entity_by_file_path(project_id, "notes/simple.md")
        .expect("entity")
        .expect("exists")
        .id;

    let path = vault.join("notes/simple.md");
    let mut content = fs::read_to_string(&path).expect("read");
    content.push_str("\n- [fact] added by the incremental test\n");
    fs::write(&path, content).expect("write");

    let report = service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("reconcile");
    assert_eq!(report.updated, 1);
    assert_eq!(report.added, 0);

    let counts = store.counts(project_id).expect("counts");
    assert_eq!(counts.entities, 15, "no duplicate entity");
    assert_eq!(counts.observations, 17, "one observation added");
    let entity = store
        .entity_by_file_path(project_id, "notes/simple.md")
        .expect("entity")
        .expect("exists");
    assert_eq!(entity.id, entity_id, "row id stays stable across updates");
}

#[test]
fn rename_keeps_permalink_and_leaves_no_ghost() {
    let (_dir, vault, mut store, project_id) = setup("rename");
    service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("initial");
    let original = store
        .entity_by_file_path(project_id, "notes/simple.md")
        .expect("entity")
        .expect("exists");

    fs::rename(
        vault.join("notes/simple.md"),
        vault.join("notes/renamed.md"),
    )
    .expect("rename");
    let outcome = service(&mut store, project_id, &vault, false)
        .move_file("notes/simple.md", "notes/renamed.md")
        .expect("move");
    assert_eq!(outcome, IndexOutcome::Indexed);

    assert!(
        store
            .entity_by_file_path(project_id, "notes/simple.md")
            .expect("query")
            .is_none(),
        "old path must not linger"
    );
    let moved = store
        .entity_by_file_path(project_id, "notes/renamed.md")
        .expect("query")
        .expect("moved row");
    assert_eq!(
        moved.permalink, original.permalink,
        "update_permalinks_on_move defaults to false"
    );
    assert_eq!(store.counts(project_id).expect("counts").entities, 15);

    let reconcile = service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("reconcile");
    assert_eq!(reconcile.removed, 0);
    assert_eq!(reconcile.added, 0);
}

#[test]
fn rename_updates_permalink_when_configured() {
    let (_dir, vault, mut store, project_id) = setup("rename-permalink");
    service(&mut store, project_id, &vault, true)
        .reconcile()
        .expect("initial");

    fs::rename(
        vault.join("notes/simple.md"),
        vault.join("notes/renamed.md"),
    )
    .expect("rename");
    service(&mut store, project_id, &vault, true)
        .move_file("notes/simple.md", "notes/renamed.md")
        .expect("move");

    let moved = store
        .entity_by_file_path(project_id, "notes/renamed.md")
        .expect("query")
        .expect("moved row");
    assert_eq!(moved.permalink.as_deref(), Some("oracle/notes/renamed"));
}

#[test]
fn deleted_file_is_pruned() {
    let (_dir, vault, mut store, project_id) = setup("delete");
    service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("initial");
    fs::remove_file(vault.join("notes/task-markers.md")).expect("delete");

    let report = service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("reconcile");
    assert_eq!(report.removed, 1);
    assert_eq!(store.counts(project_id).expect("counts").entities, 14);
}

#[test]
fn malformed_file_does_not_corrupt_the_index() {
    let (_dir, vault, mut store, project_id) = setup("malformed");
    service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("initial");
    fs::write(
        vault.join("notes/broken.md"),
        "---\ntitle: \"Unterminated\ntags: [a, b\n---\n\n# Broken\n",
    )
    .expect("write");

    let report = service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("reconcile");
    // The fixture vault already contains one malformed file; the new one adds a second.
    assert_eq!(report.skipped, 2);
    assert_eq!(report.added, 0);
    let counts = store.counts(project_id).expect("counts");
    assert_eq!(counts.entities, 15, "broken file is not indexed");
    assert!(
        store
            .entity_by_file_path(project_id, "notes/simple.md")
            .expect("query")
            .is_some(),
        "healthy notes stay indexed"
    );
}

#[test]
fn incremental_and_full_rebuild_converge() {
    let (_dir, vault, mut store, project_id) = setup("converge");
    service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("initial");

    // Mutate the vault: edit one note, delete another, add a third.
    let simple = vault.join("notes/simple.md");
    let mut content = fs::read_to_string(&simple).expect("read");
    content.push_str("\n- [fact] edited for convergence\n");
    fs::write(&simple, content).expect("write");
    fs::remove_file(vault.join("notes/task-markers.md")).expect("delete");
    fs::write(
        vault.join("notes/added.md"),
        "# Added\n\n- [fact] created for convergence\n\nSee [[notes/simple]].\n",
    )
    .expect("write");

    service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("incremental");
    let incremental = snapshot(&store, project_id);

    let mut fresh = Store::open_in_memory().expect("store");
    let fresh_project = fresh
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .expect("project");
    rebuild_vault(
        &mut fresh,
        fresh_project,
        &vault,
        &RebuildOptions::new("oracle"),
    )
    .expect("full rebuild");
    let full = snapshot(&fresh, fresh_project);

    assert_eq!(
        incremental, full,
        "incremental reconcile must converge on full rebuild"
    );
}

#[test]
fn debounced_events_drive_reconcile() {
    let (_dir, vault, mut store, project_id) = setup("debounce");
    service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("initial");

    let start = Instant::now();
    let mut debouncer = Debouncer::new(Duration::from_millis(200));
    // An editor save can emit several events for the same path.
    debouncer.push(FileEvent {
        path: "notes/simple.md".to_owned(),
        kind: ChangeKind::Created,
        at: start,
    });
    debouncer.push(FileEvent {
        path: "notes/simple.md".to_owned(),
        kind: ChangeKind::Modified,
        at: start + Duration::from_millis(20),
    });
    assert!(
        debouncer
            .drain_ready(start + Duration::from_millis(100))
            .is_empty()
    );

    let path = vault.join("notes/simple.md");
    let mut content = fs::read_to_string(&path).expect("read");
    content.push_str("\n- [fact] debounced update\n");
    fs::write(&path, content).expect("write");

    let ready = debouncer.drain_ready(start + Duration::from_millis(220));
    assert_eq!(ready.len(), 1, "events for one path coalesce");
    assert_eq!(ready[0].kind, ChangeKind::Modified);

    let report = service(&mut store, project_id, &vault, false)
        .reconcile()
        .expect("reconcile");
    assert_eq!(report.updated, 1);
    assert_eq!(store.counts(project_id).expect("counts").observations, 17);
}
