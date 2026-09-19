//! SQLite schema and migrations.
//!
//! Table and column names mirror Basic Memory 0.23.2 so the derived index can be
//! compared with the reference projections (`entity`, `observation`, `relation`,
//! `search_index`). The FTS5 table is created here but populated by the search
//! phase, where stemming and CJK channels must match the reference.

use rusqlite::Connection;

use crate::error::Result;

/// Current schema version written to `index_metadata`.
pub const SCHEMA_VERSION: i64 = 1;

const DDL: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS index_metadata (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS project (
    id          INTEGER PRIMARY KEY,
    external_id TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL UNIQUE,
    permalink   TEXT NOT NULL UNIQUE,
    path        TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS entity (
    id              INTEGER PRIMARY KEY,
    external_id     TEXT NOT NULL UNIQUE,
    project_id      INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    title           TEXT NOT NULL,
    note_type       TEXT NOT NULL,
    entity_metadata TEXT,
    content_type    TEXT NOT NULL,
    permalink       TEXT,
    file_path       TEXT NOT NULL,
    checksum        TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS uix_entity_permalink_project
    ON entity (permalink, project_id) WHERE permalink IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uix_entity_file_path_project
    ON entity (file_path, project_id);
CREATE INDEX IF NOT EXISTS ix_entity_project_id ON entity (project_id);
CREATE INDEX IF NOT EXISTS ix_entity_note_type ON entity (note_type);

CREATE TABLE IF NOT EXISTS observation (
    id         INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    entity_id  INTEGER NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
    category   TEXT NOT NULL DEFAULT 'note',
    content    TEXT NOT NULL,
    context    TEXT,
    tags       TEXT NOT NULL DEFAULT '[]'
);

CREATE INDEX IF NOT EXISTS ix_observation_entity_id ON observation (entity_id);
CREATE INDEX IF NOT EXISTS ix_observation_category ON observation (category);

CREATE TABLE IF NOT EXISTS relation (
    id            INTEGER PRIMARY KEY,
    project_id    INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    from_id       INTEGER NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
    to_id         INTEGER REFERENCES entity(id) ON DELETE CASCADE,
    to_name       TEXT NOT NULL,
    relation_type TEXT NOT NULL,
    context       TEXT,
    UNIQUE (from_id, to_name, relation_type)
);

CREATE INDEX IF NOT EXISTS ix_relation_from_id ON relation (from_id);
CREATE INDEX IF NOT EXISTS ix_relation_to_id ON relation (to_id);
CREATE INDEX IF NOT EXISTS ix_relation_type ON relation (relation_type);

-- Reference DDL (models/search.py); populated in the search phase.
CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
    id UNINDEXED,
    title,
    content_stems,
    content_snippet,
    permalink,
    file_path UNINDEXED,
    type UNINDEXED,
    project_id UNINDEXED,
    from_id UNINDEXED,
    to_id UNINDEXED,
    relation_type UNINDEXED,
    entity_id UNINDEXED,
    category UNINDEXED,
    metadata UNINDEXED,
    created_at UNINDEXED,
    updated_at UNINDEXED,
    tokenize='unicode61 tokenchars 0x2F',
    prefix='1,2,3,4'
);

-- Reference DDL (models/search.py), except that the vectors live in a regular
-- BLOB table instead of the sqlite-vec `vec0` virtual table: the reference does
-- exact KNN over normalized vectors, which is what we compute in Rust, and a
-- virtual table would require loading the extension into every connection.
CREATE TABLE IF NOT EXISTS search_vector_chunks (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    entity_id          INTEGER NOT NULL,
    project_id         INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    chunk_key          TEXT NOT NULL,
    chunk_text         TEXT NOT NULL,
    source_hash        TEXT NOT NULL,
    entity_fingerprint TEXT NOT NULL,
    embedding_model    TEXT NOT NULL,
    vector_index       TEXT NOT NULL,
    embedding_status   TEXT NOT NULL CHECK (embedding_status IN ('pending', 'ready')),
    updated_at         TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_search_vector_chunks_project_entity
    ON search_vector_chunks (project_id, entity_id);
CREATE UNIQUE INDEX IF NOT EXISTS uix_search_vector_chunks_entity_key
    ON search_vector_chunks (project_id, entity_id, chunk_key);

CREATE TABLE IF NOT EXISTS search_vector_embeddings (
    rowid       INTEGER PRIMARY KEY REFERENCES search_vector_chunks(id) ON DELETE CASCADE,
    embedding   BLOB NOT NULL,
    source_hash TEXT NOT NULL
);
"#;

/// Create tables if needed and record the schema version.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(DDL)?;
    conn.execute(
        "INSERT INTO index_metadata (key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// Read a metadata value, if present.
pub fn metadata(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut statement = conn.prepare("SELECT value FROM index_metadata WHERE key = ?1")?;
    let mut rows = statement.query([key])?;
    Ok(rows.next()?.map(|row| row.get(0)).transpose()?)
}

/// Whether this SQLite build exposes FTS5.
pub fn has_fts5(conn: &Connection) -> bool {
    conn.prepare("SELECT count(*) FROM search_index").is_ok()
}
