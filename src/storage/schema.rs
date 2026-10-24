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
    -- Upstream 0.24.0 (#1455): a non-Markdown file records the media type it was indexed
    -- with, so a listing can say what a file is instead of calling everything text.
    format          TEXT,
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
    id          INTEGER PRIMARY KEY,
    project_id  INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    entity_id   INTEGER NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
    category    TEXT NOT NULL DEFAULT 'note',
    content     TEXT NOT NULL,
    context     TEXT,
    tags        TEXT NOT NULL DEFAULT '[]',
    -- Upstream 0.24.0 (#1426, SPEC-82): an authored `@effective`/`@valid`/... qualifier
    -- states when the claim holds. NULL for an observation without one.
    time_kind   TEXT,
    valid_start TEXT,
    -- Half-open: `valid_end` is exclusive. An instant stores the same value in both, so a
    -- range test needs no special case.
    valid_end   TEXT
);

CREATE INDEX IF NOT EXISTS ix_observation_entity_id ON observation (entity_id);
CREATE INDEX IF NOT EXISTS ix_observation_category ON observation (category);
-- The valid-time filters scan by range, so the start is indexed for the ordered case.
CREATE INDEX IF NOT EXISTS ix_observation_valid_start ON observation (valid_start);

CREATE TABLE IF NOT EXISTS relation (
    id            INTEGER PRIMARY KEY,
    project_id    INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    from_id       INTEGER NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
    -- Upstream 0.24.0 (#1344): deleting a target leaves its inbound relations in the
    -- unresolved state the indexer already produces for a forward reference, instead of
    -- erasing them. `from_id` keeps CASCADE because an entity does own the relations it
    -- declares.
    to_id         INTEGER REFERENCES entity(id) ON DELETE SET NULL,
    to_name       TEXT NOT NULL,
    relation_type TEXT NOT NULL,
    context       TEXT,
    UNIQUE (from_id, to_name, relation_type)
);

-- Upstream 0.24.0 (#1422, #1381-#1484): the per-project accepted-change journal the
-- Wiki projector reads. A row is written when a note write is accepted, so the projected
-- `log.md` lists changes in the order they happened rather than in file order.
CREATE TABLE IF NOT EXISTS accepted_change (
    id          INTEGER PRIMARY KEY,
    project_id  INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    -- The journal outlives the note it records: a delete's own row must survive the
    -- delete, so the reference is SET NULL rather than CASCADE (upstream 0.24.0 #1636).
    entity_id   INTEGER REFERENCES entity(id) ON DELETE SET NULL,
    -- `create`, `update`, `edit`, `move`, or `delete`.
    kind        TEXT NOT NULL,
    file_path   TEXT NOT NULL,
    -- The path before a move, for the log's "moved from" line.
    previous_path TEXT,
    -- Who made the change, when the runtime identifies its callers. A local runtime
    -- supplies no actor, so the column stays NULL and the vault gains no provenance it
    -- did not ask for (upstream 0.24.0 #1636).
    actor       TEXT,
    created_at  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS ix_accepted_change_project
    ON accepted_change (project_id, id DESC);

-- Upstream 0.24.0 (#1398): a note's body indexed by heading, so the POSIX-style read
-- tools can address one section instead of the whole file.
CREATE TABLE IF NOT EXISTS note_section (
    id         INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    entity_id  INTEGER NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
    level      INTEGER NOT NULL DEFAULT 0,
    heading    TEXT NOT NULL DEFAULT '',
    start_line INTEGER NOT NULL,
    end_line   INTEGER NOT NULL,
    content    TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS ix_note_section_entity_id ON note_section (entity_id);
CREATE INDEX IF NOT EXISTS ix_note_section_heading ON note_section (heading);

CREATE INDEX IF NOT EXISTS ix_relation_from_id ON relation (from_id);
CREATE INDEX IF NOT EXISTS ix_relation_to_id ON relation (to_id);
CREATE INDEX IF NOT EXISTS ix_relation_type ON relation (relation_type);

-- Reference DDL (models/search.py); populated in the search phase.
CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
    id UNINDEXED,
    title,
    content_stems,
    -- Upstream 0.24.0 (#1294): character unigrams and bigrams for scripts written
    -- without spaces, kept in their own column so they add recall without changing the
    -- bm25 length normalization of the word index that every other query ranks with.
    cjk_grams,
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
