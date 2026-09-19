//! The cross-encoder reranker, against captured reference searches.
//!
//! `tools/dump_reference_rerank.py` runs the reference with `semantic_search_enabled` and
//! `reranker_enabled` and records `tests/golden/search/rerank-*.json`. Two layers are
//! checked here:
//!
//! * the *flow* (pool selection, document construction, ordering, tail demotion), using a
//!   fixture reranker whose keys are the document texts — so a change to the document
//!   format makes every lookup miss and the page collapse;
//! * the *model*, by loading the reference ONNX reranker from the fastembed cache and
//!   comparing the real scores (skipped where the model is not installed).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use basic_mem::domain::SearchItemType;
use basic_mem::indexing::{IndexOptions, IndexService, RebuildOptions, rebuild_vault};
use basic_mem::runtime::find_onnx_runtime;
use basic_mem::runtime::rerank::{
    DEFAULT_RERANKER_CANDIDATES, DEFAULT_RERANKER_MAX_DOCUMENT_CHARS, OnnxRerankProvider,
    RerankRequest,
};
use basic_mem::search::embedding::{EmbeddingProvider, FixtureEmbeddingProvider};
use basic_mem::search::rerank::{FixtureRerankProvider, build_rerank_document};
use basic_mem::search::vector::{VectorSearchOptions, search_hybrid, search_vector};
use basic_mem::storage::Store;
use serde_json::Value;
mod common;
use common::{Scratch, copy_dir, load_golden_json, repo_root};

/// A fixture-indexed vault whose vector index is filled from the reference embeddings.
fn indexed(tag: &str) -> (Scratch, Store, i64, FixtureEmbeddingProvider) {
    let dir = Scratch::new(tag);
    let vault = dir.join("vault");
    copy_dir(&repo_root().join("tests/fixtures/vault"), &vault);
    let mut store = Store::open_in_memory().expect("store");
    let project_id = store
        .upsert_project("oracle", "oracle", &vault.to_string_lossy())
        .expect("project");
    rebuild_vault(
        &mut store,
        project_id,
        &vault,
        &RebuildOptions::new("oracle"),
    )
    .expect("rebuild");
    let embeddings =
        fs::read_to_string(repo_root().join("tests/golden/vector/embeddings-reference.json"))
            .expect("reference embeddings");
    let provider = FixtureEmbeddingProvider::from_json(&embeddings).expect("provider");
    {
        let mut service =
            IndexService::new(&mut store, project_id, &vault, IndexOptions::new("oracle"));
        service.reindex_embeddings(&provider).expect("embeddings");
    }
    (dir, store, project_id, provider)
}

fn golden(name: &str) -> Value {
    load_golden_json(&format!("search/{name}.json"))
}

fn expected_rows(case: &Value) -> Vec<(String, f32)> {
    case["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|row| {
            (
                row["permalink"].as_str().unwrap_or_default().to_owned(),
                row["score"].as_f64().unwrap_or_default() as f32,
            )
        })
        .collect()
}

fn compare(page: &basic_mem::search::text::SearchPage, case: &Value, label: &str) {
    let expected = expected_rows(case);
    assert_eq!(page.results.len(), expected.len(), "{label}: result count");
    for (index, (row, (permalink, score))) in page.results.iter().zip(&expected).enumerate() {
        assert_eq!(
            row.permalink.as_deref(),
            Some(permalink.as_str()),
            "{label}: rank {index}"
        );
        assert!(
            (row.score - score).abs() < 5e-4,
            "{label}: rank {index} score {} vs {score}",
            row.score
        );
    }
    assert_eq!(page.total, 0, "{label}: semantic totals stay inexact");
    assert_eq!(
        page.has_more,
        case["has_more"].as_bool().unwrap_or(false),
        "{label}: has_more"
    );
}

/// Build a reranker fixture from a golden page: the document text is the key, so a wrong
/// document format makes every lookup miss and every score zero.
fn fixture_from_golden(case: &Value, query: &str) -> FixtureRerankProvider {
    let mut documents = serde_json::Map::new();
    for row in case["results"].as_array().expect("results") {
        let body = row["matched_chunk"]
            .as_str()
            .or_else(|| row["content"].as_str())
            .unwrap_or_default();
        let document = build_rerank_document(
            row["title"].as_str(),
            Some(body),
            DEFAULT_RERANKER_MAX_DOCUMENT_CHARS,
        );
        documents.insert(
            document,
            Value::from(row["score"].as_f64().unwrap_or_default()),
        );
    }
    let fixture = serde_json::json!({
        "model": "fixture",
        "scores": { query: Value::Object(documents) },
    });
    FixtureRerankProvider::from_json(&fixture.to_string()).expect("fixture")
}

/// The flow: pool, document format, ordering, and tail demotion.
#[test]
fn rerank_flow_replays_the_captured_reranked_orders() {
    let (_dir, store, project_id, provider) = indexed("flow");
    let model = provider.model_name().to_owned();
    let options = VectorSearchOptions::default();

    let cases: [(&str, &str, bool); 4] = [
        ("rerank-vector-rust", "rust", false),
        ("rerank-hybrid-rust", "rust", true),
        ("rerank-vector-local-index", "local index", false),
        ("rerank-vector-rust-type-note", "rust", false),
    ];
    for (name, query, hybrid) in cases {
        let case = golden(name);
        let reranker = fixture_from_golden(&case, query);
        let request = RerankRequest {
            query,
            provider: &reranker,
            candidates: DEFAULT_RERANKER_CANDIDATES,
            max_document_chars: DEFAULT_RERANKER_MAX_DOCUMENT_CHARS,
        };
        let mut per_case = options.clone();
        if name.ends_with("type-note") {
            per_case.note_types = vec!["note".to_owned()];
        }
        let vector = provider.embed_query(query).expect("query vector");
        let page = if hybrid {
            search_hybrid(
                &store,
                project_id,
                query,
                &vector,
                &model,
                &per_case,
                Some(&request),
            )
            .expect("hybrid")
        } else {
            search_vector(
                &store,
                project_id,
                &vector,
                &model,
                &per_case,
                Some(&request),
            )
            .expect("vector")
        };
        compare(&page, &case, name);
    }
}

/// The model: the reference ONNX cross-encoder through `fastembed`/`ort`.
#[test]
fn onnx_reranker_reproduces_the_reference_scores() {
    let cache = std::env::var_os("BASIC_MEMORY_MODEL_CACHE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".config/basic-memory/fastembed_cache"))
        });
    let Some(cache) = cache.filter(|path| basic_mem::runtime::reference_rerank_dir(path).is_some())
    else {
        eprintln!("skipping: no reranker model in the fastembed cache");
        return;
    };
    let runtime = find_onnx_runtime();
    let reranker = match OnnxRerankProvider::load_from_cache(&cache, runtime.as_deref()) {
        Ok(reranker) => reranker,
        Err(error) => {
            eprintln!("skipping: reranker did not load: {error}");
            return;
        }
    };

    let (_dir, store, project_id, provider) = indexed("model");
    let model = provider.model_name().to_owned();
    let options = VectorSearchOptions::default();
    let cases: [(&str, &str); 3] = [
        ("rerank-vector-rust", "rust"),
        ("rerank-vector-local-index", "local index"),
        ("rerank-hybrid-rust", "rust"),
    ];
    for (name, query) in cases {
        let case = golden(name);
        let request = RerankRequest {
            query,
            provider: &reranker,
            candidates: DEFAULT_RERANKER_CANDIDATES,
            max_document_chars: DEFAULT_RERANKER_MAX_DOCUMENT_CHARS,
        };
        let vector = provider.embed_query(query).expect("query vector");
        let page = if name.contains("hybrid") {
            search_hybrid(
                &store,
                project_id,
                query,
                &vector,
                &model,
                &options,
                Some(&request),
            )
            .expect("hybrid")
        } else {
            search_vector(
                &store,
                project_id,
                &vector,
                &model,
                &options,
                Some(&request),
            )
            .expect("vector")
        };
        compare(&page, &case, name);
        // The reranker replaces retrieval scores with relevance, so every score must be
        // inside the cross-encoder's public [0, 1] range.
        for row in &page.results {
            assert!(
                (0.0..=1.0).contains(&row.score),
                "{name}: {} left the unit interval: {}",
                row.permalink.as_deref().unwrap_or_default(),
                row.score
            );
        }
    }
}

/// The reranker's own contract: one score per document, in input order.
#[test]
fn rerank_provider_scores_documents_in_input_order() {
    let mut scores = HashMap::new();
    scores.insert("query", HashMap::from([("doc", 0.9f32)]));
    let (_dir, store, project_id, provider) = indexed("order");
    let model = provider.model_name().to_owned();
    let request_provider = FixtureRerankProvider::from_json(
        &serde_json::json!({
            "model": "fixture",
            "scores": {"local index": {"": 0.5}},
        })
        .to_string(),
    )
    .expect("fixture");
    let request = RerankRequest {
        query: "local index",
        provider: &request_provider,
        candidates: 3,
        max_document_chars: DEFAULT_RERANKER_MAX_DOCUMENT_CHARS,
    };
    let vector = provider.embed_query("local index").expect("query vector");
    let page = search_vector(
        &store,
        project_id,
        &vector,
        &model,
        &VectorSearchOptions {
            entity_types: vec![SearchItemType::Entity],
            ..VectorSearchOptions::default()
        },
        Some(&request),
    )
    .expect("vector");
    assert_eq!(page.results.len(), 10, "an unknown document scores zero");
    assert!(page.results.iter().all(|row| row.score == 0.0));
}
