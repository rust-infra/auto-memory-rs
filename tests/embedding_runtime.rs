//! Phase 8b: the Rust ONNX runtime must reproduce the reference vectors.
//!
//! The test runs the *reference* model files (`qdrant/bge-small-en-v1.5-onnx-q`,
//! CLS pooling, 384 dimensions) through `fastembed`/`ort` and compares against
//! `tests/golden/vector/embeddings-reference.json`, which was produced by the
//! Python `fastembed` runtime the reference implementation uses.
//!
//! It skips (reporting why) when the model cache or a loadable ONNX Runtime is
//! missing, so a machine without the reference install still runs the suite; the
//! deterministic fixture provider keeps the rest of the vector path verified.

use std::fs;
use std::path::PathBuf;

use auto_memory::runtime::{OnnxEmbeddingProvider, find_onnx_runtime};
use auto_memory::search::embedding::{EmbeddingProvider, cosine_similarity};
use serde_json::Value;
mod common;
use common::repo_root;

/// The fastembed cache to load the real model from, using the same discovery as the CLI
/// (`--model-cache`, then `AUTO_MEMORY_MODEL_CACHE`, then the default search path).
fn model_cache() -> Option<PathBuf> {
    let cache = std::env::var_os(auto_memory::runtime::MODEL_CACHE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(auto_memory::runtime::default_model_cache);
    cache.is_dir().then_some(cache)
}

fn reference_embeddings() -> Value {
    let path = repo_root().join("tests/golden/vector/embeddings-reference.json");
    serde_json::from_str(&fs::read_to_string(&path).expect("reference embeddings")).expect("json")
}

fn expected_vector(value: &Value) -> Vec<f32> {
    value
        .as_array()
        .expect("vector")
        .iter()
        .map(|component| component.as_f64().expect("component") as f32)
        .collect()
}

#[test]
fn onnx_runtime_reproduces_reference_vectors() {
    let Some(cache) = model_cache() else {
        eprintln!("skipping: no reference fastembed cache on this machine");
        return;
    };
    let Some(runtime) = find_onnx_runtime() else {
        eprintln!("skipping: no loadable ONNX Runtime (set ORT_DYLIB_PATH)");
        return;
    };

    let provider = OnnxEmbeddingProvider::load_from_cache(&cache, Some(&runtime))
        .unwrap_or_else(|error| panic!("failed to load {}: {error}", runtime.display()));
    assert_eq!(provider.dimensions(), 384, "pinned model dimensions");
    assert_eq!(provider.model_name(), "BAAI/bge-small-en-v1.5");

    let golden = reference_embeddings();
    assert_eq!(golden["dimensions"].as_u64(), Some(384));

    // Same graph, same tokenizer, same normalization. The two bindings still differ
    // in session options (threads, session-level optimizations), which shows up as a
    // ~2e-4 per-component drift on int8-quantized weights. That is ~1e-5 in cosine
    // similarity — two orders of magnitude below the smallest gap between corpus
    // scores — so tolerances are pinned to the observed envelope instead of bit
    // equality; the fixture provider covers exact reference vectors.
    const COMPONENT_TOLERANCE: f32 = 5e-4;
    const COSINE_TOLERANCE: f32 = 1e-5;

    for query in ["local index", "rust"] {
        let expected = expected_vector(&golden["queries"][query]);
        let actual = provider.embed_query(query).expect("query embedding");
        assert_eq!(actual.len(), expected.len(), "query {query}: dimensions");
        let drift = max_abs_diff(&actual, &expected);
        assert!(
            drift < COMPONENT_TOLERANCE,
            "query {query}: max component drift {drift} is too large"
        );
        let similarity = cosine_similarity(&actual, &expected);
        assert!(
            (1.0 - similarity).abs() < COSINE_TOLERANCE,
            "query {query}: cosine with the reference vector is {similarity}"
        );
        let norm = actual.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < COSINE_TOLERANCE,
            "query {query}: norm {norm}"
        );

        // The drift must not reorder results: compare the ranking of a sample of
        // chunk vectors under both the reference and the locally produced query.
        let sample: Vec<String> = golden["vectors"]
            .as_object()
            .expect("vectors")
            .keys()
            .take(8)
            .cloned()
            .collect();
        let mut reference_ranked: Vec<(f32, String)> = sample
            .iter()
            .map(|text| {
                (
                    cosine_similarity(&expected, &expected_vector(&golden["vectors"][text])),
                    text.clone(),
                )
            })
            .collect();
        let mut local_ranked: Vec<(f32, String)> = sample
            .iter()
            .map(|text| {
                (
                    cosine_similarity(&actual, &expected_vector(&golden["vectors"][text])),
                    text.clone(),
                )
            })
            .collect();
        reference_ranked.sort_by(|left, right| right.0.total_cmp(&left.0));
        local_ranked.sort_by(|left, right| right.0.total_cmp(&left.0));
        assert_eq!(
            reference_ranked
                .iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>(),
            local_ranked
                .iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>(),
            "query {query}: the local runtime must preserve the reference ranking"
        );
    }

    // Document vectors: one batch, compared against the same chunk texts the
    // reference embedded while indexing.
    let texts: Vec<String> = golden["vectors"]
        .as_object()
        .expect("vectors")
        .keys()
        .take(4)
        .cloned()
        .collect();
    let actual = provider.embed_documents(&texts).expect("document vectors");
    assert_eq!(actual.len(), texts.len());
    for (text, actual) in texts.iter().zip(&actual) {
        let expected = expected_vector(&golden["vectors"][text]);
        let drift = max_abs_diff(actual, &expected);
        assert!(
            drift < COMPONENT_TOLERANCE,
            "document drift {drift} for {}",
            text.lines().next().unwrap_or_default()
        );
    }
}

fn max_abs_diff(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max)
}
