//! Shared document loading for full rebuilds, incremental indexing, and reconcile.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;

use crate::domain::document::{DocumentTimestamps, ParsedDocument};
use crate::domain::permalink::generate_permalink;
use crate::domain::timeframe;
use crate::error::Result;
use crate::markdown::parse_document;
use crate::storage::checksum_bytes;

/// How generated permalinks are built for a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermalinkPolicy {
    /// Project permalink used as the prefix for generated permalinks.
    pub project_permalink: String,
    /// Whether generated permalinks are prefixed with the project slug.
    pub include_project: bool,
}

impl PermalinkPolicy {
    /// Reference default: generated permalinks include the project slug.
    pub fn new(project_permalink: impl Into<String>) -> Self {
        Self {
            project_permalink: project_permalink.into(),
            include_project: true,
        }
    }

    /// Build the canonical permalink for one document.
    pub fn for_document(&self, document: &ParsedDocument, relative_path: &str) -> String {
        if let Some(explicit) = &document.frontmatter.permalink {
            return explicit.as_str().to_owned();
        }
        let generated = generate_permalink(relative_path);
        if self.include_project && !self.project_permalink.is_empty() {
            format!("{}/{generated}", self.project_permalink)
        } else {
            generated
        }
    }
}

/// A parsed document ready to be written to the index.
#[derive(Debug, Clone)]
pub struct IndexedDocument {
    /// Project-relative, slash-separated path.
    pub relative_path: String,
    /// Canonical permalink (project-prefixed unless explicit in frontmatter).
    pub permalink: String,
    /// SHA-256 checksum of the raw file bytes.
    pub checksum: String,
    /// Timestamps written to the `entity` row and its search rows.
    pub timestamps: DocumentTimestamps,
    /// Parsed document.
    pub document: ParsedDocument,
}

/// Result of loading one file from disk.
#[derive(Debug, Clone)]
pub enum LoadOutcome {
    /// The file parsed and is ready to index.
    ///
    /// Boxed because `IndexedDocument` is much larger than the other variants.
    Loaded(Box<IndexedDocument>),
    /// The file exists but its frontmatter is malformed (reference indexer skips it).
    MalformedFrontmatter,
    /// The file does not exist or could not be read.
    Missing,
}

/// Read and parse one markdown file relative to `root`.
pub fn load_indexed_document(
    root: &Path,
    relative_path: &str,
    policy: &PermalinkPolicy,
) -> Result<LoadOutcome> {
    let absolute = root.join(relative_path);
    let Ok(bytes) = fs::read(&absolute) else {
        return Ok(LoadOutcome::Missing);
    };
    let file_times = file_times(&absolute);
    let Ok(content) = String::from_utf8(bytes.clone()) else {
        return Ok(LoadOutcome::Missing);
    };
    let document = parse_document(relative_path, &content)?;
    if document.frontmatter_error {
        return Ok(LoadOutcome::MalformedFrontmatter);
    }
    let permalink = policy.for_document(&document, relative_path);
    let timestamps = DocumentTimestamps::for_document(&document, file_times.0, file_times.1);
    Ok(LoadOutcome::Loaded(Box::new(IndexedDocument {
        relative_path: relative_path.to_owned(),
        permalink,
        checksum: checksum_bytes(&bytes),
        timestamps,
        document,
    })))
}

/// The timestamps a note gets when its frontmatter omits them.
///
/// Measured against the reference (0.23.2, fresh vault, file written 09:11:50.939,
/// index run 09:11:53.4): `entity.updated_at` is the file `st_mtime`, while
/// `entity.created_at` is the **row insert time** — not `st_ctime`. So the fallback for
/// `created` is "now", and only `modified` comes from the file.
fn file_times(path: &Path) -> (timeframe::Instant, timeframe::Instant) {
    let Ok(metadata) = fs::metadata(path) else {
        let now = timeframe::now_local();
        return (now, now);
    };
    (
        timeframe::now_local(),
        metadata_to_instant(metadata.modified()),
    )
}

fn metadata_to_instant(time: std::io::Result<SystemTime>) -> timeframe::Instant {
    let Ok(time) = time else {
        return timeframe::now_local();
    };
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        // `datetime.fromtimestamp` keeps microseconds, and the directory listing's
        // `updated_*` order depends on that precision.
        Ok(duration) => {
            timeframe::from_unix_seconds_micros(duration.as_secs() as i64, duration.subsec_micros())
        }
        Err(error) => {
            // Pre-epoch timestamps cannot be represented as unsigned durations.
            let duration = error.duration();
            timeframe::from_unix_seconds_micros(
                -(duration.as_secs() as i64),
                duration.subsec_micros(),
            )
        }
    }
}

/// Collect every markdown file under `root` (sorted, dot-directories skipped).
pub fn markdown_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut files: Vec<(String, PathBuf)> = WalkDir::new(root)
        // The reference walk uses `Path::is_dir`, so it descends into symlinked
        // directories; `walkdir` only does that with `follow_links` (and it breaks
        // link cycles instead of recursing forever).
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| {
            // Dot-*directories* are skipped (`./.obsidian`), dot-*files* are not.
            entry.depth() == 0
                || !(entry.file_type().is_dir()
                    && entry.file_name().to_string_lossy().starts_with('.'))
        })
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_type().is_file() && entry.path().extension().is_some_and(|ext| ext == "md")
        })
        .map(|entry| {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            (relative, path.to_path_buf())
        })
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// The reference walk skips dot-*directories*, keeps dot-*files*, indexes only
    /// `.md`, descends into symlinked directories, and returns sorted relative paths.
    #[test]
    fn markdown_files_skips_dot_directories_and_follows_links() {
        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!("auto-memory-rs-markdown-files-{nanos}"));
        let write = |relative: &str| {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
            fs::write(&path, "body").expect("write");
        };
        write("notes/a.md");
        write("notes/b.txt");
        write("notes/nested/deep.md");
        write(".hidden/skipped.md");
        write(".dot.md");
        write("target/linked.md");
        symlink(root.join("target"), root.join("link")).expect("symlink");

        let files: Vec<String> = markdown_files(&root)
            .into_iter()
            .map(|(relative, _)| relative)
            .collect();
        assert_eq!(
            files,
            [
                ".dot.md",
                "link/linked.md",
                "notes/a.md",
                "notes/nested/deep.md",
                "target/linked.md"
            ]
        );
        fs::remove_dir_all(&root).ok();
    }
}
