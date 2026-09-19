//! Index migrations: reopening a database must never lose rows or a version stamp.
//!
//! The port mirrors the reference's "create if missing" migration, so the cases that matter
//! are the ones a user actually hits: an existing index is re-stamped rather than rebuilt, a
//! blank file gets the full schema, and a database whose `schema_version` row is missing or
//! stale (an older build, a hand-edited file) is repaired in place.

use std::fs;

use auto_memory::storage::{SCHEMA_VERSION, Store};
mod common;
use common::Scratch;

#[test]
fn a_blank_file_gets_the_full_schema_and_the_current_version() {
    let dir = Scratch::new("blank");
    let path = dir.join("memory.db");
    let store = Store::open(&path).expect("open");
    assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
    assert!(store.has_fts5(), "the FTS5 table must exist");
    // Every table the store writes to is present.
    let count = |table: &str| -> i64 {
        store
            .connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap_or_else(|error| panic!("{table}: {error}"))
    };
    for table in [
        "project",
        "entity",
        "observation",
        "relation",
        "search_index",
        "search_vector_chunks",
        "search_vector_embeddings",
    ] {
        assert_eq!(count(table), 0, "{table} exists");
    }
    // `index_metadata` holds the version stamp written by the migration itself.
    assert_eq!(count("index_metadata"), 1);
}

#[test]
fn reopening_keeps_rows_and_restamps_the_version() {
    let dir = Scratch::new("reopen");
    let path = dir.join("memory.db");
    let vault = dir.join("vault");
    fs::create_dir_all(&vault).expect("vault");
    fs::write(
        vault.join("note.md"),
        "# Note\n\n- [fact] survives the reopen\n",
    )
    .expect("write");

    {
        let mut store = Store::open(&path).expect("open");
        let project_id = store
            .upsert_project("oracle", "oracle", &vault.to_string_lossy())
            .expect("project");
        let document = auto_memory::markdown::parse_document(
            "note.md",
            &fs::read_to_string(vault.join("note.md")).expect("read"),
        )
        .expect("parse");
        store
            .replace_document(
                project_id,
                "oracle",
                Some("oracle/note"),
                "checksum",
                &document,
                &auto_memory::domain::DocumentTimestamps {
                    created_at: "2026-01-01 00:00:00".to_owned(),
                    updated_at: "2026-01-01 00:00:00".to_owned(),
                },
            )
            .expect("index");
    }

    // An older build (or a hand-edited file) may carry no version row at all.
    {
        let connection = rusqlite::Connection::open(&path).expect("raw open");
        connection
            .execute(
                "DELETE FROM index_metadata WHERE key = 'schema_version'",
                [],
            )
            .expect("clear");
        connection
            .execute(
                "INSERT INTO index_metadata (key, value) VALUES ('schema_version', '0')",
                [],
            )
            .expect("stale");
    }

    let store = Store::open(&path).expect("reopen");
    assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
    let project = store
        .project_by_permalink("oracle")
        .expect("lookup")
        .expect("project");
    assert_eq!(project.path, vault.to_string_lossy());
    assert!(
        store
            .entity_by_file_path(project.id, "note.md")
            .expect("lookup")
            .is_some(),
        "the indexed note survives a migration"
    );
    assert_eq!(store.counts(project.id).expect("counts").entities, 1);
}

#[test]
fn migrating_twice_is_a_no_op() {
    let dir = Scratch::new("idempotent");
    let path = dir.join("memory.db");
    drop(Store::open(&path).expect("open"));
    let before = fs::metadata(&path).expect("metadata").len();
    let store = Store::open(&path).expect("reopen");
    assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
    // `CREATE TABLE IF NOT EXISTS` plus an upsert must not rewrite the schema or drop rows.
    let after = fs::metadata(&path).expect("metadata").len();
    assert_eq!(before, after, "a second migration must not grow the file");
}
