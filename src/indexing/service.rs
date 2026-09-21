//! Incremental indexing: single-file index/remove/move plus vault reconciliation.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Serialize;

use crate::error::Result;
use crate::indexing::document::{
    IndexedDocument, LoadOutcome, PermalinkPolicy, load_indexed_document, markdown_files,
};
use crate::indexing::rebuild::{RebuildOptions, RebuildReport, rebuild_vault};
use crate::search::chunking::{build_chunk_records, entity_fingerprint};
use crate::search::embedding::EmbeddingProvider;
use crate::storage::checksum_bytes;
use crate::storage::{Store, VectorRow};

/// Options for incremental indexing.
#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// Permalink policy for newly seen or moved documents.
    pub permalink: PermalinkPolicy,
    /// Whether a move recomputes the permalink (reference default: `false`).
    pub update_permalinks_on_move: bool,
}

impl IndexOptions {
    /// Options mirroring the reference defaults for `project`.
    pub fn new(project_permalink: impl Into<String>) -> Self {
        Self {
            permalink: PermalinkPolicy::new(project_permalink),
            update_permalinks_on_move: false,
        }
    }

    /// Override the move-permalink behavior.
    pub fn with_update_permalinks_on_move(mut self, value: bool) -> Self {
        self.update_permalinks_on_move = value;
        self
    }
}

/// Result of indexing one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum IndexOutcome {
    /// The document was parsed and written to the index.
    Indexed,
    /// The stored checksum already matches; no write was needed.
    Unchanged,
    /// Frontmatter is malformed; the reference indexer skips the file.
    SkippedMalformed,
    /// The file is not readable or does not exist.
    Missing,
}

/// Summary of one reconciliation pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct ReconcileReport {
    /// Files present on disk but missing from the index.
    pub added: usize,
    /// Files whose checksum changed.
    pub updated: usize,
    /// Files whose checksum was already current.
    pub unchanged: usize,
    /// Files skipped because they are malformed or unreadable.
    pub skipped: usize,
    /// Index rows removed because the file disappeared.
    pub removed: usize,
    /// Relation targets resolved during this pass.
    pub relations_resolved: usize,
}

/// Summary of one embedding reindex pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct EmbeddingReport {
    /// Chunks in the current index.
    pub chunks: usize,
    /// Chunks whose stored vector was reused (unchanged `source_hash`).
    pub reused: usize,
    /// Chunks embedded during this pass.
    pub embedded: usize,
}

/// Incremental indexer bound to one project and vault root.
pub struct IndexService<'a> {
    store: &'a mut Store,
    project_id: i64,
    root: PathBuf,
    options: IndexOptions,
}

impl<'a> IndexService<'a> {
    /// Create a service for one project.
    pub fn new(
        store: &'a mut Store,
        project_id: i64,
        root: impl Into<PathBuf>,
        options: IndexOptions,
    ) -> Self {
        Self {
            store,
            project_id,
            root: root.into(),
            options,
        }
    }

    /// Index one file, skipping the write when its checksum is unchanged.
    pub async fn index_file(&mut self, relative_path: &str) -> Result<IndexOutcome> {
        self.index_file_inner(relative_path, true).await
    }

    /// Checksum currently stored for one indexed path.
    pub async fn entity_checksum(&self, relative_path: &str) -> Result<Option<String>> {
        Ok(self
            .store
            .entity_by_file_path(self.project_id, relative_path)
            .await?
            .and_then(|entity| entity.checksum))
    }

    /// Checksum of one file on disk, or `None` when it does not exist.
    pub fn file_checksum(&self, relative_path: &str) -> Result<Option<String>> {
        match std::fs::read(self.root.join(relative_path)) {
            Ok(bytes) => Ok(Some(checksum_bytes(&bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Index one file; relation resolution is deferred when `resolve` is false so
    /// a reconciliation pass can resolve every target once at the end.
    async fn index_file_inner(
        &mut self,
        relative_path: &str,
        resolve: bool,
    ) -> Result<IndexOutcome> {
        let policy = self.options.permalink.clone();
        let outcome = load_indexed_document(&self.root, relative_path, &policy);
        let indexed = match outcome? {
            LoadOutcome::Loaded(indexed) => indexed,
            LoadOutcome::MalformedFrontmatter => return Ok(IndexOutcome::SkippedMalformed),
            LoadOutcome::Missing => return Ok(IndexOutcome::Missing),
        };

        if let Some(existing) = self
            .store
            .entity_by_file_path(self.project_id, relative_path)
            .await?
        {
            if existing.checksum.as_deref() == Some(indexed.checksum.as_str()) {
                return Ok(IndexOutcome::Unchanged);
            }
        }
        self.write(&indexed, resolve).await?;
        Ok(IndexOutcome::Indexed)
    }

    /// Index one file even when the stored checksum matches.
    pub async fn force_index_file(&mut self, relative_path: &str) -> Result<IndexOutcome> {
        let policy = self.options.permalink.clone();
        match load_indexed_document(&self.root, relative_path, &policy)? {
            LoadOutcome::Loaded(indexed) => {
                self.write(&indexed, true).await?;
                Ok(IndexOutcome::Indexed)
            }
            LoadOutcome::MalformedFrontmatter => Ok(IndexOutcome::SkippedMalformed),
            LoadOutcome::Missing => Ok(IndexOutcome::Missing),
        }
    }

    /// Remove one document from the index. Returns whether a row was removed.
    pub async fn remove_file(&mut self, relative_path: &str) -> Result<bool> {
        let existed = self
            .store
            .entity_by_file_path(self.project_id, relative_path)
            .await?
            .is_some();
        if existed {
            self.store
                .remove_document(self.project_id, relative_path)
                .await?;
        }
        Ok(existed)
    }

    /// Move an indexed document and re-index its new contents.
    ///
    /// With `update_permalinks_on_move = false` (reference default) the existing
    /// permalink is preserved; otherwise it is recomputed from the new path.
    pub async fn move_file(&mut self, from: &str, to: &str) -> Result<IndexOutcome> {
        let policy = self.options.permalink.clone();
        let indexed = match load_indexed_document(&self.root, to, &policy)? {
            LoadOutcome::Loaded(indexed) => indexed,
            LoadOutcome::MalformedFrontmatter => return Ok(IndexOutcome::SkippedMalformed),
            LoadOutcome::Missing => return Ok(IndexOutcome::Missing),
        };

        let existing = self
            .store
            .entity_by_file_path(self.project_id, from)
            .await?;
        let permalink = match &existing {
            Some(entity) if !self.options.update_permalinks_on_move => entity.permalink.clone(),
            _ => Some(indexed.permalink.clone()),
        };

        if let Some(entity) = &existing {
            // The reference's move rewrites the destination with the *entity's* identity:
            // title, type, and the permalink policy's value, regardless of what the file
            // said before (a note without frontmatter keeps its title instead of adopting
            // the new filename). Other frontmatter keys and the body survive; the body is
            // trimmed, as the reference's writer does.
            let absolute = self.root.join(to);
            let content = std::fs::read_to_string(&absolute)?;
            let mut updates = vec![
                (
                    "title".to_owned(),
                    serde_yaml_ng::Value::String(entity.title.clone()),
                ),
                (
                    "type".to_owned(),
                    serde_yaml_ng::Value::String(entity.note_type.clone()),
                ),
            ];
            if let Some(permalink) = &permalink {
                updates.push((
                    "permalink".to_owned(),
                    serde_yaml_ng::Value::String(permalink.clone()),
                ));
            }
            let merged = crate::markdown::serialize::merge_frontmatter(&content, &updates)?;
            crate::markdown::serialize::write_atomic(&absolute, &merged)?;
            self.store
                .move_document(self.project_id, from, to, permalink.as_deref())
                .await?;
        }

        // Re-read the destination so the index sees the rewritten frontmatter.
        let indexed = match load_indexed_document(&self.root, to, &policy)? {
            LoadOutcome::Loaded(indexed) => indexed,
            LoadOutcome::MalformedFrontmatter => return Ok(IndexOutcome::SkippedMalformed),
            LoadOutcome::Missing => return Ok(IndexOutcome::Missing),
        };
        self.write_with_permalink(&indexed, permalink.as_deref(), true)
            .await?;
        Ok(IndexOutcome::Indexed)
    }

    /// Scan the vault and apply the difference to the index.
    pub async fn reconcile(&mut self) -> Result<ReconcileReport> {
        let files = markdown_files(&self.root);
        let mut report = ReconcileReport::default();
        let mut seen = HashSet::with_capacity(files.len());

        for (relative_path, _absolute) in files {
            seen.insert(relative_path.clone());
            let existed = self
                .store
                .entity_by_file_path(self.project_id, &relative_path)
                .await?
                .is_some();
            match self.index_file_inner(&relative_path, false).await? {
                IndexOutcome::Indexed if existed => report.updated += 1,
                IndexOutcome::Indexed => report.added += 1,
                IndexOutcome::Unchanged => report.unchanged += 1,
                IndexOutcome::SkippedMalformed | IndexOutcome::Missing => report.skipped += 1,
            }
        }

        for path in self.store.file_paths(self.project_id).await? {
            if !seen.contains(&path) {
                self.store.remove_document(self.project_id, &path).await?;
                report.removed += 1;
            }
        }

        report.relations_resolved = self.store.resolve_relations(self.project_id).await?;
        Ok(report)
    }

    /// Full rebuild through the shared rebuild pipeline.
    pub async fn full_rebuild(&mut self) -> Result<RebuildReport> {
        let options = RebuildOptions {
            project_permalink: self.options.permalink.project_permalink.clone(),
            include_project_in_permalink: self.options.permalink.include_project,
        };
        rebuild_vault(self.store, self.project_id, &self.root, &options).await
    }

    /// Recompute the vector index with `provider` (reference `reindex --embeddings`).
    ///
    /// Chunks whose `source_hash` is unchanged keep their stored vector, matching the
    /// reference upsert (`SQLiteVecIndex.upsert` matches on
    /// `(entity_id, chunk_key, source_hash)`); everything else is embedded and the
    /// project's vector rows are replaced transactionally.
    pub async fn reindex_embeddings(
        &mut self,
        provider: &dyn EmbeddingProvider,
    ) -> Result<EmbeddingReport> {
        let rows = self.store.semantic_rows(self.project_id).await?;
        let records = build_chunk_records(&rows);
        let existing = self
            .store
            .vector_embeddings(self.project_id, provider.model_name())
            .await?;

        // Fingerprints are per owning search row, so group the chunk records the way
        // the reference does before writing them.
        let mut fingerprints: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for row in &rows {
            let owned: Vec<_> = records
                .iter()
                .filter(|record| {
                    record
                        .chunk_key
                        .starts_with(&format!("{}:{}:", row.item_type, row.id))
                })
                .cloned()
                .collect();
            if owned.is_empty() {
                continue;
            }
            fingerprints.insert(
                format!("{}:{}", row.item_type, row.id),
                entity_fingerprint(&owned),
            );
        }

        let mut report = EmbeddingReport {
            chunks: records.len(),
            reused: 0,
            embedded: 0,
        };
        let mut pending: Vec<&crate::search::chunking::ChunkRecord> = Vec::new();
        for record in &records {
            match existing.get(&record.chunk_key) {
                Some((source_hash, _)) if *source_hash == record.source_hash => {
                    report.reused += 1;
                }
                _ => {
                    report.embedded += 1;
                    pending.push(record);
                }
            }
        }
        let pending_texts: Vec<String> = pending
            .iter()
            .map(|record| record.chunk_text.clone())
            .collect();
        let embedded = provider.embed_documents(&pending_texts)?;
        if embedded.len() != pending_texts.len() {
            return Err(crate::error::Error::Embedding {
                message: format!(
                    "provider returned {} vectors for {} chunks",
                    embedded.len(),
                    pending_texts.len()
                ),
            });
        }
        // Keyed by chunk key: several chunks can share one text, and the lookup
        // below is per chunk.
        let mut fresh: std::collections::HashMap<&str, Vec<f32>> = std::collections::HashMap::new();
        for (record, vector) in pending.iter().zip(embedded) {
            fresh.insert(record.chunk_key.as_str(), vector);
        }

        let owner_of = |chunk_key: &str| -> Option<i64> {
            let (item_type, id) = chunk_key.split_once(':')?;
            let id = id.split(':').next()?.parse::<i64>().ok()?;
            rows.iter()
                .find(|row| row.item_type == item_type && row.id == id)
                .and_then(|row| row.entity_id)
        };

        let mut vector_rows = Vec::with_capacity(records.len());
        for record in &records {
            let Some(entity_id) = owner_of(&record.chunk_key) else {
                continue;
            };
            let vector = match existing.get(&record.chunk_key) {
                Some((source_hash, vector)) if *source_hash == record.source_hash => vector.clone(),
                _ => fresh
                    .get(record.chunk_key.as_str())
                    .cloned()
                    .unwrap_or_default(),
            };
            if vector.is_empty() {
                continue;
            }
            vector_rows.push(VectorRow {
                entity_id,
                chunk_key: record.chunk_key.clone(),
                chunk_text: record.chunk_text.clone(),
                source_hash: record.source_hash.clone(),
                entity_fingerprint: fingerprints
                    .get(
                        &record
                            .chunk_key
                            .split(':')
                            .take(2)
                            .collect::<Vec<_>>()
                            .join(":"),
                    )
                    .cloned()
                    .unwrap_or_default(),
                embedding: vector,
            });
        }
        self.store
            .replace_vector_index(self.project_id, provider.model_name(), &vector_rows)
            .await?;
        Ok(report)
    }

    async fn write(&mut self, indexed: &IndexedDocument, resolve: bool) -> Result<()> {
        self.write_with_permalink(indexed, Some(&indexed.permalink), resolve)
            .await
    }

    async fn write_with_permalink(
        &mut self,
        indexed: &IndexedDocument,
        permalink: Option<&str>,
        resolve: bool,
    ) -> Result<()> {
        self.store
            .replace_document(
                self.project_id,
                &self.options.permalink.project_permalink,
                permalink,
                &indexed.checksum,
                &indexed.document,
                &indexed.timestamps,
            )
            .await?;
        if resolve {
            self.store.resolve_relations(self.project_id).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_to_reference_move_policy() {
        let options = IndexOptions::new("oracle");
        assert!(!options.update_permalinks_on_move);
        assert!(options.permalink.include_project);
    }
}
