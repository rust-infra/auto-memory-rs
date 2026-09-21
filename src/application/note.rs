//! Note mutation: read, write, edit, move, and delete markdown notes.
//!
//! Mirrors the reference note-write flow: the file is the source of truth, so every
//! operation rewrites it atomically (frontmatter merged, body preserved) and then
//! refreshes the derived index for that path. `write_note` refuses to clobber an
//! existing file unless the caller asks for an overwrite; `move_note` renames the file
//! and keeps the entity (and, with the reference default
//! `update_permalinks_on_move=false`, its permalink).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_yaml_ng::{Mapping, Value};

use crate::domain::permalink::generate_permalink;
use crate::error::{Error, Result};
use crate::graph::resolve_entity_path;
use crate::indexing::service::{IndexOptions, IndexService};
use crate::markdown::edit::{
    EditOperation, EditOptions, apply_edit_operation, merge_metadata_into_markdown,
};
use crate::markdown::parse_document;
use crate::markdown::serialize::{dump_yaml, write_atomic};
use crate::storage::Store;

/// One note as returned by [`NoteService::read_note`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteDocument {
    /// Project-relative path.
    pub file_path: String,
    /// Resolved permalink.
    pub permalink: Option<String>,
    /// Note title.
    pub title: String,
    /// Note type.
    #[serde(rename = "type")]
    pub note_type: String,
    /// Body without frontmatter.
    pub content: String,
    /// Observation count.
    pub observation_count: usize,
    /// Relation count.
    pub relation_count: usize,
}

/// Ordered frontmatter metadata supplied to note mutations.
#[derive(Debug, Clone, Default)]
pub struct NoteMetadata {
    entries: Vec<(String, Value)>,
}

impl NoteMetadata {
    /// Wrap ordered frontmatter entries.
    pub fn from_pairs(entries: Vec<(String, Value)>) -> Self {
        Self { entries }
    }

    /// Append explicit tags after the base metadata.
    ///
    /// Entries retain insertion order, so an explicit `tags` value wins over the same
    /// key supplied through general metadata.
    pub fn with_tags(mut self, tags: &[String]) -> Self {
        if !tags.is_empty() {
            self.entries.push((
                "tags".to_owned(),
                Value::Sequence(tags.iter().cloned().map(Value::String).collect()),
            ));
        }
        self
    }

    /// Borrow the entries in insertion order.
    pub fn as_pairs(&self) -> &[(String, Value)] {
        &self.entries
    }
}

/// Note mutation service bound to one project and vault.
pub struct NoteService<'a> {
    store: &'a mut Store,
    project_id: i64,
    root: PathBuf,
    options: IndexOptions,
}

impl<'a> NoteService<'a> {
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

    /// Vault root this service writes into.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read one note by permalink or project-relative path.
    pub fn read_note(&self, identifier: &str) -> Result<NoteDocument> {
        let file_path = self.resolve(identifier)?;
        self.read_path(&file_path)
    }

    /// Create a note, or overwrite it when `overwrite` is set.
    ///
    /// The file is written as `---\n<frontmatter>---\n\n<body>` with
    /// `title`/`type`/`permalink` plus the caller metadata, then indexed. `content`
    /// defaults to `# <title>` for an empty body, matching the reference writer.
    pub fn write_note(
        &mut self,
        relative_path: &str,
        content: &str,
        metadata: &NoteMetadata,
        overwrite: bool,
    ) -> Result<NoteDocument> {
        self.write_note_with_type(relative_path, content, metadata, None, overwrite)
    }

    /// Create a note with an explicit default type.
    ///
    /// `note_type` is the caller's `note_type` argument: it supplies the frontmatter
    /// `type` when the content does not carry one, while content frontmatter stays
    /// authoritative — the same precedence the reference applies on its create path.
    pub fn write_note_with_type(
        &mut self,
        relative_path: &str,
        content: &str,
        metadata: &NoteMetadata,
        note_type: Option<&str>,
        overwrite: bool,
    ) -> Result<NoteDocument> {
        let relative_path = normalize_path(relative_path)?;
        let absolute = self.root.join(&relative_path);
        if absolute.exists() && !overwrite {
            return Err(Error::InvalidArgument {
                message: format!("File already exists: {relative_path}"),
            });
        }
        if let Some(parent) = absolute.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let document = parse_document(&relative_path, content)?;
        let title = document.frontmatter.title.clone();
        let note_type = note_type
            .map(str::to_owned)
            .unwrap_or_else(|| document.frontmatter.note_type.clone());
        let permalink = self
            .options
            .permalink
            .for_document(&document, &relative_path);

        let mut frontmatter = Mapping::new();
        frontmatter.insert(
            Value::String("title".to_owned()),
            Value::String(title.clone()),
        );
        frontmatter.insert(Value::String("type".to_owned()), Value::String(note_type));
        frontmatter.insert(
            Value::String("permalink".to_owned()),
            Value::String(permalink),
        );
        for (key, _) in metadata.as_pairs() {
            if matches!(key.as_str(), "title" | "type" | "permalink") {
                return Err(Error::InvalidArgument {
                    message: format!("metadata cannot set {key}; it is derived from the note"),
                });
            }
        }
        let mut text = String::new();
        for (key, value) in metadata.as_pairs() {
            frontmatter.insert(Value::String(key.clone()), value.clone());
        }
        // The reference's create path writes a plain body verbatim, but content that
        // arrived with its own frontmatter is parsed: the block is folded into the written
        // frontmatter and the body is what followed it (trimmed). The difference is
        // observable in the trailing newline.
        let content_had_frontmatter = crate::markdown::serialize::split_frontmatter(content)
            .ok()
            .flatten()
            .is_some();
        let body = if content_had_frontmatter {
            document.content.trim()
        } else {
            content
        };
        let body = if body.is_empty() {
            format!("# {title}")
        } else {
            body.to_owned()
        };
        text.push_str(&crate::markdown::serialize::render(&frontmatter, &body));

        write_atomic(&absolute, &text)?;
        self.reindex(&relative_path)?;
        self.read_path(&relative_path)
    }

    /// Apply one edit operation to an existing note.
    ///
    /// `metadata` is merged into the frontmatter (`title`/`type`/`permalink` are
    /// derived and rejected); the body edit follows the reference semantics.
    pub fn edit_note(
        &mut self,
        identifier: &str,
        operation: EditOperation,
        content: &str,
        options: &EditOptions,
        metadata: &NoteMetadata,
    ) -> Result<NoteDocument> {
        self.edit_note_with_status(identifier, operation, content, options, metadata)
            .map(|(document, _)| document)
    }

    /// Edit a note, creating it when the identifier names one that does not exist yet.
    ///
    /// The reference's `edit_note` is upsert-shaped: a missing note is created at the path
    /// the identifier names (title from the filename), the operation is applied to the
    /// empty document, and the result reports `file_created`.
    pub fn edit_note_with_status(
        &mut self,
        identifier: &str,
        operation: EditOperation,
        content: &str,
        options: &EditOptions,
        metadata: &NoteMetadata,
    ) -> Result<(NoteDocument, bool)> {
        let existing = self.resolve(identifier);
        let relative_path = match existing {
            Ok(relative_path) => relative_path,
            Err(error) => {
                if !matches!(error, Error::InvalidArgument { .. }) {
                    return Err(error);
                }
                let relative_path = create_path_for(identifier)?;
                let edited = apply_edit_operation("", operation, content, options)?;
                let document =
                    self.write_note_with_type(&relative_path, &edited, metadata, None, true)?;
                return Ok((document, true));
            }
        };
        let absolute = self.root.join(&relative_path);
        let current = std::fs::read_to_string(&absolute)?;
        let edited = apply_edit_operation(&current, operation, content, options)?;
        let merged = merge_metadata_into_markdown(&edited, metadata.as_pairs())?;
        write_atomic(&absolute, &merged)?;
        self.reindex(&relative_path)?;
        Ok((self.read_path(&relative_path)?, false))
    }

    /// Move one note to a new project-relative path.
    pub fn move_note(&mut self, from: &str, to: &str) -> Result<NoteDocument> {
        let from = self.resolve(from)?;
        let to = normalize_path(to)?;
        let source = self.root.join(&from);
        let target = self.root.join(&to);
        if target.exists() {
            return Err(Error::InvalidArgument {
                message: format!("File already exists: {to}"),
            });
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&source, &target)?;

        let index_options = self.options.clone();
        let mut service = IndexService::new(self.store, self.project_id, &self.root, index_options);
        service.move_file(&from, &to)?;
        self.read_path(&to)
    }

    /// Delete one note and drop it from the index.
    pub fn delete_note(&mut self, identifier: &str) -> Result<String> {
        let relative_path = self.resolve(identifier)?;
        let absolute = self.root.join(&relative_path);
        if absolute.exists() {
            std::fs::remove_file(&absolute)?;
        }
        self.store
            .remove_document(self.project_id, &relative_path)?;
        Ok(relative_path)
    }

    /// Resolve a permalink or relative path into a project-relative path.
    pub fn resolve(&self, identifier: &str) -> Result<String> {
        let trimmed = identifier
            .trim()
            .trim_start_matches("memory://")
            .trim_matches('/');
        if trimmed.is_empty() {
            return Err(Error::InvalidArgument {
                message: "identifier must not be empty".to_owned(),
            });
        }
        // A path-shaped identifier is only usable while it stays inside the vault: a
        // `..` segment would otherwise let a caller read, edit, or move files outside
        // the project root. Anything else falls through to the index, whose paths were
        // produced by walking the vault.
        let normalized = trimmed.replace('\\', "/");
        if normalized.ends_with(".md")
            && is_safe_relative_path(&normalized)
            && self.root.join(&normalized).is_file()
        {
            return Ok(normalized);
        }
        if let Some(entity) = resolve_entity_path(self.store, self.project_id, trimmed)? {
            return Ok(entity.file_path);
        }
        let generated = generate_permalink(trimmed);
        if let Some(entity) = resolve_entity_path(self.store, self.project_id, &generated)? {
            return Ok(entity.file_path);
        }
        Err(Error::InvalidArgument {
            message: format!("note not found: {identifier}"),
        })
    }

    fn read_path(&self, relative_path: &str) -> Result<NoteDocument> {
        let absolute = self.root.join(relative_path);
        let text = std::fs::read_to_string(&absolute).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::InvalidArgument {
                    message: format!("note not found: {relative_path}"),
                }
            } else {
                Error::Io(error)
            }
        })?;
        let document = parse_document(relative_path, &text)?;
        let permalink = self
            .store
            .entity_by_file_path(self.project_id, relative_path)?
            .and_then(|entity| entity.permalink);
        Ok(NoteDocument {
            file_path: relative_path.to_owned(),
            permalink,
            title: document.frontmatter.title.clone(),
            note_type: document.frontmatter.note_type.clone(),
            content: document.content.clone(),
            observation_count: document.observations.len(),
            relation_count: document.relations.len(),
        })
    }

    fn reindex(&mut self, relative_path: &str) -> Result<()> {
        let options = self.options.clone();
        let mut service = IndexService::new(self.store, self.project_id, &self.root, options);
        service.force_index_file(relative_path)?;
        self.store.resolve_relations(self.project_id)?;
        Ok(())
    }
}

/// Normalize a caller path into the project-relative form.
fn normalize_path(path: &str) -> Result<String> {
    let normalized = path
        .trim()
        .trim_start_matches("memory://")
        .trim_matches('/')
        .replace('\\', "/");
    if normalized.is_empty() {
        return Err(Error::InvalidArgument {
            message: "path must not be empty".to_owned(),
        });
    }
    if !is_safe_relative_path(&normalized) {
        return Err(Error::InvalidArgument {
            message: format!("invalid path: {path}"),
        });
    }
    Ok(normalized)
}

/// The project-relative path an `edit_note` identifier names when the note is absent.
///
/// The reference creates the note the identifier points at: `memory://` is stripped, a
/// missing `.md` is appended, and the title comes from the filename on the create path.
fn create_path_for(identifier: &str) -> Result<String> {
    let trimmed = identifier
        .trim()
        .trim_start_matches("memory://")
        .trim_matches('/');
    if trimmed.is_empty() {
        return Err(Error::InvalidArgument {
            message: "identifier must not be empty".to_owned(),
        });
    }
    let path = if trimmed.to_ascii_lowercase().ends_with(".md") {
        trimmed.to_owned()
    } else {
        format!("{trimmed}.md")
    };
    normalize_path(&path)
}

/// Whether a project-relative path can only ever name something inside the vault.
///
/// Rejects absolute paths and any `.`/`..`/empty segment, so a caller cannot walk out
/// of the project root by spelling a path it controls.
pub fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

/// Render the frontmatter block for a note (helper for callers building files).
pub fn frontmatter_block(mapping: &Mapping) -> String {
    format!("---\n{}---\n", dump_yaml(mapping))
}
