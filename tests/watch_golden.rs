//! Phase 6b: the filesystem watcher drives the incremental index.
//!
//! Covers what the reference watcher provides: debounced coalescing per path,
//! ignored paths never reaching the index, delete/create pairing becoming a move
//! (permalink preserved), removals, and the real `notify` loop end to end.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use basic_mem::indexing::{
    ChangeKind, IndexOptions, IndexService, VaultWatcher, WatchReport, shutdown_when, watch_vault,
};
use basic_mem::runtime::block_on;
use basic_mem::storage::Store;
mod common;
use common::{Scratch, fixture};

#[test]
fn created_modified_and_deleted_files_follow_the_watch_window() {
    let (_dir, vault, mut store, project_id) = fixture("basic");
    let mut service =
        IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
    assert!(service.reconcile().expect("initial reconcile").added > 0);
    let mut watcher = VaultWatcher::new(service, &vault).with_window(Duration::from_millis(50));

    fs::write(
        vault.join("notes/watched.md"),
        "# Watched\n\n- [fact] fresh\n",
    )
    .expect("write");
    let start = Instant::now();
    watcher.record_at("notes/watched.md", ChangeKind::Created, start);
    assert_eq!(watcher.pending(), 1);
    assert!(watcher.poll(start).expect("poll").is_empty(), "window open");
    let report = watcher
        .poll(start + Duration::from_millis(60))
        .expect("poll");
    assert_eq!(report.indexed, 1, "{report:?}");
    assert_eq!(watcher.pending(), 0);

    fs::write(
        vault.join("notes/watched.md"),
        "# Watched\n\n- [fact] changed\n",
    )
    .expect("write");
    watcher.record_at("notes/watched.md", ChangeKind::Modified, start);
    let report = watcher
        .poll(start + Duration::from_millis(60))
        .expect("poll");
    assert_eq!(report.indexed, 1);

    fs::remove_file(vault.join("notes/watched.md")).expect("remove");
    watcher.record_at("notes/watched.md", ChangeKind::Removed, start);
    let report = watcher
        .poll(start + Duration::from_millis(60))
        .expect("poll");
    assert_eq!(report.removed, 1, "{report:?}");
    drop(watcher);
}

#[test]
fn rename_is_paired_into_a_move() {
    let (_dir, vault, mut store, project_id) = fixture("move");
    let mut service =
        IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
    service.reconcile().expect("initial reconcile");
    let before = service
        .entity_checksum("notes/simple.md")
        .expect("checksum")
        .expect("indexed");
    let mut watcher = VaultWatcher::new(service, &vault).with_window(Duration::from_millis(50));

    fs::rename(
        vault.join("notes/simple.md"),
        vault.join("notes/simple-moved.md"),
    )
    .expect("rename");
    let start = Instant::now();
    watcher.record_at("notes/simple.md", ChangeKind::Removed, start);
    watcher.record_at("notes/simple-moved.md", ChangeKind::Created, start);
    let report = watcher
        .poll(start + Duration::from_millis(60))
        .expect("poll");
    assert_eq!(report.moved, 1, "{report:?}");
    assert_eq!(report.indexed, 0);
    drop(watcher);

    let moved = store
        .entity_by_file_path(project_id, "notes/simple-moved.md")
        .expect("lookup")
        .expect("moved entity");
    // The move rewrites the destination with the entity's identity (the reference's own
    // move does the same), so the checksum moves with the file while the permalink does not.
    assert_ne!(moved.checksum.as_deref(), Some(before.as_str()));
    assert_eq!(moved.title, "simple", "the title survives the rename");
    assert_eq!(
        moved.permalink.as_deref(),
        Some("oracle/notes/simple"),
        "update_permalinks_on_move=false keeps the permalink"
    );
    assert!(
        store
            .entity_by_file_path(project_id, "notes/simple.md")
            .expect("lookup")
            .is_none(),
        "the old path must not keep a ghost row"
    );
}

#[test]
fn ignored_paths_and_non_markdown_never_enter_the_queue() {
    let (_dir, vault, mut store, project_id) = fixture("ignore");
    let service = IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
    let mut watcher = VaultWatcher::new(service, &vault).with_window(Duration::from_millis(10));

    for path in [
        ".obsidian/app.json",
        ".hidden.md",
        "node_modules/pkg/readme.md",
        "notes/scratch.tmp",
        "notes/readme.txt",
    ] {
        watcher.record(path, ChangeKind::Created);
    }
    assert_eq!(watcher.pending(), 0, "no ignored path may be queued");
    assert_eq!(watcher.flush().expect("flush"), WatchReport::default());
}

#[test]
fn repeated_events_for_one_path_coalesce() {
    let (_dir, vault, mut store, project_id) = fixture("coalesce");
    let mut service =
        IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
    service.reconcile().expect("initial reconcile");
    let mut watcher = VaultWatcher::new(service, &vault).with_window(Duration::from_millis(50));

    fs::write(
        vault.join("notes/simple.md"),
        "# Simple Note\n\n- [note] revised\n",
    )
    .expect("write");
    let start = Instant::now();
    watcher.record_at("notes/simple.md", ChangeKind::Created, start);
    watcher.record_at(
        "notes/simple.md",
        ChangeKind::Modified,
        start + Duration::from_millis(5),
    );
    watcher.record_at(
        "notes/simple.md",
        ChangeKind::Modified,
        start + Duration::from_millis(10),
    );
    assert_eq!(watcher.pending(), 1, "one path, one pending entry");
    let report = watcher
        .poll(start + Duration::from_millis(70))
        .expect("poll");
    assert_eq!(report.indexed, 1, "coalesced into a single write");
}

/// The real `notify` loop: a file written while watching reaches the index.
#[test]
fn notify_loop_indexes_a_new_file() {
    let dir = Scratch::new("loop");
    let vault = dir.join("vault");
    let index_path = dir.join("memory.db");
    fs::create_dir_all(&vault).expect("vault");
    let vault_for_watcher = vault.clone();

    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let stop_for_thread = stop.clone();
    let index_for_thread = index_path.clone();
    let handle = std::thread::spawn(move || {
        let mut store = Store::open(&index_for_thread).expect("store");
        let project_id = store
            .upsert_project("oracle", "oracle", &vault_for_watcher.to_string_lossy())
            .expect("project");
        let service = IndexService::new(
            &mut store,
            project_id,
            &vault_for_watcher,
            IndexOptions::new("oracle"),
        );
        let watcher =
            VaultWatcher::new(service, &vault_for_watcher).with_window(Duration::from_millis(50));
        // The loop is async now; this thread owns the store, so it drives the runtime.
        block_on(watch_vault(
            watcher,
            shutdown_when(|| stop_for_thread.load(Ordering::Relaxed)),
        ))
        .expect("runtime")
        .expect("watch loop")
    });

    std::thread::sleep(Duration::from_millis(300));
    fs::write(
        vault.join("note.md"),
        "# Note\n\n- [fact] from the watcher\n",
    )
    .expect("write");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut indexed = false;
    while Instant::now() < deadline {
        if let Ok(store) = Store::open(&index_path) {
            if let Ok(Some(entity)) = store.entity_by_file_path(1, "note.md") {
                indexed = entity.checksum.is_some();
                if indexed {
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::Relaxed);
    let batches = handle.join().expect("watch thread");

    assert!(indexed, "the watcher must index the new note");
    assert!(batches >= 1, "at least one batch applied");
}
