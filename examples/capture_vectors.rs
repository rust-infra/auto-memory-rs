//! Capture the vector golden for the 0.24 chunk shape.
//!
//! Run with `cargo run --release --example capture_vectors -- <out.json>`.
//!
//! This is a *capture* tool, not a test: it writes the JSON that
//! `tests/golden/vector/embeddings-0.24.json` holds. The vectors come from the local ONNX
//! runtime, which `tests/embedding_runtime.rs` pins to the reference model's output within
//! 5e-4 per component and 1e-5 cosine.
//!
//! Why a separate capture exists: upstream 0.24.0 (#1335) changed the entity-body chunk
//! boundaries, so the 0.23.2 corpus — captured from the Python reference — holds vectors
//! for chunk texts that no longer exist. The chunk *rules* are pinned by unit tests in
//! `src/search/chunking.rs`; this file pins the resulting texts and their vectors so the
//! regression is still detectable.

use std::fs;
use std::path::{Path, PathBuf};

use auto_memory::indexing::rebuild::{RebuildOptions, rebuild_vault};
use auto_memory::runtime::{OnnxEmbeddingProvider, default_model_cache, find_onnx_runtime};
use auto_memory::search::chunking::{
    ChunkRecord, SemanticRow, build_chunk_records, compose_row_source_text,
};
use auto_memory::search::embedding::EmbeddingProvider;
use auto_memory::storage::Store;
use serde_json::{Map, Value, json};

/// Queries the vector goldens rank against.
const QUERIES: &[&str] = &["local index", "rust"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/golden/vector/embeddings-0.24.json")
        });

    let runtime = find_onnx_runtime()
        .ok_or("no loadable ONNX Runtime; set ORT_DYLIB_PATH to a libonnxruntime shared library")?;
    let cache = default_model_cache();
    let provider = OnnxEmbeddingProvider::load_from_cache(&cache, Some(&runtime))?;
    eprintln!(
        "using {} ({} dims) from {}",
        provider.model_name(),
        provider.dimensions(),
        cache.display()
    );

    let vault = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault");
    let records = records_for(&vault)?;
    eprintln!("capturing {} chunk texts", records.len());

    let mut vectors = Map::new();
    for record in &records {
        // Deduplicated: two rows can share a chunk text, and the fixture provider looks a
        // text up by value.
        if vectors.contains_key(&record.chunk_text) {
            continue;
        }
        let vector = provider.embed_documents(std::slice::from_ref(&record.chunk_text))?;
        let vector = vector
            .first()
            .ok_or("the provider returned no vector")?
            .iter()
            .map(|component| json!(component))
            .collect::<Vec<_>>();
        vectors.insert(record.chunk_text.clone(), Value::Array(vector));
    }

    let mut queries = Map::new();
    for query in QUERIES {
        let vector = provider.embed_query(query)?;
        queries.insert(
            (*query).to_owned(),
            Value::Array(vector.iter().map(|component| json!(component)).collect()),
        );
    }

    let document = json!({
        "model": provider.model_name(),
        "dimensions": provider.dimensions(),
        "_comment": "Captured from the local ONNX runtime, which tests/embedding_runtime.rs \
                     pins to the reference model within 5e-4 per component. Upstream 0.24.0 \
                     #1335 changed the entity-body chunk texts, so the 0.23.2 capture no \
                     longer covers them. The chunk rules are pinned by unit tests; this file \
                     pins the resulting texts and their vectors.",
        "vectors": Value::Object(vectors),
        "queries": Value::Object(queries),
    });

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out, serde_json::to_string_pretty(&document)?)?;
    eprintln!("wrote {}", out.display());
    Ok(())
}

/// Build the chunk records the index would hold for the fixture vault.
fn records_for(vault: &Path) -> Result<Vec<ChunkRecord>, Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut store = Store::open_in_memory().await?;
        let project_id = store
            .upsert_project("oracle", "oracle", &vault.to_string_lossy())
            .await?;
        rebuild_vault(
            &mut store,
            project_id,
            vault,
            &RebuildOptions::new("oracle"),
        )
        .await?;
        let rows = store.semantic_rows(project_id).await?;
        let semantic: Vec<SemanticRow> = rows
            .iter()
            .map(|row| SemanticRow {
                id: row.id,
                item_type: row.item_type.clone(),
                title: row.title.clone(),
                permalink: row.permalink.clone(),
                content_snippet: row.content_snippet.clone(),
                category: row.category.clone(),
                relation_type: row.relation_type.clone(),
                entity_id: row.entity_id,
            })
            .collect();
        // The composed text is what gets chunked; asserting the helper is not dead keeps
        // this tool honest if the composition changes.
        for row in &semantic {
            std::hint::black_box(compose_row_source_text(row));
        }
        Ok(build_chunk_records(&semantic))
    })
}
