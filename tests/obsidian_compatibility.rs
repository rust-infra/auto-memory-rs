//! Phase 14: Obsidian is the only manual management interface.
//!
//! The promise this file pins is narrow and concrete: a human editing the vault in
//! Obsidian — writing a note, renaming it in the file explorer, deleting it, linking to
//! a note that does not exist yet — must leave the derived index correct, and the vault
//! alone must be enough to rebuild everything.
//!
//! Indexer-level convergence (rename/delete/reconcile vs. full rebuild) is already
//! covered by `tests/incremental_golden.rs`; what is tested here is the *Obsidian*
//! flavor of those operations: the atomic save Obsidian actually performs, the internal
//! directories it writes, and link integrity across renames and deletions.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use basic_mem::indexing::{
    ChangeKind, IndexOptions, IndexService, VaultWatcher, shutdown_when, watch_vault,
};
use basic_mem::runtime::block_on;
use basic_mem::storage::Store;
mod common;
use common::{Scratch, copy_dir, fixture, repo_root};

/// Write the way Obsidian does: a sibling temp file, then a rename over the target.
fn atomic_write(vault: &Path, relative_path: &str, content: &str) {
    let target = vault.join(relative_path);
    let temp = target.with_extension("md.tmp");
    fs::write(&temp, content).expect("write temp");
    fs::rename(&temp, &target).expect("rename over target");
}

/// The `links_to` search row owned by the note that links to a future target.
fn relation_search_row(store: &Store, project_id: i64) -> basic_mem::domain::SearchResult {
    store
        .search_text(
            project_id,
            &basic_mem::search::TextSearchOptions {
                entity_types: vec![basic_mem::domain::SearchItemType::Relation],
                ..basic_mem::search::TextSearchOptions::default()
            },
        )
        .expect("search")
        .results
        .into_iter()
        .find(|row| {
            row.file_path == "notes/points-here.md"
                && row.relation_type.as_deref() == Some("links_to")
        })
        .expect("relation search row")
}

/// Phase 14 item 2: an atomic save replaces content without changing the note's identity.
///
/// Obsidian never writes a note in place; it writes `.md.tmp` and renames it over the
/// target. The rename produces both a create (for the target) and a delete (for the temp
/// name), so the watcher must ignore the temp file and treat the target as one write —
/// keeping the entity id, permalink, and `created_at` while the checksum changes.
#[test]
fn obsidian_atomic_save_keeps_the_note_identity() {
    let (_dir, vault, mut store, project_id) = fixture("atomic");
    {
        // `IndexService` borrows the store mutably, so every phase that touches the
        // store directly runs in its own scope (the watcher takes ownership).
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        service.reconcile().expect("initial reconcile");
    }

    let before = store
        .entity_by_file_path(project_id, "notes/simple.md")
        .expect("lookup")
        .expect("indexed note");
    let before_checksum = before.checksum.clone();
    let before_created = store.entity_created_at(before.id).expect("created_at");
    let before_entities = store.counts(project_id).expect("counts").entities;

    atomic_write(
        &vault,
        "notes/simple.md",
        "# Simple Note\n\n- [note] rewritten while Obsidian was open\n",
    );

    {
        let service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        let mut watcher = VaultWatcher::new(service, &vault).with_window(Duration::from_millis(20));
        // The temp file is not a markdown note, so it must never reach the queue.
        watcher.record("notes/simple.md.tmp", ChangeKind::Created);
        watcher.record("notes/simple.md", ChangeKind::Created);
        assert_eq!(watcher.pending(), 1, "only the note path is queued");
        let report = watcher.flush().expect("flush");
        assert_eq!(report.indexed, 1, "{report:?}");
    }

    let after = store
        .entity_by_file_path(project_id, "notes/simple.md")
        .expect("lookup")
        .expect("still indexed");
    assert_eq!(
        after.id, before.id,
        "an atomic save must not replace the entity"
    );
    assert_eq!(after.permalink, before.permalink);
    assert_ne!(after.checksum, before_checksum, "the content did change");
    assert_eq!(
        store.entity_created_at(after.id).expect("created_at"),
        before_created,
        "the note was edited, not recreated"
    );
    assert_eq!(
        store.counts(project_id).expect("counts").entities,
        before_entities,
        "no ghost entity for the temp file"
    );
    assert!(
        store
            .entity_by_file_path(project_id, "notes/simple.md.tmp")
            .expect("lookup")
            .is_none()
    );
}

/// Open the index, retrying the transient `SQLITE_BUSY` of a concurrent `Store::open`.
///
/// The index is opened twice in this test (the watcher thread and the assertions), and
/// applying the reference's per-connection profile starts with `journal_mode=WAL`, which
/// takes a brief exclusive lock. Two connections opening the same fresh file at once can
/// therefore see `SQLITE_BUSY` before either has installed its busy timeout, so the
/// opener retries.
fn open_index(path: &Path) -> Store {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Store::open(path) {
            Ok(store) => return store,
            Err(error) => assert!(Instant::now() < deadline, "opening the index: {error}"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Phase 14 item 1/2 end to end: the real `notify` loop follows an atomic save.
#[test]
fn real_watcher_follows_an_obsidian_atomic_save() {
    let dir = Scratch::new("atomic-live");
    let vault = dir.join("vault");
    copy_dir(&repo_root().join("tests/fixtures/vault"), &vault);
    let index_path = dir.join("memory.db");
    let vault_for_thread = vault.clone();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let index_for_thread = index_path.clone();
    let handle = std::thread::spawn(move || {
        let mut store = open_index(&index_for_thread);
        let project_id = store
            .upsert_project("oracle", "oracle", &vault_for_thread.to_string_lossy())
            .expect("project");
        let mut service = IndexService::new(
            &mut store,
            project_id,
            &vault_for_thread,
            IndexOptions::new("oracle"),
        );
        service.reconcile().expect("initial reconcile");
        let watcher =
            VaultWatcher::new(service, &vault_for_thread).with_window(Duration::from_millis(50));
        // The loop is async now; this thread owns the store, so it drives the runtime.
        block_on(watch_vault(
            watcher,
            shutdown_when(|| stop_for_thread.load(Ordering::Relaxed)),
        ))
        .expect("runtime")
        .expect("watch loop")
    });

    // The watcher thread reconciles the vault first, so wait for the note to exist
    // before snapshotting it.
    let before = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(store) = Store::open(&index_path)
                && let Ok(Some(entity)) = store.entity_by_file_path(1, "notes/simple.md")
            {
                break entity;
            }
            assert!(
                Instant::now() < deadline,
                "the initial reconcile must index the vault"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    std::thread::sleep(Duration::from_millis(200));
    atomic_write(
        &vault,
        "notes/simple.md",
        "# Simple Note\n\n- [note] saved by the editor\n",
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut updated = None;
    while Instant::now() < deadline {
        if let Ok(store) = Store::open(&index_path)
            && let Ok(Some(entity)) = store.entity_by_file_path(1, "notes/simple.md")
            && entity.checksum != before.checksum
        {
            updated = Some(entity);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::Relaxed);
    handle.join().expect("watch thread");

    let updated = updated.expect("the watcher must pick up the saved note");
    assert_eq!(updated.id, before.id);
    assert_eq!(
        updated.permalink, before.permalink,
        "an atomic save keeps the permalink"
    );
    assert_eq!(updated.title, "simple");
}

/// Phase 14 item 7: Obsidian's own directories are invisible to the index.
///
/// `.obsidian/` holds editor state and `.basic-memory/` holds vault-local config; both
/// are dot-directories, and the reference ignore rules must keep them out of the index
/// whether the change arrives through the watcher or through a reconcile.
#[test]
fn obsidian_internal_directories_never_enter_the_index() {
    let (_dir, vault, mut store, project_id) = fixture("internal-dirs");
    {
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        service.reconcile().expect("initial reconcile");
    }
    let before = store.counts(project_id).expect("counts");

    fs::create_dir_all(vault.join(".obsidian/plugins/dataview")).expect("plugin dir");
    fs::write(vault.join(".obsidian/workspace.json"), "{\"open\":[]}").expect("write");
    fs::write(
        vault.join(".obsidian/plugins/dataview/main.js"),
        "module.exports = {};",
    )
    .expect("write");
    fs::create_dir_all(vault.join(".basic-memory")).expect("config dir");
    fs::write(
        vault.join(".basic-memory/config.json"),
        "{\"env\":\"user\"}",
    )
    .expect("write");

    {
        let service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        let mut watcher = VaultWatcher::new(service, &vault).with_window(Duration::from_millis(10));
        for path in [
            ".obsidian/workspace.json",
            ".obsidian/plugins/dataview/main.js",
            ".basic-memory/config.json",
        ] {
            watcher.record(path, ChangeKind::Created);
        }
        assert_eq!(watcher.pending(), 0, "editor state must not be queued");
        let report = watcher.flush().expect("flush");
        assert_eq!(report, Default::default());
    }

    // The fixture vault already ships `.obsidian/app.json`, so this also proves a
    // reconcile leaves it alone.
    {
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        service.reconcile().expect("reconcile");
    }
    assert_eq!(store.counts(project_id).expect("counts"), before);
    for path in [
        ".obsidian/app.json",
        ".obsidian/workspace.json",
        ".basic-memory/config.json",
    ] {
        assert!(
            store
                .entity_by_file_path(project_id, path)
                .expect("lookup")
                .is_none(),
            "{path} must not be indexed"
        );
    }
}

/// Phase 14 item 5/6: a wikilink to a note that does not exist yet.
///
/// The link is authored first (as it is while writing), stays unresolved, and then
/// resolves once the target note is created — without re-editing the source note.
#[test]
fn wikilink_to_a_future_note_resolves_when_the_target_appears() {
    let (_dir, vault, mut store, project_id) = fixture("future-link");
    {
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        service.reconcile().expect("initial reconcile");
    }

    fs::write(
        vault.join("notes/points-here.md"),
        "# Points Here\n\n- links_to [[notes/later-target]]\n",
    )
    .expect("write");
    {
        let service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        let mut watcher = VaultWatcher::new(service, &vault).with_window(Duration::from_millis(20));
        watcher.record("notes/points-here.md", ChangeKind::Created);
        assert_eq!(watcher.flush().expect("flush").indexed, 1);
    }

    let relations = store.relations(project_id).expect("relations");
    assert_eq!(
        relations
            .iter()
            .filter(|relation| relation.to_name == "notes/later-target")
            .count(),
        1,
        "the dangling link is recorded"
    );
    assert!(
        relations
            .iter()
            .all(|relation| relation.to_name != "notes/later-target" || relation.to_id.is_none()),
        "a link to a missing note stays unresolved"
    );
    // A relation row only carries a permalink once its target resolves: the permalink
    // is built from the target's permalink, so a dangling link's row has none.
    let unresolved_row = relation_search_row(&store, project_id);
    assert_eq!(
        unresolved_row.permalink, None,
        "an unresolved link has no target permalink to name"
    );
    // The row's title names the target only once it exists, so an unresolved link reads
    // as a bare source row.
    assert_eq!(unresolved_row.title, "points-here");

    fs::write(
        vault.join("notes/later-target.md"),
        "# Later Target\n\n- [fact] written after the link\n",
    )
    .expect("write");
    {
        let service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        let mut watcher = VaultWatcher::new(service, &vault).with_window(Duration::from_millis(20));
        watcher.record("notes/later-target.md", ChangeKind::Created);
        assert_eq!(watcher.flush().expect("flush").indexed, 1);
    }

    let target = store
        .entity_by_file_path(project_id, "notes/later-target.md")
        .expect("lookup")
        .expect("indexed target");
    let relations = store.relations(project_id).expect("relations");
    let resolved = relations
        .iter()
        .find(|relation| relation.to_name == "notes/later-target")
        .expect("relation row");
    assert_eq!(
        resolved.to_id,
        Some(target.id),
        "creating the target resolves the existing link"
    );
    // The relation's search row is regenerated so its permalink names the resolved
    // target instead of the raw link text.
    let resolved_row = relation_search_row(&store, project_id);
    // The resolved row names the link target (`from -> <target name>`), which the
    // unresolved row could not do.
    assert_eq!(
        resolved_row.title, "points-here -> later-target",
        "the row now names the resolved target"
    );
    assert!(
        resolved_row
            .permalink
            .as_deref()
            .is_some_and(|permalink| permalink.ends_with("/links-to/oracle/notes/later-target")),
        "the relation row now points at the target: {:?}",
        resolved_row.permalink
    );
}

/// Phase 14 item 3: renaming in the file explorer must not break incoming links.
///
/// The reference default `update_permalinks_on_move=false` keeps the permalink, which is
/// exactly what makes other notes' links keep resolving after a rename.
#[test]
fn file_explorer_rename_keeps_incoming_links_intact() {
    let (_dir, vault, mut store, project_id) = fixture("rename-links");
    {
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        service.reconcile().expect("initial reconcile");
    }

    let simple = store
        .entity_by_file_path(project_id, "notes/simple.md")
        .expect("lookup")
        .expect("indexed note");
    let incoming_before = store
        .relations(project_id)
        .expect("relations")
        .into_iter()
        .find(|relation| relation.to_id == Some(simple.id))
        .expect("a note links to simple.md");

    fs::create_dir_all(vault.join("archive")).expect("dir");
    fs::rename(
        vault.join("notes/simple.md"),
        vault.join("archive/simple.md"),
    )
    .expect("rename");
    {
        let service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        let mut watcher = VaultWatcher::new(service, &vault).with_window(Duration::from_millis(20));
        watcher.record("notes/simple.md", ChangeKind::Removed);
        watcher.record("archive/simple.md", ChangeKind::Created);
        let report = watcher.flush().expect("flush");
        assert_eq!(report.moved, 1, "{report:?}");
    }

    let moved = store
        .entity_by_file_path(project_id, "archive/simple.md")
        .expect("lookup")
        .expect("moved note");
    assert_eq!(moved.id, simple.id, "the note keeps its identity");
    assert_eq!(
        store
            .entity_by_permalink(project_id, "oracle/notes/simple")
            .expect("lookup")
            .expect("permalink")
            .id,
        simple.id,
        "the permalink still resolves after the rename"
    );
    assert!(
        store
            .entity_by_file_path(project_id, "notes/simple.md")
            .expect("lookup")
            .is_none(),
        "no ghost row at the old path"
    );

    let incoming_after = store
        .relations(project_id)
        .expect("relations")
        .into_iter()
        .find(|relation| {
            relation.to_id == incoming_before.to_id
                && relation.from_id == incoming_before.from_id
                && relation.relation_type == incoming_before.relation_type
        })
        .expect("the incoming link survives");
    assert_eq!(incoming_after.to_id, Some(simple.id));
}

/// Phase 14 item 4/8: deleting a note drops its derived state, and the vault alone
/// restores everything on a full reindex.
#[test]
fn deleting_a_note_drops_derived_state_and_a_full_reindex_restores_the_vault() {
    let (_dir, vault, mut store, project_id) = fixture("delete-recover");
    {
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        service.reconcile().expect("initial reconcile");
    }

    // A note that links to `notes/simple.md`, so a deletion has a link to strand.
    fs::write(
        vault.join("notes/points-here.md"),
        "# Points Here\n\n- links_to [[notes/simple]]\n",
    )
    .expect("write");
    {
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        service.index_file("notes/points-here.md").expect("index");
    }
    let simple = store
        .entity_by_file_path(project_id, "notes/simple.md")
        .expect("lookup")
        .expect("indexed note");
    assert!(
        store
            .relations(project_id)
            .expect("relations")
            .iter()
            .any(|relation| relation.to_name == "notes/simple" && relation.to_id == Some(simple.id))
    );

    fs::remove_file(vault.join("notes/simple.md")).expect("delete");
    {
        let service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        let mut watcher = VaultWatcher::new(service, &vault).with_window(Duration::from_millis(20));
        watcher.record("notes/simple.md", ChangeKind::Removed);
        assert_eq!(watcher.flush().expect("flush").removed, 1);
    }

    assert!(
        store
            .entity_by_file_path(project_id, "notes/simple.md")
            .expect("lookup")
            .is_none()
    );
    assert!(
        store
            .observations_for_entity(simple.id)
            .expect("observations")
            .is_empty(),
        "the deleted note's observations go with it"
    );
    let page = store
        .search_text(
            project_id,
            &basic_mem::search::TextSearchOptions {
                permalink: Some("oracle/notes/simple".to_owned()),
                ..basic_mem::search::TextSearchOptions::default()
            },
        )
        .expect("search");
    assert_eq!(page.total, 0, "its search rows are gone");
    // `relation.to_id` is `ON DELETE CASCADE` in the reference schema too, so the
    // stranded link disappears with the target until the source is re-read.
    assert_eq!(
        store
            .relations(project_id)
            .expect("relations")
            .iter()
            .filter(|relation| relation.to_name == "notes/simple")
            .count(),
        0
    );

    // The vault is the only durable state: a full rebuild restores the link, now
    // visibly unresolved because its target no longer exists.
    {
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        let report = service.full_rebuild().expect("full rebuild");
        assert!(report.documents_indexed > 0, "{report:?}");
    }
    let relations = store.relations(project_id).expect("relations");
    let restored = relations
        .iter()
        .find(|relation| relation.to_name == "notes/simple")
        .expect("the dangling link comes back");
    assert_eq!(restored.to_id, None);
    assert!(
        store
            .entity_by_file_path(project_id, "notes/simple.md")
            .expect("lookup")
            .is_none()
    );
}
