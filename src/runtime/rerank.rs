//! Local cross-encoder reranker.
//!
//! Mirrors `FastEmbedRerankProvider`: a fastembed cross-encoder reads the query and each
//! candidate's text together and returns one logit per document, which the provider
//! squashes with a clamped sigmoid to a `[0, 1]` relevance. The reranker is off by
//! default in the reference (`reranker_enabled=False`, "adds latency and a first-run
//! model download"), so nothing here runs unless a caller asks for it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fastembed::{
    OnnxSource, RerankInitOptionsUserDefined, TextRerank, TokenizerFiles, UserDefinedRerankingModel,
};

use crate::error::{Error, Result};

/// The reference's default reranker (`DEFAULT_FASTEMBED_RERANK_MODEL`).
pub const REFERENCE_RERANK_MODEL: &str = "jinaai/jina-reranker-v1-tiny-en";
/// Cache directory name for the reference model.
pub const REFERENCE_RERANK_REPO: &str = "models--jinaai--jina-reranker-v1-tiny-en";
/// Candidate documents handed to the cross-encoder.
pub const DEFAULT_RERANKER_CANDIDATES: usize = 20;
/// Characters of each candidate passed to the cross-encoder
/// (reference `reranker_max_document_chars`).
pub const DEFAULT_RERANKER_MAX_DOCUMENT_CHARS: usize = 2000;
/// Reference `RERANK_POOL_CHUNK_FANOUT`: chunk headroom over the rerank window.
pub const RERANK_POOL_CHUNK_FANOUT: usize = 4;

/// One reranker request: the provider plus the knobs the flow needs.
pub struct RerankRequest<'a> {
    /// The query the candidates were retrieved for.
    pub query: &'a str,
    /// The cross-encoder.
    pub provider: &'a dyn RerankProvider,
    /// How many top candidates are rescored.
    pub candidates: usize,
    /// Per-candidate character cap.
    pub max_document_chars: usize,
}

/// A cross-encoder scorer.
pub trait RerankProvider {
    /// Score every document against `query`, in input order, each in `[0, 1]`.
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>>;
    /// Model identifier.
    fn model_name(&self) -> &str;
}

/// The ONNX cross-encoder behind the reference's default reranker.
pub struct OnnxRerankProvider {
    model: Mutex<TextRerank>,
    model_name: String,
}

impl OnnxRerankProvider {
    /// Load the reference reranker from a fastembed cache directory.
    pub fn load_from_cache(cache_root: &Path, runtime: Option<&Path>) -> Result<Self> {
        let model_dir = reference_rerank_dir(cache_root).ok_or_else(|| Error::Embedding {
            message: format!(
                "no {REFERENCE_RERANK_REPO} snapshot under {}",
                cache_root.display()
            ),
        })?;
        Self::load(&model_dir, runtime)
    }

    /// Load the reranker from a directory holding the ONNX file and tokenizer files.
    pub fn load(model_dir: &Path, runtime: Option<&Path>) -> Result<Self> {
        if let Some(runtime) = runtime {
            ort::init_from(runtime).map_err(|error| Error::Embedding {
                message: format!(
                    "failed to load ONNX Runtime from {}: {error}",
                    runtime.display()
                ),
            })?;
        }
        tracing::info!(model_dir = %model_dir.display(), "loading the rerank model");
        let onnx_file = model_dir.join("onnx/model.onnx");
        let onnx_file = if onnx_file.is_file() {
            onnx_file
        } else {
            model_dir.join("model.onnx")
        };
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read_file(&model_dir.join("tokenizer.json"))?,
            config_file: read_file(&model_dir.join("config.json"))?,
            special_tokens_map_file: read_file(&model_dir.join("special_tokens_map.json"))?,
            tokenizer_config_file: read_file(&model_dir.join("tokenizer_config.json"))?,
        };
        let model = UserDefinedRerankingModel::new(OnnxSource::File(onnx_file), tokenizer_files);
        let session =
            TextRerank::try_new_from_user_defined(model, RerankInitOptionsUserDefined::default())
                .map_err(|error| Error::Embedding {
                message: format!("failed to load the reranker: {error}"),
            })?;
        Ok(Self {
            model: Mutex::new(session),
            model_name: REFERENCE_RERANK_MODEL.to_owned(),
        })
    }
}

impl RerankProvider for OnnxRerankProvider {
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let mut model = self.model.lock().map_err(|_| Error::Embedding {
            message: "reranker session lock was poisoned".to_owned(),
        })?;
        // fastembed returns the results sorted by score; the reference's provider keeps
        // the *input* order, so the indices are used to put them back.
        let texts: Vec<&str> = documents.iter().map(String::as_str).collect();
        let ranked = model
            .rerank(query, texts, false, None)
            .map_err(|error| Error::Embedding {
                message: format!("rerank failed: {error}"),
            })?;
        let mut scores = vec![0.0f32; documents.len()];
        for result in ranked {
            let Some(slot) = scores.get_mut(result.index) else {
                return Err(Error::Embedding {
                    message: format!("reranker returned an out-of-range index: {}", result.index),
                });
            };
            *slot = squash_logit(result.score)?;
        }
        Ok(scores)
    }

    fn model_name(&self) -> &str {
        &self.model_name
    }
}

/// `_sigmoid`, clamped so the exponential cannot overflow.
pub fn squash_logit(logit: f32) -> Result<f32> {
    if !logit.is_finite() {
        return Err(Error::Embedding {
            message: format!("reranker returned a non-finite logit: {logit}"),
        });
    }
    let clamped = logit.clamp(-30.0, 30.0);
    Ok(1.0 / (1.0 + (-clamped).exp()))
}

/// Find the reference reranker's snapshot directory inside a fastembed cache.
pub fn reference_rerank_dir(cache_root: &Path) -> Option<PathBuf> {
    let snapshots = cache_root.join(REFERENCE_RERANK_REPO).join("snapshots");
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(snapshots)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.join("tokenizer.json").is_file())
        .collect();
    candidates.sort();
    candidates.pop()
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|error| Error::Embedding {
        message: format!("failed to read {}: {error}", path.display()),
    })
}

#[cfg(test)]
mod tests {
    use super::squash_logit;

    #[test]
    fn logits_squash_into_the_unit_interval() {
        assert!((squash_logit(0.0).expect("score") - 0.5).abs() < 1e-6);
        assert!(squash_logit(4.0).expect("score") > 0.98);
        assert!(squash_logit(-4.0).expect("score") < 0.02);
        // The clamp keeps huge logits finite instead of overflowing the exponential.
        assert!((squash_logit(1e9).expect("score") - 1.0).abs() < 1e-6);
        assert!(squash_logit(f32::NAN).is_err());
    }
}
