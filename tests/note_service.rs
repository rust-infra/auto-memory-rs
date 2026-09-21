//! Phase 10: `NoteService` keeps the file and the index in step.
//!
//! Every operation writes the file first (atomically) and then refreshes the derived
//! index, so the assertions check both sides: the markdown bytes and the indexed row.

use std::fs;
use std::path::PathBuf;

use auto_memory::application::note::{NoteMetadata, NoteService};
use auto_memory::indexing::{IndexOptions, IndexService};
use auto_memory::markdown::{EditOperation, EditOptions};
use auto_memory::storage::Store;
use serde_yaml_ng::Value;
mod common;
use common::{Scratch, copy_dir};

/// Temp vault with an indexed project.
fn fixture(tag: &str) -> (Scratch, PathBuf, Store, i64) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault"),
        &vault,
    );
    let mut store = Store::open_in_memory().expect("store");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .expect("project");
    let mut service =
        IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
    service.full_rebuild().expect("rebuild");
    (dir, vault, store, project_id)
}

#[test]
fn write_then_read_round_trips_through_the_index() {
    let (_dir, vault, mut store, project_id) = fixture("write");
    {
        let mut notes =
            NoteService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        let metadata = vec![
            ("status".to_owned(), Value::String("active".to_owned())),
            ("priority".to_owned(), Value::Number(3.into())),
        ];
        let metadata = NoteMetadata::from_pairs(metadata);
        let written = notes
            .write_note("notes/fresh.md", "Body text\n", &metadata, false)
            .expect("write");
        assert_eq!(written.file_path, "notes/fresh.md");
        assert_eq!(written.title, "fresh");
        assert_eq!(written.permalink.as_deref(), Some("oracle/notes/fresh"));
        assert_eq!(written.content, "Body text");

        let text = fs::read_to_string(vault.join("notes/fresh.md")).expect("file");
        // A plain body is written verbatim, so its trailing newline survives; content that
        // arrived with its own frontmatter is trimmed instead (reference behaviour, pinned
        // by `tests/mcp_note_tools_golden.rs`).
        assert_eq!(
            text,
            "---\ntitle: fresh\ntype: note\npermalink: oracle/notes/fresh\nstatus: active\npriority: 3\n---\n\nBody text\n"
        );

        // Reading by permalink works too.
        assert_eq!(
            notes.read_note("oracle/notes/fresh").expect("read").content,
            "Body text"
        );
    }

    // The derived index saw the new note, with the merged metadata.
    let entry = store
        .entity_by_file_path(project_id, "notes/fresh.md")
        .expect("lookup")
        .expect("indexed");
    assert_eq!(entry.title, "fresh");
    assert_eq!(
        entry.metadata.get("status"),
        Some(&serde_json::json!("active"))
    );
    assert_eq!(
        entry.metadata.get("priority"),
        Some(&serde_json::json!("3"))
    );

    {
        let mut notes =
            NoteService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        // Overwrite protection is on by default.
        assert!(
            notes
                .write_note("notes/fresh.md", "Other\n", &NoteMetadata::default(), false)
                .is_err(),
            "existing files are not clobbered"
        );
        let overwritten = notes
            .write_note("notes/fresh.md", "Other\n", &NoteMetadata::default(), true)
            .expect("overwrite");
        assert_eq!(overwritten.content, "Other");
    }
}

#[test]
fn edits_update_file_and_index_together() {
    let (_dir, vault, mut store, project_id) = fixture("edit");
    {
        let mut notes =
            NoteService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        let options = EditOptions::new();
        notes
            .edit_note(
                "notes/simple",
                EditOperation::Append,
                "\n## Extra\n\nAppended body.",
                &options,
                &NoteMetadata::default(),
            )
            .expect("append");
        let text = fs::read_to_string(vault.join("notes/simple.md")).expect("file");
        assert!(text.ends_with("Appended body."), "body appended: {text}");

        let mut section = EditOptions::new();
        section.section = Some("Extra".to_owned());
        notes
            .edit_note(
                "notes/simple",
                EditOperation::ReplaceSection,
                "Replaced body.",
                &section,
                &NoteMetadata::default(),
            )
            .expect("replace section");
        let text = fs::read_to_string(vault.join("notes/simple.md")).expect("file");
        assert!(text.contains("## Extra\nReplaced body."), "{text}");
        assert!(!text.contains("Appended body."));

        // Metadata merge keeps the body and the identity fields intact.
        let metadata = NoteMetadata::from_pairs(vec![(
            "status".to_owned(),
            Value::String("active".to_owned()),
        )]);
        notes
            .edit_note(
                "notes/simple",
                EditOperation::Append,
                "",
                &options,
                &metadata,
            )
            .expect("metadata merge");
        let text = fs::read_to_string(vault.join("notes/simple.md")).expect("file");
        assert!(text.contains("\nstatus: active\n"), "{text}");
        // The merge only adds caller keys; the note's frontmatter had none to begin
        // with, and the permalink keeps coming from the entity resolution path.
        assert_eq!(
            notes
                .read_note("notes/simple")
                .expect("read")
                .permalink
                .as_deref(),
            Some("oracle/notes/simple")
        );

        // A failed edit leaves the file untouched.
        let before = fs::read_to_string(vault.join("notes/simple.md")).expect("file");
        let mut missing = EditOptions::new();
        missing.section = Some("Nope".to_owned());
        assert!(
            notes
                .edit_note(
                    "notes/simple",
                    EditOperation::InsertAfterSection,
                    "x",
                    &missing,
                    &NoteMetadata::default(),
                )
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(vault.join("notes/simple.md")).expect("file"),
            before
        );
    }

    // The index reflects the newest body.
    let entity = store
        .entity_by_file_path(project_id, "notes/simple.md")
        .expect("lookup")
        .expect("indexed");
    let content = store.entity_content(entity.id).expect("content");
    assert!(content.contains("Replaced body."), "{content}");
}

#[test]
fn move_and_delete_keep_the_index_consistent() {
    let (_dir, vault, mut store, project_id) = fixture("move");
    {
        let mut notes =
            NoteService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));

        let moved = notes
            .move_note("notes/simple", "notes/simple-moved.md")
            .expect("move");
        assert_eq!(moved.file_path, "notes/simple-moved.md");
        assert_eq!(
            moved.permalink.as_deref(),
            Some("oracle/notes/simple"),
            "the reference keeps the permalink unless update_permalinks_on_move is set"
        );
        assert!(!vault.join("notes/simple.md").exists());

        let deleted = notes.delete_note("notes/simple-moved").expect("delete");
        assert_eq!(deleted, "notes/simple-moved.md");
        assert!(!vault.join("notes/simple-moved.md").exists());
        assert!(notes.read_note("notes/simple-moved").is_err());
    }

    assert!(
        store
            .entity_by_file_path(project_id, "notes/simple-moved.md")
            .expect("lookup")
            .is_none()
    );
    assert!(
        store
            .entity_by_file_path(project_id, "notes/simple.md")
            .expect("lookup")
            .is_none()
    );
}
