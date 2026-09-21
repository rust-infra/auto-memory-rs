//! The SQLite connection profile, and the locking behavior it buys.
//!
//! The reference configures *every* connection in `basic_memory/db.py`
//! (`_configure_sqlite_connection`, wired to SQLAlchemy's `connect` event): WAL for
//! file-backed databases plus `busy_timeout=10000`, `synchronous=NORMAL`,
//! `cache_size=-64000`, `temp_store=MEMORY`, and `wal_autocheckpoint=1000`.
//!
//! The two that change behavior are WAL — under the rollback journal a reader that
//! reopens the index in a loop can starve a writer across its commit, which is how the
//! intermittent `database is locked` failure in `obsidian_compatibility.rs` appeared —
//! and `busy_timeout`, where the reference's 10 s replaces rusqlite's own 5 s default
//! (`sqlite3_busy_timeout(db, 5000)` on every open). The first test pins the exact
//! profile; the second shows the shared-index behavior those values are there for.

use std::thread;
use std::time::{Duration, Instant};

use auto_memory::storage::Store;
use rusqlite::{Connection, ErrorCode};
mod common;
use common::{Scratch, block_on};

/// Read one integer-valued pragma (or `query_row`'s panic when the name is wrong).
fn pragma_i64(connection: &Connection, sql: &str) -> i64 {
    connection
        .query_row(sql, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

/// Read one text-valued pragma.
fn pragma_text(connection: &Connection, sql: &str) -> String {
    connection
        .query_row(sql, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_connection_profile_matches_the_reference() {
    let dir = Scratch::new("profile");
    let path = dir.join("memory.db");
    let store = Store::open(&path).await.expect("open");

    async fn pragma_value(store: &Store, sql: &'static str) -> i64 {
        store
            .with_connection(move |connection| pragma_i64(connection, sql))
            .await
            .expect("pragma")
    }
    assert_eq!(pragma_value(&store, "PRAGMA busy_timeout").await, 10_000);
    assert_eq!(
        pragma_value(&store, "PRAGMA synchronous").await,
        1,
        "NORMAL"
    );
    assert_eq!(pragma_value(&store, "PRAGMA cache_size").await, -64_000);
    assert_eq!(pragma_value(&store, "PRAGMA temp_store").await, 2, "MEMORY");
    assert_eq!(
        pragma_value(&store, "PRAGMA wal_autocheckpoint").await,
        1_000
    );
    assert_eq!(pragma_value(&store, "PRAGMA foreign_keys").await, 1);
    assert_eq!(
        store
            .with_connection(|connection| pragma_text(connection, "PRAGMA journal_mode"))
            .await
            .expect("journal mode"),
        "wal"
    );

    // In-memory databases cannot use WAL, so the reference skips that one pragma;
    // the rest of the profile still applies.
    let memory = Store::open_in_memory().await.expect("memory");
    assert_ne!(
        memory
            .with_connection(|connection| pragma_text(connection, "PRAGMA journal_mode"))
            .await
            .expect("journal mode"),
        "wal"
    );
    assert_eq!(
        memory
            .with_connection(|connection| pragma_i64(connection, "PRAGMA busy_timeout"))
            .await
            .expect("busy timeout"),
        10_000
    );

    store.close().await.expect("close");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_writer_waits_for_a_locked_index_instead_of_failing() {
    let dir = Scratch::new("locked");
    let path = dir.join("memory.db");
    // Both stores open before the lock exists, so the only statement issued while the
    // lock is held is the write under test (`Store::open` runs migrations, which would
    // themselves wait). This arm shows a shared index does not make the second writer
    // fail outright; it holds for any busy handler at least as long as `hold`, so the
    // exact profile — 10 s, WAL — is what the first test pins.
    let _reader = Store::open(&path).await.expect("open");
    let writer = Store::open(&path).await.expect("open");

    let blocker = Connection::open(&path).expect("blocker");
    blocker
        .busy_timeout(Duration::from_millis(0))
        .expect("timeout");
    blocker
        .execute_batch("BEGIN IMMEDIATE")
        .expect("begin immediate");

    // Control: a connection without the profile cannot write while the lock is held.
    // Without this arm the test could pass vacuously if the lock were never taken.
    let control = Connection::open(&path).expect("control");
    control
        .busy_timeout(Duration::from_millis(0))
        .expect("timeout");
    let refused = control.execute(
        "INSERT INTO index_metadata (key, value) VALUES ('probe', 'x')",
        [],
    );
    assert!(
        matches!(&refused, Err(rusqlite::Error::SqliteFailure(error, _))
            if error.code == ErrorCode::DatabaseBusy),
        "an unconfigured writer must be refused while the lock is held, got {refused:?}"
    );

    let hold = Duration::from_millis(400);
    let started = Instant::now();
    let handle =
        thread::spawn(move || block_on(writer.upsert_project("oracle", "oracle", "/tmp/vault")));
    thread::sleep(hold);
    blocker.execute_batch("COMMIT").expect("release the lock");

    let outcome = handle.join().expect("writer thread");
    let waited = started.elapsed();
    assert!(
        outcome.is_ok(),
        "the writer must wait for the lock instead of failing: {outcome:?}"
    );
    assert!(
        waited >= hold - Duration::from_millis(100),
        "the write cannot finish before the lock is released (waited {waited:?})"
    );
}
