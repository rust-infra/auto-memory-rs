//! Embedding provider abstraction and vector math.
//!
//! The reference uses a local fastembed ONNX model (`BAAI/bge-small-en-v1.5`,
//! 384 dimensions) and L2-normalizes vectors. The trait below keeps the search
//! layer independent of the concrete runtime; `FixtureEmbeddingProvider` replays
//! captured vectors for offline parity tests.

use std::collections::HashMap;

use serde::Deserialize;

use crate::error::{Error, Result};

/// Vector dimensions of the pinned reference model.
pub const REFERENCE_DIMENSIONS: usize = 384;
/// Reference embedding model identifier.
pub const REFERENCE_MODEL: &str = "BAAI/bge-small-en-v1.5";

/// Something that can embed documents and queries.
pub trait EmbeddingProvider: Send + Sync {
    /// Model identifier (stored with every chunk).
    fn model_name(&self) -> &str;
    /// Vector dimensions.
    fn dimensions(&self) -> usize;
    /// Embed indexed documents.
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// Embed one search query.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut vectors = self.embed_documents(&[text.to_owned()])?;
        Ok(vectors
            .pop()
            .unwrap_or_else(|| vec![0.0; self.dimensions()]))
    }
}

/// L2-normalize a vector in place (no-op for zero vectors).
pub fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector.iter_mut() {
            *value /= norm;
        }
    }
}

/// Cosine similarity for unit-normalized vectors (dot product).
pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    left.iter()
        .zip(right.iter())
        .map(|(a, b)| a * b)
        .sum::<f32>()
}

#[derive(Debug, Deserialize)]
struct FixtureFile {
    vectors: HashMap<String, Vec<f32>>,
    #[serde(default)]
    queries: HashMap<String, Vec<f32>>,
}

/// Replays captured reference vectors (offline parity testing).
#[derive(Debug, Clone)]
pub struct FixtureEmbeddingProvider {
    model: String,
    dimensions: usize,
    vectors: HashMap<String, Vec<f32>>,
    queries: HashMap<String, Vec<f32>>,
}

impl FixtureEmbeddingProvider {
    /// Load chunk and query vectors from the captured JSON document.
    pub fn from_json(json: &str) -> Result<Self> {
        let file: FixtureFile = serde_json::from_str(json)?;
        let dimensions = file
            .vectors
            .values()
            .next()
            .map_or(REFERENCE_DIMENSIONS, Vec::len);
        Ok(Self {
            model: REFERENCE_MODEL.to_owned(),
            dimensions,
            vectors: file.vectors,
            queries: file.queries,
        })
    }

    /// Captured vector for one chunk key.
    pub fn vector_for(&self, chunk_key: &str) -> Option<&Vec<f32>> {
        self.vectors.get(chunk_key)
    }
}

impl EmbeddingProvider for FixtureEmbeddingProvider {
    fn model_name(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        texts
            .iter()
            .map(|text| {
                self.vectors
                    .get(text)
                    .cloned()
                    .ok_or_else(|| Error::Frontmatter {
                        message: format!("no captured vector for input: {text}"),
                    })
            })
            .collect()
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.queries
            .get(text)
            .cloned()
            .ok_or_else(|| Error::Frontmatter {
                message: format!("no captured query vector for: {text}"),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_produces_unit_length() {
        let mut vector = vec![3.0, 4.0];
        normalize(&mut vector);
        assert!((vector[0] - 0.6).abs() < 1e-6);
        assert!((vector[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_identical_vectors_is_one() {
        let mut vector = vec![0.5, 0.5, 0.5];
        normalize(&mut vector);
        assert!((cosine_similarity(&vector, &vector) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_handles_length_mismatch() {
        assert_eq!(cosine_similarity(&[1.0, 0.0], &[1.0]), 0.0);
    }
}
