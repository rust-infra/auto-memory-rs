//! Full rebuild: scan a vault, parse every markdown note, and replace the index.

use std::path::Path;

use serde::Serialize;

use crate::error::Result;
use crate::indexing::document::{
    LoadOutcome, PermalinkPolicy, load_indexed_document, markdown_files,
};
use crate::storage::Store;

/// Options that change how permalinks are generated during a rebuild.
#[derive(Debug, Clone)]
pub struct RebuildOptions {
    /// Project permalink used as the generated-permalink prefix.
    pub project_permalink: String,
    /// Whether generated permalinks are prefixed with the project slug
    /// (reference default: `true`).
    pub include_project_in_permalink: bool,
}

impl RebuildOptions {
    /// Options mirroring the reference defaults for `project`
    /// (`permalinks_include_project = true`).
    pub fn new(project_permalink: impl Into<String>) -> Self {
        Self {
            project_permalink: project_permalink.into(),
            include_project_in_permalink: true,
        }
    }

    /// The permalink policy implied by these options.
    pub fn policy(&self) -> PermalinkPolicy {
        PermalinkPolicy {
            project_permalink: self.project_permalink.clone(),
            include_project: self.include_project_in_permalink,
        }
    }
}

/// Summary of one full rebuild pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RebuildReport {
    /// Markdown files discovered in the vault.
    pub files_seen: usize,
    /// Files indexed into the store.
    pub documents_indexed: usize,
    /// Files skipped (unreadable or malformed frontmatter).
    pub documents_skipped: usize,
    /// Stale index rows removed because their file no longer exists.
    pub documents_removed: usize,
    /// Observation rows written.
    pub observations: usize,
    /// Relation rows written.
    pub relations: usize,
    /// Relation targets resolved to existing entities.
    pub relations_resolved: usize,
}

/// Rebuild the index for `root`.
///
/// Existing rows for the same `file_path` are updated in place; rows whose file
/// disappeared from disk are pruned, so a full rebuild always converges on the
/// current vault contents.
pub fn rebuild_vault(
    store: &mut Store,
    project_id: i64,
    root: &Path,
    options: &RebuildOptions,
) -> Result<RebuildReport> {
    let policy = options.policy();
    let files = markdown_files(root);
    let mut report = RebuildReport {
        files_seen: files.len(),
        documents_indexed: 0,
        documents_skipped: 0,
        documents_removed: 0,
        observations: 0,
        relations: 0,
        relations_resolved: 0,
    };
    let mut seen_paths = Vec::with_capacity(files.len());

    for (relative_path, _absolute) in &files {
        seen_paths.push(relative_path.clone());
        match load_indexed_document(root, relative_path, &policy)? {
            LoadOutcome::Loaded(indexed) => {
                report.observations += indexed.document.observations.len();
                report.relations += indexed.document.relations.len();
                store.replace_document(
                    project_id,
                    &options.project_permalink,
                    Some(&indexed.permalink),
                    &indexed.checksum,
                    &indexed.document,
                    &indexed.timestamps,
                )?;
                report.documents_indexed += 1;
            }
            // Reference behavior: malformed YAML files are dropped by the indexer.
            LoadOutcome::MalformedFrontmatter | LoadOutcome::Missing => {
                report.documents_skipped += 1;
            }
        }
    }

    for stale in store.file_paths(project_id)? {
        if !seen_paths.contains(&stale) {
            store.remove_document(project_id, &stale)?;
            report.documents_removed += 1;
        }
    }

    report.relations_resolved = store.resolve_relations(project_id)?;
    store.set_metadata("parser_version", env!("CARGO_PKG_VERSION"))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_to_project_prefixed_permalinks() {
        let options = RebuildOptions::new("oracle");
        assert!(options.include_project_in_permalink);
        assert_eq!(options.policy().project_permalink, "oracle");
    }
}
