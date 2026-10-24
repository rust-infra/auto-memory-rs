//! Chunk vectors are content-addressed, so an edit re-embeds only what it touched.
//!
//! Upstream 0.24.0 (#1691): vector chunks used to be keyed by the search row they were cut
//! from, and every rewrite recreates those rows with new ids, so no chunk matched the
//! previous manifest and a single edit re-embedded the whole note. The key is now a hash
//! of the chunk text, and the row it belongs to travels in a separate `row_key`.
//!
//! The reuse count is the observable: `reindex_embeddings` reports how many chunks it
//! reused, so an unchanged note must report its chunks as reused rather than embedded.

mod common;

use std::fs;

use auto_memory::indexing::rebuild::{RebuildOptions, rebuild_vault};
use auto_memory::search::embedding::FixtureEmbeddingProvider;
use auto_memory::search::vector::{SearchKey, row_key_from_chunk_key};
use auto_memory::storage::Store;
use common::{Scratch, copy_dir, repo_root};

/// A fixture provider that replays the captured corpus.
///
/// Any text the corpus does not cover answers a deterministic zero vector, which keeps the
/// reuse accounting exact without inventing similarity.
fn provider() -> FixtureEmbeddingProvider {
    let path = repo_root().join("tests/golden/vector/embeddings-reference.json");
    let json = fs::read_to_string(path).expect("reference embeddings");
    FixtureEmbeddingProvider::from_json(&json).expect("provider")
}

async fn indexed(tag: &str) -> (Scratch, Store, i64) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_dir(&repo_root().join("tests/fixtures/vault"), &vault);
    let mut store = Store::open_in_memory().await.expect("store");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .await
        .expect("project");
    rebuild_vault(
        &mut store,
        project_id,
        &vault,
        &RebuildOptions::new("oracle"),
    )
    .await
    .expect("rebuild");
    (dir, store, project_id)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reindex_without_edits_reuses_every_chunk() {
    let (dir, mut store, project_id) = indexed("reuse-stable").await;
    let provider = provider();
    let vault = dir.join("vault");

    let first = {
        let mut service = auto_memory::indexing::service::IndexService::new(
            &mut store,
            project_id,
            &vault,
            auto_memory::indexing::service::IndexOptions::new("oracle"),
        );
        service
            .reindex_embeddings(&provider)
            .await
            .expect("first embedding pass")
    };
    assert!(first.chunks > 0, "the fixture has chunks");
    assert_eq!(first.embedded, first.chunks);

    let second = {
        let mut service = auto_memory::indexing::service::IndexService::new(
            &mut store,
            project_id,
            &vault,
            auto_memory::indexing::service::IndexOptions::new("oracle"),
        );
        service
            .reindex_embeddings(&provider)
            .await
            .expect("second embedding pass")
    };
    assert_eq!(
        second.reused, second.chunks,
        "nothing changed, so nothing should be embedded again"
    );
    assert_eq!(second.embedded, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_re_embeds_only_the_note_it_touched() {
    let (dir, mut store, project_id) = indexed("reuse-edit").await;
    let provider = provider();
    let vault = dir.join("vault");

    let before = {
        let mut service = auto_memory::indexing::service::IndexService::new(
            &mut store,
            project_id,
            &vault,
            auto_memory::indexing::service::IndexOptions::new("oracle"),
        );
        service
            .reindex_embeddings(&provider)
            .await
            .expect("first embedding pass")
    };

    // Touch one note. Every other note's text is unchanged, which is exactly the case the
    // old id-keyed scheme could not see.
    let path = vault.join("notes/simple.md");
    let mut body = fs::read_to_string(&path).expect("note");
    body.push_str("\nAn appended line that no other note carries.\n");
    fs::write(&path, body).expect("write");

    {
        let mut service = auto_memory::indexing::service::IndexService::new(
            &mut store,
            project_id,
            &vault,
            auto_memory::indexing::service::IndexOptions::new("oracle"),
        );
        service.reconcile().await.expect("reconcile");
    }

    let after = {
        let mut service = auto_memory::indexing::service::IndexService::new(
            &mut store,
            project_id,
            &vault,
            auto_memory::indexing::service::IndexOptions::new("oracle"),
        );
        service
            .reindex_embeddings(&provider)
            .await
            .expect("second embedding pass")
    };

    assert!(
        after.reused > 0,
        "the untouched notes must keep their vectors: {after:?}"
    );
    // Exactly one chunk is new: the edited note's entity body. Under a key that names the
    // search row this would be three — the body, the note's observation and its relation,
    // because a write deletes and re-inserts those rows, so their ids change even though
    // their text does not. That difference is the whole point of #1691, so it is asserted
    // exactly rather than as a bound.
    assert_eq!(
        after.embedded, 1,
        "only the edited body is new text: {after:?}"
    );
    assert_eq!(after.chunks, before.chunks, "the edit added no chunk");
    assert_eq!(
        after.reused + after.embedded,
        after.chunks,
        "every chunk is either reused or embedded"
    );
}

#[test]
fn a_chunk_names_its_row_beside_a_content_key() {
    // The key cannot carry the row id any more, so `row_key` does. This is the invariant the
    // vector store depends on: a chunk whose row cannot be read is dropped from results.
    let key = row_key_from_chunk_key("observation:42").expect("row key");
    assert_eq!(key.item_type, "observation");
    assert_eq!(key.id, 42);
    assert_eq!(
        key,
        SearchKey {
            item_type: "observation".to_owned(),
            id: 42,
        }
    );
    assert!(
        row_key_from_chunk_key("observation:0f1e2d3c:0").is_none(),
        "a chunk key's second segment is a hash, so it is not a row key"
    );
}
