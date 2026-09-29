//! Transactional access to the rebuildable SQLite index.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, named_params, params};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::domain::document::{DocumentTimestamps, ParsedDocument};
use crate::domain::timeframe;
use crate::error::{Error, Result};
use crate::search::chunking::SemanticRow;
use crate::search::index_rows::{SearchIndexWriteRow, entity_row, observation_row, relation_row};
use crate::search::text::{SearchPage, TextSearchOptions, search_text};
use crate::storage::records::{
    Counts, DirectoryEntityRow, EntityRow, ObservationRow, ProjectRow, RelatedRow, RelationRow,
    SearchRowView, VectorChunkRow, VectorRow,
};
use crate::storage::schema;

/// Handle to the derived SQLite index.
///
/// The index is a cache: every row is rebuildable from the markdown corpus. SQLite
/// work runs on a dedicated `tokio-rusqlite` connection thread and every operation is
/// exposed as an async method.
pub struct Store {
    conn: Option<tokio_rusqlite::Connection>,
}

/// Map a `tokio-rusqlite` error back to the crate error, preserving application
/// errors that were carried through the connection thread.
fn map_async_error(error: tokio_rusqlite::Error) -> Error {
    match error {
        tokio_rusqlite::Error::Rusqlite(error) => Error::Sqlite(error),
        tokio_rusqlite::Error::Other(error) => match error.downcast::<Error>() {
            Ok(error) => *error,
            Err(error) => Error::AsyncSqlite(tokio_rusqlite::Error::Other(error)),
        },
        error => Error::AsyncSqlite(error),
    }
}

/// Run one fallible closure on a `tokio-rusqlite` connection.
async fn call_connection<T>(
    conn: &tokio_rusqlite::Connection,
    function: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
) -> Result<T>
where
    T: Send + 'static,
{
    conn.call(move |conn| {
        function(conn).map_err(|error| tokio_rusqlite::Error::Other(Box::new(error)))
    })
    .await
    .map_err(map_async_error)
}

/// Value written to `search_vector_chunks.vector_index` for the local backend.
///
/// The reference stores `sqlite-vec` there and keeps vectors in a `vec0` table; we
/// keep the column so the metadata stays comparable, but the vectors live in
/// `search_vector_embeddings` as BLOBs and are scored in Rust.
pub const VECTOR_INDEX_NAME: &str = "blob";

/// Per-connection SQLite profile, mirroring the reference's `_configure_sqlite_connection`.
///
/// The reference applies this to *every* connection through SQLAlchemy's `connect` event
/// (`basic_memory/db.py`): WAL for file-backed databases, `busy_timeout=10000`,
/// `synchronous=NORMAL`, `cache_size=-64000`, `temp_store=MEMORY`, and
/// `wal_autocheckpoint=1000`.
///
/// Two of these change behavior. WAL is the load-bearing one: under the default rollback
/// journal a reader that reopens the index in a loop (the watcher tests, or an editor
/// sync polling alongside `mcp`) can hold `SHARED` across the writer's commit and starve
/// it past its timeout, which is how the intermittent `database is locked` failure in
/// `tests/obsidian_compatibility.rs` was produced. `busy_timeout` is parity rather than a
/// fix — rusqlite already installs a 5 s default (`sqlite3_busy_timeout(db, 5000)` in
/// `inner_connection.rs`) and the reference asks for 10.
const BUSY_TIMEOUT_MS: i64 = 10_000;
const CACHE_SIZE_KIB: i64 = -64_000;
const WAL_AUTOCHECKPOINT_PAGES: i64 = 1_000;

/// Encode one vector as little-endian `f32` bytes.
pub fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Decode little-endian `f32` bytes written by [`encode_vector`].
pub fn decode_vector(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Reference `find_related` SQLite query (Basic Memory 0.23.2), with `{{seeds}}`
/// substituted by the seed entity ids.
///
/// Kept verbatim on purpose: `ORDER BY depth, type, id` plus `LIMIT` decides which
/// related rows survive, and the reference relies on it for its `max_related`
/// semantics.
const FIND_RELATED_SQL: &str = r"
WITH RECURSIVE entity_graph AS (
    SELECT
        e.id,
        'entity' as type,
        e.title,
        e.permalink,
        e.file_path,
        NULL as from_id,
        NULL as to_id,
        NULL as relation_type,
        NULL as to_name,
        NULL as content,
        NULL as category,
        NULL as entity_id,
        0 as depth,
        e.id as root_id,
        e.created_at,
        e.created_at as relation_date,
        0 as is_incoming,
        e.project_id as project_id,
        ',' || e.id || ',' as entity_path
    FROM entity e
    WHERE e.id IN ({{seeds}})
    AND (:since IS NULL OR e.created_at >= :since)
    AND e.project_id = :project_id

    UNION ALL

    SELECT
        r.id,
        'relation' as type,
        r.relation_type || ': ' || r.to_name as title,
        '' as permalink,
        e_from.file_path,
        r.from_id,
        r.to_id,
        r.relation_type,
        r.to_name,
        NULL as content,
        NULL as category,
        NULL as entity_id,
        eg.depth + 1,
        eg.root_id,
        e_from.created_at,
        e_from.created_at as relation_date,
        CASE WHEN r.from_id = eg.id THEN 0 ELSE 1 END as is_incoming,
        eg.project_id as project_id,
        eg.entity_path as entity_path
    FROM entity_graph eg
    JOIN relation r ON (
        eg.type = 'entity' AND
        (r.from_id = eg.id OR r.to_id = eg.id) AND
        r.project_id = eg.project_id
    )
    JOIN entity e_from ON (
        r.from_id = e_from.id
        AND (:since IS NULL OR e_from.created_at >= :since)
        AND e_from.project_id = r.project_id
    )
    WHERE eg.depth < :max_depth

    UNION ALL

    SELECT
        e.id,
        'entity' as type,
        e.title,
        CASE
            WHEN e.permalink IS NULL THEN ''
            ELSE e.permalink
        END as permalink,
        e.file_path,
        NULL as from_id,
        NULL as to_id,
        NULL as relation_type,
        NULL as to_name,
        NULL as content,
        NULL as category,
        NULL as entity_id,
        eg.depth + 1,
        eg.root_id,
        e.created_at,
        eg.relation_date,
        eg.is_incoming,
        e.project_id as project_id,
        eg.entity_path || e.id || ',' as entity_path
    FROM entity_graph eg
    JOIN entity e ON (
        eg.type = 'relation' AND
        e.id = CASE
            WHEN eg.is_incoming = 0 THEN eg.to_id
            ELSE eg.from_id
        END
        AND (:since IS NULL OR e.created_at >= :since)
    )
    WHERE eg.depth < :max_depth
    AND instr(eg.entity_path, ',' || e.id || ',') = 0
    AND (:since IS NULL OR eg.relation_date >= :since)
)
SELECT DISTINCT
    type,
    id,
    title,
    permalink,
    file_path,
    from_id,
    to_id,
    relation_type,
    to_name,
    content,
    category,
    entity_id,
    MIN(depth) as depth,
    root_id,
    created_at
FROM entity_graph
WHERE depth > 0
GROUP BY type, id, title, permalink, file_path, from_id, to_id,
         relation_type, to_name, content, category, entity_id, root_id, created_at
ORDER BY depth, type, id
LIMIT :max_results
";

/// Apply the reference's per-connection SQLite profile to a freshly opened connection.
///
/// `file_backed` mirrors the reference's `enable_wal = db_type != MEMORY`: in-memory
/// databases cannot use WAL, so the mode is left alone there.
fn configure_connection(conn: &Connection, file_backed: bool) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "cache_size", CACHE_SIZE_KIB)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "wal_autocheckpoint", WAL_AUTOCHECKPOINT_PAGES)?;
    if file_backed {
        // `PRAGMA journal_mode` reports the mode it settled on, so it cannot go through
        // `pragma_update`. SQLite keeps the previous mode on filesystems that cannot
        // support WAL; the reference tolerates that, and so do we.
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    }
    Ok(())
}

impl Store {
    fn connection(&self) -> &tokio_rusqlite::Connection {
        self.conn.as_ref().expect("the SQLite connection is open")
    }

    /// Run one closure on the `tokio-rusqlite` connection thread.
    async fn call<T>(
        &self,
        function: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        call_connection(self.connection(), function).await
    }

    /// Open (or create) an index at `path` and run migrations.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        // SQLite creates the database file but not its directory; the CLI/MCP are
        // routinely pointed at `~/.local/share/auto-memory/memory.db` on a fresh machine.
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let path = path.to_owned();
        let conn = tokio_rusqlite::Connection::open(path)
            .await
            .map_err(Error::from)?;
        Self::prepare(&conn, true).await?;
        Ok(Self { conn: Some(conn) })
    }

    /// Open a throwaway in-memory index (used by tests and short-lived CLIs).
    pub async fn open_in_memory() -> Result<Self> {
        let conn = tokio_rusqlite::Connection::open_in_memory()
            .await
            .map_err(Error::from)?;
        Self::prepare(&conn, false).await?;
        Ok(Self { conn: Some(conn) })
    }

    /// Configure a fresh connection and run the schema migrations.
    async fn prepare(conn: &tokio_rusqlite::Connection, file_backed: bool) -> Result<()> {
        call_connection(conn, move |conn| {
            configure_connection(conn, file_backed)?;
            schema::migrate(conn)?;
            Ok(())
        })
        .await
    }

    /// Close the underlying connection and wait for its SQLite thread to exit.
    pub async fn close(mut self) -> Result<()> {
        if let Some(conn) = self.conn.take() {
            conn.close().await.map_err(Error::from)?;
        }
        Ok(())
    }

    /// Read the semantic chunk inputs (search rows) for a project.
    pub async fn semantic_rows(&self, project_id: i64) -> Result<Vec<SemanticRow>> {
        self.call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, type, title, permalink, content_snippet, category, relation_type, entity_id
                   FROM search_index WHERE project_id = ?1 ORDER BY type, id",
            )?;
            let rows = statement
                .query_map([project_id], |row| {
                    Ok(SemanticRow {
                        id: row.get(0)?,
                        item_type: row.get(1)?,
                        title: row.get(2)?,
                        permalink: row.get(3)?,
                        content_snippet: row.get(4)?,
                        category: row.get(5)?,
                        relation_type: row.get(6)?,
                        entity_id: row.get(7)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        }).await
    }

    /// Run a text search against this index.
    pub async fn search_text(
        &self,
        project_id: i64,
        options: &TextSearchOptions,
    ) -> Result<SearchPage> {
        let options = options.clone();
        self.call(move |conn| search_text(conn, project_id, &options))
            .await
    }

    /// Run a closure with the underlying SQLite connection.
    pub async fn with_connection<T>(
        &self,
        function: impl FnOnce(&Connection) -> T + Send + 'static,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        self.call(move |conn| Ok(function(conn))).await
    }

    /// Schema version recorded in the index.
    pub async fn schema_version(&self) -> Result<i64> {
        self.call(|conn| {
            Ok(schema::metadata(conn, "schema_version")?
                .and_then(|value| value.parse().ok())
                .unwrap_or_default())
        })
        .await
    }

    /// Whether this SQLite build supports FTS5 virtual tables.
    pub async fn has_fts5(&self) -> bool {
        self.call(|conn| Ok(schema::has_fts5(conn)))
            .await
            .unwrap_or(false)
    }

    /// Read a metadata value recorded during indexing.
    pub async fn metadata(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_owned();
        self.call(move |conn| schema::metadata(conn, &key)).await
    }

    /// Write a metadata value (index version tracking, rebuild timestamps).
    pub async fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        let key = key.to_owned();
        let value = value.to_owned();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO index_metadata (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
        .await
    }

    /// Rows in the FTS5 `search_index` table, across every project.
    ///
    /// Only used to prove the index carries no stranded rows (an FTS table has no
    /// foreign keys, so it is the one place a delete cannot cascade).
    pub async fn search_index_count(&self) -> Result<i64> {
        self.call(|conn| {
            Ok(conn.query_row("SELECT count(*) FROM search_index", [], |row| row.get(0))?)
        })
        .await
    }

    /// Register a project (or refresh its path/permalink) and return its row id.
    pub async fn upsert_project(&self, name: &str, permalink: &str, path: &str) -> Result<i64> {
        let external_id = deterministic_uuid(&format!("project:{permalink}"));
        let name = name.to_owned();
        let permalink = permalink.to_owned();
        let path = path.to_owned();
        self.call(move |conn| {
            conn.execute(
                "INSERT INTO project (external_id, name, permalink, path) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(permalink) DO UPDATE SET name = excluded.name, path = excluded.path",
                params![external_id, name, permalink, path],
            )?;
            let id = conn.query_row(
                "SELECT id FROM project WHERE permalink = ?1",
                [permalink],
                |row| row.get(0),
            )?;
            Ok(id)
        })
        .await
    }

    /// Every registered project, ordered by name (the reference merge sorts by permalink).
    pub async fn projects(&self) -> Result<Vec<ProjectRow>> {
        self.call(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, external_id, name, permalink, path FROM project ORDER BY name",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok(ProjectRow {
                        id: row.get(0)?,
                        external_id: row.get(1)?,
                        name: row.get(2)?,
                        permalink: row.get(3)?,
                        path: row.get(4)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// Find a project by permalink.
    pub async fn project_by_permalink(&self, permalink: &str) -> Result<Option<ProjectRow>> {
        let permalink = permalink.to_owned();
        self.call(move |conn| {
            let row = conn
                .query_row(
                    "SELECT id, external_id, name, permalink, path FROM project WHERE permalink = ?1",
                    [permalink],
                    |row| {
                        Ok(ProjectRow {
                            id: row.get(0)?,
                            external_id: row.get(1)?,
                            name: row.get(2)?,
                            permalink: row.get(3)?,
                            path: row.get(4)?,
                        })
                    },
                )
                .optional()?;
            Ok(row)
        }).await
    }

    /// Insert or replace one parsed document and its semantic rows.
    ///
    /// The entity row is updated in place so ids and `created_at` stay stable
    /// across re-indexing; observations and relations are replaced atomically.
    pub async fn replace_document(
        &mut self,
        project_id: i64,
        project_permalink: &str,
        permalink: Option<&str>,
        checksum: &str,
        document: &ParsedDocument,
        timestamps: &DocumentTimestamps,
    ) -> Result<i64> {
        let project_permalink = project_permalink.to_owned();
        let permalink = permalink.map(str::to_owned);
        let checksum = checksum.to_owned();
        let document = document.clone();
        let timestamps = timestamps.clone();
        self.call(move |conn| {
            let metadata = serde_json::to_string(&document.frontmatter.metadata)?;
            let now = timeframe::now_storage_timestamp();
            let external_id =
                deterministic_uuid(&format!("{project_permalink}\u{0}{}", document.file_path));

            let tx = conn.transaction()?;
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT id FROM entity WHERE project_id = ?1 AND file_path = ?2",
                    params![project_id, document.file_path],
                    |row| row.get(0),
                )
                .optional()?;

            let entity_id = match existing {
                Some(id) => {
                    tx.execute(
                        "UPDATE entity
                        SET title = ?1, note_type = ?2, permalink = ?3, checksum = ?4,
                            entity_metadata = ?5, updated_at = ?6
                      WHERE id = ?7",
                        params![
                            document.frontmatter.title,
                            document.frontmatter.note_type,
                            permalink,
                            checksum,
                            metadata,
                            timestamps.updated_at,
                            id
                        ],
                    )?;
                    id
                }
                None => {
                    tx.execute(
                        "INSERT INTO entity
                        (external_id, project_id, title, note_type, entity_metadata,
                         content_type, permalink, file_path, checksum, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'text/markdown', ?6, ?7, ?8, ?9, ?10)",
                        params![
                            external_id,
                            project_id,
                            document.frontmatter.title,
                            document.frontmatter.note_type,
                            metadata,
                            permalink,
                            document.file_path,
                            checksum,
                            timestamps.created_at,
                            timestamps.updated_at
                        ],
                    )?;
                    tx.last_insert_rowid()
                }
            };

            tx.execute("DELETE FROM observation WHERE entity_id = ?1", [entity_id])?;
            tx.execute("DELETE FROM relation WHERE from_id = ?1", [entity_id])?;
            tx.execute("DELETE FROM search_index WHERE entity_id = ?1", [entity_id])?;

            let mut observation_ids: Vec<(i64, &crate::domain::observation::Observation)> =
                Vec::with_capacity(document.observations.len());
            for observation in &document.observations {
                let tags = serde_json::to_string(
                    &observation
                        .tags
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect::<Vec<_>>(),
                )?;
                tx.execute(
                "INSERT INTO observation (project_id, entity_id, category, content, context, tags)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    project_id,
                    entity_id,
                    observation
                        .category
                        .clone()
                        .unwrap_or_else(|| "note".to_owned()),
                    observation.content,
                    observation.context,
                    tags
                ],
            )?;
                observation_ids.push((tx.last_insert_rowid(), observation));
            }

            // Reference `RelationGenerationPublisher.publish` keys relations by
            // `(relation_type, to_name)`, keeps the first authored occurrence, and inserts
            // the survivors in lexicographic order. That ordering decides relation ids
            // within a note, so it also decides the order (and the `max_related` cut) of
            // the graph traversal.
            let mut ordered_relations: Vec<&crate::domain::relation::Relation> = Vec::new();
            let mut seen_relations: std::collections::HashSet<(&str, &str)> =
                std::collections::HashSet::new();
            for relation in &document.relations {
                if seen_relations
                    .insert((relation.relation_type.as_str(), relation.target.as_str()))
                {
                    ordered_relations.push(relation);
                }
            }
            ordered_relations.sort_by(|left, right| {
                (left.relation_type.as_str(), left.target.as_str())
                    .cmp(&(right.relation_type.as_str(), right.target.as_str()))
            });

            let mut relation_ids: Vec<(i64, &crate::domain::relation::Relation)> =
                Vec::with_capacity(ordered_relations.len());
            for relation in ordered_relations {
                tx.execute(
                    "INSERT OR IGNORE INTO relation
                    (project_id, from_id, to_id, to_name, relation_type, context)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?5)",
                    params![
                        project_id,
                        entity_id,
                        relation.target,
                        relation.relation_type.as_str(),
                        relation.context
                    ],
                )?;
                relation_ids.push((tx.last_insert_rowid(), relation));
            }

            let mut search_rows =
                Vec::with_capacity(1 + observation_ids.len() + relation_ids.len());
            search_rows.push(entity_row(
                entity_id,
                &document.frontmatter.title,
                &document.frontmatter.note_type,
                permalink.as_deref(),
                &document.file_path,
                &document.content,
                &document.frontmatter.tags,
            ));
            for (observation_id, observation) in observation_ids {
                search_rows.push(observation_row(
                    observation_id,
                    entity_id,
                    permalink.as_deref(),
                    &document.file_path,
                    observation,
                ));
            }
            for (relation_id, relation) in relation_ids {
                search_rows.push(relation_row(
                    relation_id,
                    entity_id,
                    &document.file_path,
                    &document.frontmatter.title,
                    None,
                    None,
                    entity_id,
                    None,
                    relation.relation_type.as_str(),
                ));
            }
            // insert search rows into FTS5 table
            insert_search_rows(&tx, project_id, &search_rows)?;

            tx.execute(
                "INSERT INTO index_metadata (key, value) VALUES ('last_indexed_at', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [&now],
            )?;
            tx.commit()?;
            Ok(entity_id)
        })
        .await
    }

    /// Remove one indexed document (cascades to observations and relations).
    pub async fn remove_document(&mut self, project_id: i64, file_path: &str) -> Result<()> {
        let file_path = file_path.to_owned();
        self.call(move |conn| {
            let entity_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM entity WHERE project_id = ?1 AND file_path = ?2",
                    params![project_id, file_path],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(entity_id) = entity_id {
                conn.execute("DELETE FROM search_index WHERE entity_id = ?1", [entity_id])?;
            }
            conn.execute(
                "DELETE FROM entity WHERE project_id = ?1 AND file_path = ?2",
                params![project_id, file_path],
            )?;
            Ok(())
        })
        .await
    }

    /// Unregister a project and drop its derived rows. Returns whether it existed.
    ///
    /// Only the *index* is touched: the markdown vault is the source of truth and is
    /// never deleted, so the same directory can be re-registered with `project add`.
    /// `entity`, `observation`, `relation`, and `search_vector_chunks` cascade from
    /// `project`, but `search_index` is an FTS5 table with no foreign keys, so its rows
    /// are removed explicitly — otherwise a removed project would keep answering text
    /// searches whose target rows no longer exist.
    pub async fn delete_project(&mut self, permalink: &str) -> Result<bool> {
        let Some(project) = self.project_by_permalink(permalink).await? else {
            return Ok(false);
        };
        self.call(move |conn| {
            let transaction = conn.transaction()?;
            transaction.execute(
                "DELETE FROM search_index WHERE project_id = ?1",
                [project.id],
            )?;
            transaction.execute(
                "DELETE FROM search_vector_chunks WHERE project_id = ?1",
                [project.id],
            )?;
            transaction.execute("DELETE FROM project WHERE id = ?1", [project.id])?;
            transaction.commit()?;
            Ok(true)
        })
        .await
    }

    /// Resolve relation targets to entity ids after a rebuild.
    ///
    /// Targets may be an explicit permalink, a project-relative path with or
    /// without `.md`, or a bare permalink segment; the reference resolver is more
    /// elaborate and is ported incrementally.
    pub async fn resolve_relations(&self, project_id: i64) -> Result<usize> {
        let resolved = self.call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT r.id, r.to_name FROM relation r WHERE r.project_id = ?1 AND r.to_id IS NULL",
            )?;
            let unresolved: Vec<(i64, String)> = statement
                .query_map([project_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            drop(statement);

            let mut resolved = 0;
            for (relation_id, to_name) in unresolved {
                let target = normalize_target(&to_name);
                let candidate: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM entity
                          WHERE project_id = ?1
                            AND (permalink = ?2
                                 OR file_path = ?2
                                 OR file_path = ?3
                                 OR replace(file_path, '.md', '') = ?2)
                          ORDER BY id LIMIT 1",
                        params![project_id, target, format!("{target}.md")],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(to_id) = candidate {
                    conn.execute(
                        "UPDATE relation SET to_id = ?1 WHERE id = ?2",
                        params![to_id, relation_id],
                    )?;
                    resolved += 1;
                }
            }
            Ok(resolved)
        }).await?;
        if resolved > 0 {
            self.refresh_relation_search_rows(project_id).await?;
        }
        Ok(resolved)
    }

    /// Rebuild relation-level search rows so titles reflect resolved targets.
    pub async fn refresh_relation_search_rows(&self, project_id: i64) -> Result<()> {
        self.call(move |conn| {
            #[derive(Debug)]
            struct RelationSearchSource {
                id: i64,
                from_id: i64,
                from_title: String,
                from_permalink: Option<String>,
                from_file_path: String,
                to_id: Option<i64>,
                to_title: Option<String>,
                to_permalink: Option<String>,
                to_name: String,
                relation_type: String,
            }

            let mut statement = conn.prepare(
                "SELECT r.id, r.from_id, f.title, f.permalink, f.file_path,
                        r.to_id, t.title, t.permalink, r.to_name, r.relation_type
                   FROM relation r
                   JOIN entity f ON f.id = r.from_id
                   LEFT JOIN entity t ON t.id = r.to_id
                  WHERE r.project_id = ?1",
            )?;
            let rows: Vec<RelationSearchSource> = statement
                .query_map([project_id], |row| {
                    Ok(RelationSearchSource {
                        id: row.get(0)?,
                        from_id: row.get(1)?,
                        from_title: row.get(2)?,
                        from_permalink: row.get(3)?,
                        from_file_path: row.get(4)?,
                        to_id: row.get(5)?,
                        to_title: row.get(6)?,
                        to_permalink: row.get(7)?,
                        to_name: row.get(8)?,
                        relation_type: row.get(9)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);

            conn.execute(
                "DELETE FROM search_index WHERE project_id = ?1 AND type = 'relation'",
                [project_id],
            )?;

            let mut search_rows = Vec::with_capacity(rows.len());
            for row in rows {
                let target = row
                    .to_permalink
                    .clone()
                    .unwrap_or_else(|| row.to_name.clone());
                let permalink = row.from_permalink.as_ref().map(|from| {
                    crate::domain::permalink::generate_permalink(&format!(
                        "{from}/{}/{}",
                        row.relation_type, target
                    ))
                });
                search_rows.push(relation_row(
                    row.id,
                    row.from_id,
                    &row.from_file_path,
                    &row.from_title,
                    row.to_title.as_deref(),
                    permalink.as_deref(),
                    row.from_id,
                    row.to_id,
                    &row.relation_type,
                ));
            }
            insert_search_rows(conn, project_id, &search_rows)
        })
        .await
    }

    /// Fetch one entity by internal row id.
    pub async fn entity_by_id(&self, entity_id: i64) -> Result<Option<EntityRow>> {
        self.call(move |conn| {
            let row = conn
                .query_row(
                "SELECT id, external_id, title, note_type, permalink, file_path, checksum, entity_metadata
                   FROM entity WHERE id = ?1",
                [entity_id],
                map_entity_row,
            )
            .optional()?;
            Ok(row)
        }).await
    }

    /// Fetch one entity by project-relative file path.
    pub async fn entity_by_file_path(
        &self,
        project_id: i64,
        file_path: &str,
    ) -> Result<Option<EntityRow>> {
        let file_path = file_path.to_owned();
        self.call(move |conn| {
            let row = conn
                .query_row(
                    "SELECT id, external_id, title, note_type, permalink, file_path, checksum, entity_metadata
                       FROM entity WHERE project_id = ?1 AND file_path = ?2",
                    params![project_id, file_path],
                    map_entity_row,
                )
                .optional()?;
            Ok(row)
        }).await
    }

    /// Fetch one entity by permalink.
    pub async fn entity_by_permalink(
        &self,
        project_id: i64,
        permalink: &str,
    ) -> Result<Option<EntityRow>> {
        let permalink = permalink.to_owned();
        self.call(move |conn| {
            let row = conn
                .query_row(
                    "SELECT id, external_id, title, note_type, permalink, file_path, checksum, entity_metadata
                       FROM entity WHERE project_id = ?1 AND permalink = ?2",
                    params![project_id, permalink],
                    |row| {
                        let metadata: Option<String> = row.get(7)?;
                        Ok(EntityRow {
                            id: row.get(0)?,
                            external_id: row.get(1)?,
                            title: row.get(2)?,
                            note_type: row.get(3)?,
                            permalink: row.get(4)?,
                            file_path: row.get(5)?,
                            checksum: row.get(6)?,
                            metadata: metadata
                                .and_then(|raw| serde_json::from_str::<Map<String, Value>>(&raw).ok())
                                .unwrap_or_default(),
                        })
                    },
                )
                .optional()?;
            Ok(row)
        }).await
    }

    /// Entities with an exactly matching title, shortest path first.
    ///
    /// Mirrors `EntityRepository.get_by_title`, which link resolution uses to break
    /// ties between same-titled notes in different folders.
    pub async fn entities_by_title(&self, project_id: i64, title: &str) -> Result<Vec<EntityRow>> {
        let title = title.to_owned();
        self.call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, external_id, title, note_type, permalink, file_path, checksum, entity_metadata
                   FROM entity WHERE project_id = ?1 AND title = ?2
                  ORDER BY length(file_path), file_path",
            )?;
            let rows = statement
                .query_map(params![project_id, title], map_entity_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        }).await
    }

    /// Observations owned by one entity.
    pub async fn observations_for_entity(&self, entity_id: i64) -> Result<Vec<ObservationRow>> {
        self.call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, entity_id, category, content, context, tags FROM observation
                  WHERE entity_id = ?1 ORDER BY id",
            )?;
            let rows = statement
                .query_map([entity_id], |row| {
                    let tags: String = row.get(5)?;
                    Ok(ObservationRow {
                        id: row.get(0)?,
                        entity_id: row.get(1)?,
                        category: row.get(2)?,
                        content: row.get(3)?,
                        context: row.get(4)?,
                        tags: serde_json::from_str::<Vec<String>>(&tags).unwrap_or_default(),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// Note body recorded for an entity (frontmatter removed).
    pub async fn entity_content(&self, entity_id: i64) -> Option<String> {
        self.call(move |conn| {
            Ok(conn.query_row(
                "SELECT content_snippet FROM search_index
                  WHERE entity_id = ?1 AND type = 'entity' LIMIT 1",
                [entity_id],
                |row| row.get::<_, Option<String>>(0),
            ))
        })
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .flatten()
    }

    /// Indexed creation timestamp for an entity.
    pub async fn entity_created_at(&self, entity_id: i64) -> Result<Option<String>> {
        self.call(move |conn| {
            let value = conn
                .query_row(
                    "SELECT created_at FROM entity WHERE id = ?1",
                    [entity_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            Ok(value)
        })
        .await
    }

    /// Titles and external ids for a set of entity ids.
    ///
    /// Context hydration looks entities up across projects, matching
    /// `find_by_ids_for_hydration(..., include_cross_project=True)`.
    pub async fn entity_titles_and_external_ids(
        &self,
        entity_ids: &[i64],
    ) -> Result<HashMap<i64, (String, String)>> {
        if entity_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let entity_ids = entity_ids.to_vec();
        self.call(move |conn| {
            let placeholders = vec!["?"; entity_ids.len()].join(", ");
            let sql =
                format!("SELECT id, title, external_id FROM entity WHERE id IN ({placeholders})");
            let mut statement = conn.prepare(&sql)?;
            let rows =
                statement.query_map(rusqlite::params_from_iter(entity_ids.iter()), |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
            let mut lookup = HashMap::new();
            for row in rows {
                let (id, title, external_id) = row?;
                lookup.insert(id, (title, external_id));
            }
            Ok(lookup)
        })
        .await
    }

    /// Vectors already stored for a project, keyed by chunk key.
    ///
    /// The reference reuses a chunk's vector while its `source_hash` is unchanged
    /// (`SQLiteVecIndex.upsert` matches on `(entity_id, chunk_key, source_hash)`), so
    /// callers can skip re-embedding unchanged text.
    pub async fn vector_embeddings(
        &self,
        project_id: i64,
        model: &str,
    ) -> Result<HashMap<String, (String, Vec<f32>)>> {
        let model = model.to_owned();
        self.call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT c.chunk_key, c.source_hash, e.embedding
                   FROM search_vector_chunks c
                   JOIN search_vector_embeddings e ON e.rowid = c.id
                  WHERE c.project_id = ?1 AND c.embedding_model = ?2
                    AND c.embedding_status = 'ready' AND e.source_hash = c.source_hash",
            )?;
            let rows = statement.query_map(params![project_id, model], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })?;
            let mut embeddings = HashMap::new();
            for row in rows {
                let (chunk_key, source_hash, blob) = row?;
                embeddings.insert(chunk_key, (source_hash, decode_vector(&blob)));
            }
            Ok(embeddings)
        })
        .await
    }

    /// Replace a project's vector index with `rows`.
    ///
    /// The reference maintains the same rows incrementally (pending → ready across
    /// worker passes); replacing them transactionally is the local equivalent and
    /// keeps changed chunks from leaving stale vectors behind.
    pub async fn replace_vector_index(
        &mut self,
        project_id: i64,
        model: &str,
        rows: &[VectorRow],
    ) -> Result<usize> {
        let model = model.to_owned();
        let rows = rows.to_vec();
        let row_count = rows.len();
        self.call(move |conn| {
            let now = timeframe::now_storage_timestamp();
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM search_vector_chunks WHERE project_id = ?1",
                [project_id],
            )?;
            {
                let mut insert_chunk = tx.prepare(
                    "INSERT INTO search_vector_chunks
                        (entity_id, project_id, chunk_key, chunk_text, source_hash,
                         entity_fingerprint, embedding_model, vector_index, embedding_status,
                         updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'ready', ?9)",
                )?;
                let mut insert_embedding = tx.prepare(
                    "INSERT INTO search_vector_embeddings (rowid, embedding, source_hash)
                     VALUES (?1, ?2, ?3)",
                )?;
                for row in &rows {
                    insert_chunk.execute(params![
                        row.entity_id,
                        project_id,
                        row.chunk_key,
                        row.chunk_text,
                        row.source_hash,
                        row.entity_fingerprint,
                        model,
                        VECTOR_INDEX_NAME,
                        now
                    ])?;
                    let rowid = tx.last_insert_rowid();
                    insert_embedding.execute(params![
                        rowid,
                        encode_vector(&row.embedding),
                        row.source_hash
                    ])?;
                }
            }
            tx.commit()?;
            Ok(row_count)
        })
        .await
    }

    /// Stored chunks and vectors for one project (vector-search candidates).
    pub async fn vector_chunks(&self, project_id: i64, model: &str) -> Result<Vec<VectorChunkRow>> {
        let model = model.to_owned();
        self.call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT c.id, c.entity_id, c.chunk_key, c.chunk_text, c.source_hash, e.embedding
                   FROM search_vector_chunks c
                   JOIN search_vector_embeddings e ON e.rowid = c.id
                  WHERE c.project_id = ?1 AND c.vector_index = ?2
                    AND c.embedding_status = 'ready' AND c.embedding_model = ?3
                    AND e.source_hash = c.source_hash
                  ORDER BY c.entity_id, c.chunk_key",
            )?;
            let rows =
                statement.query_map(params![project_id, VECTOR_INDEX_NAME, model], |row| {
                    Ok(VectorChunkRow {
                        id: row.get(0)?,
                        entity_id: row.get(1)?,
                        chunk_key: row.get(2)?,
                        chunk_text: row.get(3)?,
                        source_hash: row.get(4)?,
                        embedding: decode_vector(&row.get::<_, Vec<u8>>(5)?),
                    })
                })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// `search_index` rows for the given ids, in the reference hydration shape.
    ///
    /// Ids collide across row types, so callers key the results by `(type, id)`.
    pub async fn search_rows_by_ids(
        &self,
        project_id: i64,
        ids: &[i64],
    ) -> Result<Vec<SearchRowView>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = ids.to_vec();
        self.call(move |conn| {
            let placeholders = vec!["?"; ids.len()].join(", ");
            let sql = format!(
                "SELECT id, title, type, permalink, file_path, content_snippet, metadata,
                        entity_id, category, relation_type, updated_at
                   FROM search_index
                  WHERE project_id = ? AND id IN ({placeholders})"
            );
            let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(ids.len() + 1);
            values.push(Box::new(project_id));
            for id in ids {
                values.push(Box::new(id));
            }
            let mut statement = conn.prepare(&sql)?;
            let rows = statement.query_map(
                rusqlite::params_from_iter(values.iter().map(std::convert::AsRef::as_ref)),
                |row| {
                    Ok(SearchRowView {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        item_type: row.get(2)?,
                        permalink: row.get(3)?,
                        file_path: row.get(4)?,
                        content_snippet: row.get(5)?,
                        metadata: row.get(6)?,
                        entity_id: row.get(7)?,
                        category: row.get(8)?,
                        relation_type: row.get(9)?,
                        updated_at: row.get(10)?,
                    })
                },
            )?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }

    /// `(permalink, external_id)` for the given entity ids.
    pub async fn entity_permalinks_and_external_ids(
        &self,
        entity_ids: &[i64],
    ) -> Result<HashMap<i64, (Option<String>, String)>> {
        if entity_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let entity_ids = entity_ids.to_vec();
        self.call(move |conn| {
            let mut unique = entity_ids;
            unique.sort_unstable();
            unique.dedup();
            let placeholders = vec!["?"; unique.len()].join(", ");
            let sql = format!(
                "SELECT id, permalink, external_id FROM entity WHERE id IN ({placeholders})"
            );
            let mut statement = conn.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(unique.iter()), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            let mut lookup = HashMap::new();
            for row in rows {
                let (id, permalink, external_id) = row?;
                lookup.insert(id, (permalink, external_id));
            }
            Ok(lookup)
        })
        .await
    }

    /// Traverse the relation graph from `roots`, mirroring reference `find_related`.
    ///
    /// The query is a verbatim port of `ContextService._build_sqlite_query`
    /// (Basic Memory 0.23.2): a recursive CTE that emits both the relations and
    /// the entities reached at each step, then keeps the shallowest occurrence of
    /// every row and returns the first `max_results` in `(depth, type, id)` order.
    /// `since` filters rows exactly where the reference does — the seed entity,
    /// the relation source, and the connected entity's `created_at`, plus the
    /// relation's own timestamp for the entity hop.
    pub async fn find_related(
        &self,
        project_id: i64,
        roots: &[i64],
        depth: u32,
        max_results: u32,
        since: Option<&str>,
    ) -> Result<Vec<RelatedRow>> {
        if roots.is_empty() || max_results == 0 {
            return Ok(Vec::new());
        }
        let roots = roots.to_vec();
        let since = since.map(str::to_owned);
        self.call(move |conn| {
            let seeds = roots
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            let sql = FIND_RELATED_SQL.replace("{{seeds}}", &seeds);
            let mut statement = conn.prepare(&sql)?;
            let rows = statement
                .query_map(
                    named_params! {
                        ":since": since,
                        ":project_id": project_id,
                        ":max_depth": i64::from(depth) * 2,
                        ":max_results": max_results,
                    },
                    |row| {
                        Ok(RelatedRow {
                            item_type: row.get("type")?,
                            id: row.get("id")?,
                            title: row.get("title")?,
                            permalink: row.get("permalink")?,
                            file_path: row.get("file_path")?,
                            from_id: row.get("from_id")?,
                            to_id: row.get("to_id")?,
                            relation_type: row.get("relation_type")?,
                            to_name: row.get("to_name")?,
                            depth: row.get("depth")?,
                            root_id: row.get("root_id")?,
                            created_at: row.get("created_at")?,
                        })
                    },
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// Every relation edge in a project with entity titles resolved.
    pub async fn relation_edges(&self) -> Result<Vec<crate::graph::RelationEdge>> {
        self.call(|conn| {
            let mut statement = conn.prepare(
                "SELECT r.id, r.from_id, r.to_id, r.to_name, r.relation_type, r.context,
                        f.title, f.permalink, f.file_path, f.external_id,
                        t.title, t.permalink, t.external_id
                   FROM relation r
                   JOIN entity f ON f.id = r.from_id
                   LEFT JOIN entity t ON t.id = r.to_id
                  ORDER BY r.id",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok(crate::graph::RelationEdge {
                        id: row.get(0)?,
                        from_id: row.get(1)?,
                        to_id: row.get(2)?,
                        to_name: row.get(3)?,
                        relation_type: row.get(4)?,
                        context: row.get(5)?,
                        from_title: row.get(6)?,
                        from_permalink: row.get(7)?,
                        from_file_path: row.get(8)?,
                        from_external_id: row.get(9)?,
                        to_title: row.get(10)?,
                        to_permalink: row.get(11)?,
                        to_external_id: row.get(12)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// Every indexed file path for a project (used by reconciliation).
    pub async fn file_paths(&self, project_id: i64) -> Result<Vec<String>> {
        self.call(move |conn| {
            let mut statement = conn
                .prepare("SELECT file_path FROM entity WHERE project_id = ?1 ORDER BY file_path")?;
            let rows = statement
                .query_map([project_id], |row| row.get(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// Move an indexed document to a new path, optionally updating its permalink.
    ///
    /// Returns `true` when a row was moved. The caller re-indexes the new path
    /// afterwards so content, checksum, and semantic rows are refreshed.
    pub async fn move_document(
        &mut self,
        project_id: i64,
        from: &str,
        to: &str,
        permalink: Option<&str>,
    ) -> Result<bool> {
        let from = from.to_owned();
        let to = to.to_owned();
        let permalink = permalink.map(str::to_owned);
        self.call(move |conn| {
            let tx = conn.transaction()?;
            let updated = match permalink {
                Some(permalink) => tx.execute(
                    "UPDATE entity SET file_path = ?1, permalink = ?2 WHERE project_id = ?3 AND file_path = ?4",
                    params![to, permalink, project_id, from],
                )?,
                None => tx.execute(
                    "UPDATE entity SET file_path = ?1 WHERE project_id = ?2 AND file_path = ?3",
                    params![to, project_id, from],
                )?,
            };
            tx.commit()?;
            Ok(updated > 0)
        }).await
    }

    /// Entities under a directory prefix, in the reference repository's row order.
    ///
    /// Ports `EntityRepository.find_by_directory_prefix`: an empty prefix (or `/`)
    /// returns every entity, anything else matches `file_path LIKE '<prefix>/%'`. The
    /// pattern is passed through to SQLite, so `_`/`%` keep their `LIKE` meaning and
    /// ASCII matching stays case-insensitive — the same quirks the reference inherits.
    pub async fn directory_rows(
        &self,
        project_id: i64,
        directory_prefix: &str,
    ) -> Result<Vec<DirectoryEntityRow>> {
        let prefix = directory_prefix.trim_matches('/').to_owned();
        self.call(move |conn| {
            let columns = "SELECT id, file_path, title, permalink, external_id, note_type, \
                           content_type, updated_at FROM entity WHERE project_id = ?1";
            let mut rows = Vec::new();
            if prefix.is_empty() {
                let mut statement = conn.prepare(&format!("{columns} ORDER BY id"))?;
                let mapped = statement.query_map([project_id], directory_row)?;
                for row in mapped {
                    rows.push(row?);
                }
                return Ok(rows);
            }
            let pattern = format!("{prefix}/%");
            let mut statement =
                conn.prepare(&format!("{columns} AND file_path LIKE ?2 ORDER BY id"))?;
            let mapped = statement.query_map(params![project_id, pattern], directory_row)?;
            for row in mapped {
                rows.push(row?);
            }
            Ok(rows)
        })
        .await
    }

    /// List entities for a project, ordered by file path.
    pub async fn entities(&self, project_id: i64) -> Result<Vec<EntityRow>> {
        self.call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, external_id, title, note_type, permalink, file_path, checksum, entity_metadata
                   FROM entity WHERE project_id = ?1 ORDER BY file_path",
            )?;
            let rows = statement
                .query_map([project_id], |row| {
                    let metadata: Option<String> = row.get(7)?;
                    Ok(EntityRow {
                        id: row.get(0)?,
                        external_id: row.get(1)?,
                        title: row.get(2)?,
                        note_type: row.get(3)?,
                        permalink: row.get(4)?,
                        file_path: row.get(5)?,
                        checksum: row.get(6)?,
                        metadata: metadata
                            .and_then(|raw| serde_json::from_str::<Map<String, Value>>(&raw).ok())
                            .unwrap_or_default(),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        }).await
    }

    /// List observations for a project, ordered by entity id then insertion order.
    pub async fn observations(&self, project_id: i64) -> Result<Vec<ObservationRow>> {
        self.call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id, entity_id, category, content, context, tags
                   FROM observation WHERE project_id = ?1 ORDER BY entity_id, id",
            )?;
            let rows = statement
                .query_map([project_id], |row| {
                    let tags: String = row.get(5)?;
                    Ok(ObservationRow {
                        id: row.get(0)?,
                        entity_id: row.get(1)?,
                        category: row.get(2)?,
                        content: row.get(3)?,
                        context: row.get(4)?,
                        tags: serde_json::from_str::<Vec<String>>(&tags).unwrap_or_default(),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// List relations for a project, ordered by source entity then insertion order.
    pub async fn relations(&self, project_id: i64) -> Result<Vec<RelationRow>> {
        self.call(move |conn| {
            let mut statement = conn.prepare(
                "SELECT from_id, to_id, to_name, relation_type, context
                   FROM relation WHERE project_id = ?1 ORDER BY from_id, id",
            )?;
            let rows = statement
                .query_map([project_id], |row| {
                    Ok(RelationRow {
                        from_id: row.get(0)?,
                        to_id: row.get(1)?,
                        to_name: row.get(2)?,
                        relation_type: row.get(3)?,
                        context: row.get(4)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// Row counts for a project.
    pub async fn counts(&self, project_id: i64) -> Result<Counts> {
        self.call(move |conn| {
            let count = |table: &str| -> Result<i64> {
                let sql = format!("SELECT count(*) FROM {table} WHERE project_id = ?1");
                Ok(conn.query_row(&sql, [project_id], |row| row.get(0))?)
            };
            Ok(Counts {
                entities: count("entity")?,
                observations: count("observation")?,
                relations: count("relation")?,
            })
        })
        .await
    }
}

fn normalize_target(to_name: &str) -> &str {
    to_name
        .split_once('|')
        .map_or(to_name, |(target, _)| target)
        .trim()
}

/// Map one `entity` result row.
fn map_entity_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EntityRow> {
    let metadata: Option<String> = row.get(7)?;
    Ok(EntityRow {
        id: row.get(0)?,
        external_id: row.get(1)?,
        title: row.get(2)?,
        note_type: row.get(3)?,
        permalink: row.get(4)?,
        file_path: row.get(5)?,
        checksum: row.get(6)?,
        metadata: metadata
            .and_then(|raw| serde_json::from_str::<Map<String, Value>>(&raw).ok())
            .unwrap_or_default(),
    })
}

/// Map one `directory_rows` result row.
fn directory_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DirectoryEntityRow> {
    Ok(DirectoryEntityRow {
        id: row.get(0)?,
        file_path: row.get(1)?,
        title: row.get(2)?,
        permalink: row.get(3)?,
        external_id: row.get(4)?,
        note_type: row.get(5)?,
        content_type: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

/// Deterministic UUID-v4-shaped identifier derived from a stable key.
pub fn deterministic_uuid(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 10
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        // `hex` is ASCII, so byte slicing is safe.
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// SHA-256 checksum of raw file bytes (lowercase hex).
pub fn checksum_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Insert derived FTS5 rows for one document.
///
/// Every row carries the timestamps of the entity that owns it, mirroring
/// `SearchService.index_entity_markdown` (`created_at=entity.created_at`).
fn insert_search_rows(
    conn: &Connection,
    project_id: i64,
    rows: &[SearchIndexWriteRow],
) -> Result<()> {
    let mut timestamps: HashMap<i64, (String, String)> = HashMap::new();
    {
        let mut lookup = conn.prepare("SELECT created_at, updated_at FROM entity WHERE id = ?1")?;
        for row in rows {
            if timestamps.contains_key(&row.entity_id) {
                continue;
            }
            let value = lookup
                .query_row([row.entity_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .optional()?;
            timestamps.insert(row.entity_id, value.unwrap_or_else(current_timestamps));
        }
    }

    let mut statement = conn.prepare(
        "INSERT INTO search_index
            (id, title, content_stems, content_snippet, permalink, file_path, type,
             project_id, from_id, to_id, relation_type, entity_id, category, metadata,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
    )?;
    for row in rows {
        let (created_at, updated_at) = timestamps
            .get(&row.entity_id)
            .cloned()
            .unwrap_or_else(current_timestamps);
        statement.execute(params![
            row.id,
            row.title,
            row.content_stems,
            row.content_snippet,
            row.permalink,
            row.file_path,
            row.item_type,
            project_id,
            row.from_id,
            row.to_id,
            row.relation_type,
            row.entity_id,
            row.category,
            row.metadata,
            created_at,
            updated_at
        ])?;
    }
    Ok(())
}

/// Local "now" in the reference storage layout, used when an entity row is absent.
fn current_timestamps() -> (String, String) {
    let now = timeframe::now_storage_timestamp();
    (now.clone(), now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parse_document;

    async fn store() -> Store {
        Store::open_in_memory().await.expect("store")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn schema_version_and_fts5_are_available() {
        let store = store().await;
        assert_eq!(
            store.schema_version().await.expect("version"),
            schema::SCHEMA_VERSION
        );
        assert!(store.has_fts5().await, "bundled SQLite must provide FTS5");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replacing_a_document_is_idempotent() {
        let mut store = store().await;
        let project_id = store
            .upsert_project("oracle", "oracle", "/tmp/vault")
            .await
            .expect("project");
        let document = parse_document(
            "notes/simple.md",
            "# Simple\n\n- [note] baseline\n\nSee [[projects/alpha]].\n",
        )
        .expect("parse");

        store
            .replace_document(
                project_id,
                "oracle",
                Some("oracle/notes/simple"),
                "abc",
                &document,
                &DocumentTimestamps::now(),
            )
            .await
            .expect("first");
        store
            .replace_document(
                project_id,
                "oracle",
                Some("oracle/notes/simple"),
                "abc",
                &document,
                &DocumentTimestamps::now(),
            )
            .await
            .expect("second");

        let counts = store.counts(project_id).await.expect("counts");
        assert_eq!(counts.entities, 1);
        assert_eq!(counts.observations, 1);
        assert_eq!(counts.relations, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn removing_a_document_cascades() {
        let mut store = store().await;
        let project_id = store
            .upsert_project("oracle", "oracle", "/tmp/vault")
            .await
            .expect("project");
        let document = parse_document("a.md", "- [fact] x\n").expect("parse");
        store
            .replace_document(
                project_id,
                "oracle",
                Some("oracle/a"),
                "abc",
                &document,
                &DocumentTimestamps::now(),
            )
            .await
            .expect("insert");
        store
            .remove_document(project_id, "a.md")
            .await
            .expect("remove");
        let counts = store.counts(project_id).await.expect("counts");
        assert_eq!(counts.entities, 0);
        assert_eq!(counts.observations, 0);
    }

    #[test]
    fn external_ids_are_deterministic() {
        // The exact spelling is observable (goldens canonicalize it, the CLI prints it),
        // so pin one value per id shape, not just stability.
        assert_eq!(
            deterministic_uuid("project:oracle"),
            "449735f3-0c61-4284-841c-18b4276ff56f"
        );
        assert_eq!(
            deterministic_uuid("oracle:notes/a"),
            "30913bc8-765e-44b1-b19d-0c23e825aab9"
        );
        assert_ne!(
            deterministic_uuid("oracle:notes/a"),
            deterministic_uuid("oracle:notes/b")
        );
    }

    #[test]
    fn checksum_matches_reference_sha256() {
        assert_eq!(
            checksum_bytes(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
