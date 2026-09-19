//! What `entity.created_at` / `entity.updated_at` mean when frontmatter is silent.
//!
//! Measured against the reference: a note with no frontmatter timestamps gets
//! `updated_at = file mtime` and `created_at = the row's insert time`. This case only shows
//! up when the file is written measurably before the index run, so the test sleeps between
//! the two and asserts the ordering rather than an exact value.

use std::fs;
use std::time::Duration;

use auto_memory::indexing::{IndexOptions, IndexService, RebuildOptions, rebuild_vault};
use auto_memory::storage::Store;
mod common;
use common::Scratch;

#[test]
fn created_at_is_the_insert_time_and_updated_at_is_the_file_mtime() {
    let dir = Scratch::new("fresh");
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(
        vault.join("note.md"),
        "# Timestamp Probe\n\n- [fact] no frontmatter timestamps\n",
    )
    .expect("write");

    // Make the file's mtime measurably older than the index run.
    let file_time = fs::metadata(vault.join("note.md"))
        .expect("metadata")
        .modified()
        .expect("mtime");
    let file_time = auto_memory::domain::timeframe::storage_timestamp(
        chrono::DateTime::<chrono::FixedOffset>::from(chrono::DateTime::<chrono::Utc>::from(
            file_time,
        ))
        .with_timezone(&chrono::Local)
        .fixed_offset(),
    );
    std::thread::sleep(Duration::from_millis(1100));

    let mut store = Store::open_in_memory().expect("store");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .expect("project");
    rebuild_vault(
        &mut store,
        project_id,
        &vault,
        &RebuildOptions::new("oracle"),
    )
    .expect("rebuild");

    let entity = store
        .entity_by_file_path(project_id, "note.md")
        .expect("lookup")
        .expect("indexed");
    let created = store
        .entity_created_at(entity.id)
        .expect("created")
        .expect("a row always has created_at");
    let updated = store
        .search_rows_by_ids(project_id, &[entity.id])
        .expect("search rows")
        .first()
        .and_then(|row| row.updated_at.clone())
        .expect("updated_at");

    assert_eq!(updated, file_time, "updated_at is the file mtime");
    assert!(
        created.as_str() > file_time.as_str(),
        "created_at must be the insert time, not the file ctime/mtime: \
         created={created} file={file_time}"
    );

    // Re-indexing keeps the original insert time (the reference's UPDATE does not touch it).
    fs::write(
        vault.join("note.md"),
        "# Timestamp Probe\n\n- [fact] edited later\n",
    )
    .expect("write");
    let mut service =
        IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
    service.force_index_file("note.md").expect("reindex");
    let same = store
        .entity_by_file_path(project_id, "note.md")
        .expect("lookup");
    let same = same.expect("still indexed");
    assert_eq!(same.id, entity.id, "the row keeps its identity");
    assert_eq!(
        store
            .entity_created_at(same.id)
            .expect("created")
            .expect("created_at"),
        created,
        "an update must not re-stamp created_at"
    );
}
