//! Phase 8 compatibility: semantic chunking against the captured reference corpus,
//! plus vector ranking and hybrid fusion math.
//!
//! Chunk keys embed row ids, which are internal, so parity is asserted on the
//! stable `(permalink, chunk_text, source_hash)` triples and the chunk count.
//! Embedding capture (fastembed runtime) is not wired yet; ranking and fusion are
//! verified with synthetic vectors.

/// Semantic score envelope. The reference runtime is not bit-reproducible across runs
/// (ONNX Runtime batching/threading): re-capturing the corpus moved identical ranks by up
/// to 1.21e-4 — above the 1e-4 the first capture needed. Ranking and `matched_chunk` stay
/// exact; only the float is allowed to drift.
const SCORE_TOLERANCE: f32 = 5e-4;

use std::collections::HashMap;
use std::fs;

use basic_mem::domain::SearchItemType;
use basic_mem::indexing::{IndexOptions, IndexService};
use basic_mem::search::chunking::{build_chunk_records, entity_fingerprint};
use basic_mem::search::embedding::{
    EmbeddingProvider, FixtureEmbeddingProvider, cosine_similarity,
};
use basic_mem::search::vector::{
    DEFAULT_MIN_SIMILARITY, DEFAULT_VECTOR_K, FUSION_BONUS, SearchKey, VectorSearchOptions,
    fuse_hybrid, matched_chunk_text, normalize_fts_scores, rank_chunks, row_key_from_chunk_key,
    search_hybrid, search_vector,
};
use serde_json::Value;
mod common;
use common::{indexed_store, repo_root};

#[test]
fn chunks_match_reference_corpus() {
    let (_dir, store, project_id) = indexed_store("chunks");
    let rows = store.semantic_rows(project_id).expect("semantic rows");
    let records = build_chunk_records(&rows);

    let golden: Value = serde_json::from_str(
        &fs::read_to_string(repo_root().join("tests/golden/vector/chunks.json")).expect("golden"),
    )
    .expect("json");
    let expected_chunks = golden["chunks"].as_array().expect("chunks");

    assert_eq!(
        records.len(),
        expected_chunks.len(),
        "chunk count must match the reference corpus"
    );

    let mut expected: Vec<(String, String, String)> = expected_chunks
        .iter()
        .map(|chunk| {
            (
                chunk["permalink"].as_str().unwrap_or_default().to_owned(),
                chunk["chunk_text"].as_str().unwrap_or_default().to_owned(),
                chunk["source_hash"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    expected.sort();
    let mut actual: Vec<(String, String, String)> = records
        .iter()
        .map(|record| {
            // Recover the row permalink from the owning search row.
            let key = record
                .chunk_key
                .split(':')
                .take(2)
                .collect::<Vec<_>>()
                .join(":");
            let permalink = rows
                .iter()
                .find(|row| format!("{}:{}", row.item_type, row.id) == key)
                .and_then(|row| row.permalink.clone())
                .unwrap_or_default();
            (
                permalink,
                record.chunk_text.clone(),
                record.source_hash.clone(),
            )
        })
        .collect();
    actual.sort();
    assert_eq!(actual.len(), expected.len());
    assert_eq!(actual, expected, "chunk text/hash multiset must match");

    // Fingerprints are derived from chunk keys (internal ids), so compare the
    // number of distinct owning entities instead of the hash values themselves.
    let mut our_entities: Vec<i64> = Vec::new();
    for row in &rows {
        if let Some(entity_id) = row.entity_id {
            let owned: Vec<_> = records
                .iter()
                .filter(|record| {
                    record
                        .chunk_key
                        .starts_with(&format!("{}:{}:", row.item_type, row.id))
                })
                .cloned()
                .collect();
            if !owned.is_empty() {
                let fingerprint = entity_fingerprint(&owned);
                assert_eq!(fingerprint.len(), 64, "sha256 hex fingerprint");
                our_entities.push(entity_id);
            }
        }
    }
    our_entities.sort_unstable();
    our_entities.dedup();
    let mut expected_entities: Vec<i64> = expected_chunks
        .iter()
        .filter_map(|chunk| chunk["entity_id"].as_i64())
        .collect();
    expected_entities.sort_unstable();
    expected_entities.dedup();
    assert_eq!(
        our_entities.len(),
        expected_entities.len(),
        "every reference entity must contribute chunks"
    );
}

/// End-to-end vector search replay with **reference** embeddings.
///
/// `tests/golden/vector/embeddings-reference.json` holds the vectors the reference
/// fastembed runtime produced for the chunk corpus and the query strings, so this
/// test isolates everything downstream of embedding production: chunk→row mapping,
/// cosine scoring, the 0.55 threshold, best-chunk-per-row aggregation, entity-only
/// filtering, and ordering. It must reproduce `tests/golden/search/vector-local-index.json`
/// (the reference's own vector search).
///
/// Scores are compared inside [`SCORE_TOLERANCE`]; ranking and `matched_chunk` are exact.
#[test]
fn vector_search_replays_reference_scores() {
    let (_dir, store, project_id) = indexed_store("vector-replay");
    let rows = store.semantic_rows(project_id).expect("semantic rows");
    let chunks = build_chunk_records(&rows);

    let embeddings =
        fs::read_to_string(repo_root().join("tests/golden/vector/embeddings-reference.json"))
            .expect("reference embeddings");
    let provider = FixtureEmbeddingProvider::from_json(&embeddings).expect("provider");
    assert_eq!(provider.dimensions(), 384, "pinned model dimensions");

    let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
    for chunk in &chunks {
        if let Some(vector) = provider.vector_for(&chunk.chunk_text) {
            vectors.insert(chunk.chunk_key.clone(), vector.clone());
        }
    }
    assert_eq!(
        vectors.len(),
        chunks.len(),
        "every chunk needs a captured reference vector"
    );

    let query = provider.embed_query("local index").expect("query vector");
    let ranked = rank_chunks(
        &chunks,
        &vectors,
        &query,
        DEFAULT_MIN_SIMILARITY,
        DEFAULT_VECTOR_K,
    );

    let mut actual: Vec<(String, f32, Option<String>)> = Vec::new();
    for scored in &ranked {
        // `search_notes` defaults to entity rows only.
        if scored.key.item_type != "entity" {
            continue;
        }
        let Some(row) = rows
            .iter()
            .find(|row| row.item_type == scored.key.item_type && row.id == scored.key.id)
        else {
            continue;
        };
        let mut row_chunks: Vec<(f32, String)> = chunks
            .iter()
            .filter(|chunk| {
                row_key_from_chunk_key(&chunk.chunk_key).is_some_and(|key| key == scored.key)
            })
            .map(|chunk| {
                let score = vectors
                    .get(&chunk.chunk_key)
                    .map_or(0.0, |vector| cosine_similarity(&query, vector));
                (score, chunk.chunk_text.clone())
            })
            .collect();
        row_chunks.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.1.cmp(&right.1))
        });
        let texts: Vec<String> = row_chunks.into_iter().map(|(_, text)| text).collect();
        actual.push((
            row.permalink.clone().unwrap_or_default(),
            scored.score,
            matched_chunk_text(row.content_snippet.as_deref(), &texts),
        ));
        if actual.len() == 10 {
            break;
        }
    }

    let golden: Value = serde_json::from_str(
        &fs::read_to_string(repo_root().join("tests/golden/search/vector-local-index.json"))
            .expect("golden"),
    )
    .expect("json");
    let expected = golden["results"].as_array().expect("results");

    assert_eq!(
        actual.len(),
        expected.len(),
        "vector search result count must match the reference"
    );
    for (index, (permalink, score, chunk_text)) in actual.iter().enumerate() {
        let entry = &expected[index];
        assert_eq!(
            Some(permalink.as_str()),
            entry["permalink"].as_str(),
            "rank {index}: permalink"
        );
        let reference = entry["score"].as_f64().expect("score") as f32;
        assert!(
            (score - reference).abs() < SCORE_TOLERANCE,
            "rank {index}: score {score} vs reference {reference}"
        );
        assert_eq!(
            chunk_text.as_deref(),
            entry["matched_chunk"].as_str(),
            "rank {index}: matched chunk"
        );
    }
}

#[test]
fn cosine_similarity_ranks_expected_vectors() {
    assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
}

#[test]
fn vector_ranking_applies_threshold_and_limit() {
    let rows = basic_mem::search::chunking::SemanticRow {
        id: 1,
        item_type: "entity".to_owned(),
        title: Some("A".to_owned()),
        permalink: Some("p/a".to_owned()),
        content_snippet: Some("Body".to_owned()),
        category: None,
        relation_type: None,
        entity_id: Some(1),
    };
    let chunks = build_chunk_records(&[rows]);
    let mut vectors = HashMap::new();
    for chunk in &chunks {
        vectors.insert(chunk.chunk_key.clone(), vec![1.0, 0.0]);
    }
    let ranked = rank_chunks(&chunks, &vectors, &[1.0, 0.0], 0.55, 100);
    assert_eq!(ranked.len(), 1);
    assert!((ranked[0].score - 1.0).abs() < 1e-6);

    let filtered = rank_chunks(&chunks, &vectors, &[0.0, 1.0], 0.55, 100);
    assert!(filtered.is_empty(), "below-threshold matches are filtered");
}

#[test]
fn hybrid_fusion_uses_reference_formula() {
    let key = SearchKey {
        item_type: "entity".to_owned(),
        id: 1,
    };
    let fused = fuse_hybrid(&[(key.clone(), 0.5)], &[(key.clone(), 1.0)]);
    assert!((fused[0].1 - (1.0 + FUSION_BONUS * 0.5)).abs() < 1e-6);

    let normalized = normalize_fts_scores(&[-3.0, -1.5]);
    assert!((normalized[0] - 1.0).abs() < 1e-6);
    assert!((normalized[1] - 0.5).abs() < 1e-6);
}

/// End-to-end semantic search through the stored index.
///
/// The vector index is built with the captured reference vectors (no ONNX runtime
/// needed), then both `--vector` and `--hybrid` are compared against the reference
/// search goldens: same rank order, same `matched_chunk`, scores inside the shared
/// envelope the reference itself has run-to-run.
#[test]
fn stored_vector_index_replays_reference_search() {
    let (dir, mut store, project_id) = indexed_store("stored-vector");
    let vault = dir.join("vault");
    let embeddings =
        fs::read_to_string(repo_root().join("tests/golden/vector/embeddings-reference.json"))
            .expect("reference embeddings");
    let provider = FixtureEmbeddingProvider::from_json(&embeddings).expect("provider");
    let model = provider.model_name().to_owned();

    let options = IndexOptions::new("oracle");
    let mut service = IndexService::new(&mut store, project_id, &vault, options);
    let first = service
        .reindex_embeddings(&provider)
        .expect("embedding reindex");
    assert_eq!(first.chunks, 78, "chunk corpus size");
    assert_eq!(first.reused, 0);
    assert_eq!(first.embedded, first.chunks);

    // Unchanged chunks keep their vectors (reference upsert matches on source hash).
    let second = service
        .reindex_embeddings(&provider)
        .expect("second embedding reindex");
    assert_eq!(second.reused, second.chunks);
    assert_eq!(second.embedded, 0);
    drop(service);

    let query = provider.embed_query("local index").expect("query vector");
    let vector_options = VectorSearchOptions::default();
    let page =
        search_vector(&store, project_id, &query, &model, &vector_options, None).expect("vector");
    let golden: Value = serde_json::from_str(
        &fs::read_to_string(repo_root().join("tests/golden/search/vector-local-index.json"))
            .expect("golden"),
    )
    .expect("json");
    compare_search_page(&page, &golden, SCORE_TOLERANCE, "vector");
    assert_eq!(page.total, 0, "semantic totals stay inexact");
    assert!(!page.total_is_exact);
    assert!(page.has_more, "probe pagination reports more results");

    let hybrid_query = provider.embed_query("rust").expect("query vector");
    let hybrid = search_hybrid(
        &store,
        project_id,
        "rust",
        &hybrid_query,
        &model,
        &vector_options,
        None,
    )
    .expect("hybrid");
    let golden: Value = serde_json::from_str(
        &fs::read_to_string(repo_root().join("tests/golden/search/hybrid-rust.json"))
            .expect("golden"),
    )
    .expect("json");
    compare_search_page(&hybrid, &golden, SCORE_TOLERANCE, "hybrid");

    // Filters on the semantic legs. The reference passes them to the FTS leg natively
    // and intersects the vector leg with a filter-only scan, so these cases would pass
    // only if both halves applied the filter.
    for (label, filters, query, text, hybrid_mode) in [
        (
            "vector-rust-type-note",
            VectorSearchOptions {
                note_types: vec!["note".to_owned()],
                ..VectorSearchOptions::default()
            },
            "rust",
            None,
            false,
        ),
        (
            "vector-rust-type-project",
            VectorSearchOptions {
                note_types: vec!["project".to_owned()],
                ..VectorSearchOptions::default()
            },
            "rust",
            None,
            false,
        ),
        (
            "vector-rust-entity-observation",
            VectorSearchOptions {
                entity_types: vec![SearchItemType::Observation],
                ..VectorSearchOptions::default()
            },
            "rust",
            None,
            false,
        ),
        (
            "hybrid-rust-category-decision",
            VectorSearchOptions {
                entity_types: vec![SearchItemType::Observation],
                categories: vec!["decision".to_owned()],
                ..VectorSearchOptions::default()
            },
            "rust",
            Some("rust"),
            true,
        ),
    ] {
        let query_vector = provider.embed_query(query).expect("query vector");
        let page = if hybrid_mode {
            search_hybrid(
                &store,
                project_id,
                text.expect("hybrid text"),
                &query_vector,
                &model,
                &filters,
                None,
            )
            .expect("hybrid")
        } else {
            search_vector(&store, project_id, &query_vector, &model, &filters, None)
                .expect("vector")
        };
        let golden: Value = serde_json::from_str(
            &fs::read_to_string(
                repo_root()
                    .join("tests/golden/search")
                    .join(format!("{label}.json")),
            )
            .expect("golden"),
        )
        .expect("json");
        compare_search_page(&page, &golden, SCORE_TOLERANCE, label);
        assert_eq!(
            page.results.len(),
            golden["results"].as_array().expect("results").len(),
            "{label}: filtered result count"
        );
    }
}

/// Compare one search page against a golden: permalink order, `matched_chunk`, and
/// scores within `tolerance`.
fn compare_search_page(
    page: &basic_mem::search::text::SearchPage,
    golden: &Value,
    tolerance: f32,
    label: &str,
) {
    let expected = golden["results"].as_array().expect("results");
    assert_eq!(page.results.len(), expected.len(), "{label}: result count");
    for (index, (result, entry)) in page.results.iter().zip(expected).enumerate() {
        assert_eq!(
            result.permalink.as_deref(),
            entry["permalink"].as_str(),
            "{label}: rank {index} permalink"
        );
        let reference = entry["score"].as_f64().expect("score") as f32;
        assert!(
            (result.score - reference).abs() < tolerance,
            "{label}: rank {index} score {} vs {reference}",
            result.score
        );
        if let Some(matched) = entry["matched_chunk"].as_str() {
            assert_eq!(
                result.matched_chunk.as_deref(),
                Some(matched),
                "{label}: rank {index} matched_chunk"
            );
        }
    }
}
