//! Local ONNX embedding runtime (reference model parity).
//!
//! The reference computes semantic vectors with the Python `fastembed` runtime and
//! the quantized `qdrant/bge-small-en-v1.5-onnx-q` model (384 dimensions, CLS
//! pooling, L2-normalized). This module runs the *same* ONNX graph and tokenizer
//! through `fastembed`'s Rust bindings, loading the model files from the reference
//! cache (`~/.config/basic-memory/fastembed_cache`) so no download is required.
//!
//! ONNX Runtime itself is loaded dynamically (`ort/load-dynamic`), so builds do not
//! need a bundled runtime; [`OnnxEmbeddingProvider::load`] fails with a clear error
//! when the library is missing. `tests/embedding_runtime.rs` verifies the produced
//! vectors against `tests/golden/vector/embeddings-reference.json`.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fastembed::{
    Embedding, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};

use crate::error::{Error, Result};
use crate::search::embedding::{EmbeddingProvider, REFERENCE_DIMENSIONS, REFERENCE_MODEL};

/// Directory name the Python fastembed runtime uses for this model.
pub const REFERENCE_MODEL_REPO: &str = "models--qdrant--bge-small-en-v1.5-onnx-q";

/// ONNX file inside the model snapshot (quantized export).
const MODEL_FILE: &str = "model_optimized.onnx";

/// ONNX embedding provider backed by the reference model files.
pub struct OnnxEmbeddingProvider {
    model: Mutex<TextEmbedding>,
    dimensions: usize,
}

impl OnnxEmbeddingProvider {
    /// Load the model files in `model_dir`.
    ///
    /// `runtime` optionally points at a specific ONNX Runtime shared library; when
    /// it is `None` the library is resolved from `ORT_DYLIB_PATH` or the system
    /// loader path.
    pub fn load(model_dir: &Path, runtime: Option<&Path>) -> Result<Self> {
        match runtime {
            Some(runtime) => {
                ort::init_from(runtime).map_err(|error| Error::Embedding {
                    message: format!(
                        "failed to load ONNX Runtime from {}: {error}",
                        runtime.display()
                    ),
                })?;
                tracing::info!(runtime = %runtime.display(), "loaded ONNX Runtime");
            }
            // `ort` resolves the library itself in this case (its own
            // `ORT_DYLIB_PATH`, then the platform loader path), so log what was asked
            // for rather than a path we never chose.
            None => {
                tracing::info!("no explicit ONNX Runtime path; letting ort resolve it");
            }
        }
        tracing::info!(model_dir = %model_dir.display(), "loading the embedding model");
        let onnx_file = read_model_file(model_dir)?;
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read_file(&model_dir.join("tokenizer.json"))?,
            config_file: read_file(&model_dir.join("config.json"))?,
            special_tokens_map_file: read_file(&model_dir.join("special_tokens_map.json"))?,
            tokenizer_config_file: read_file(&model_dir.join("tokenizer_config.json"))?,
        };
        // bge-small pools the CLS token; matching it keeps vectors comparable with
        // the reference runtime (mean pooling would silently change every score).
        let model =
            UserDefinedEmbeddingModel::new(onnx_file, tokenizer_files).with_pooling(Pooling::Cls);
        let embedding =
            TextEmbedding::try_new_from_user_defined(model, InitOptionsUserDefined::default())
                .map_err(|error| Error::Embedding {
                    message: format!("failed to initialise the embedding session: {error}"),
                })?;
        Ok(Self {
            model: Mutex::new(embedding),
            dimensions: REFERENCE_DIMENSIONS,
        })
    }

    /// Load the reference model from a fastembed/huggingface hub cache root.
    pub fn load_from_cache(cache_root: &Path, runtime: Option<&Path>) -> Result<Self> {
        let model_dir = reference_model_dir(cache_root).ok_or_else(|| Error::Embedding {
            message: format!(
                "no {} snapshot under {}",
                REFERENCE_MODEL_REPO,
                cache_root.display()
            ),
        })?;
        Self::load(&model_dir, runtime)
    }

    /// Embed one batch of texts, preserving order.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut model = self.model.lock().map_err(|_| Error::Embedding {
            message: "embedding session lock was poisoned".to_owned(),
        })?;
        let embeddings: Vec<Embedding> =
            model.embed(texts, None).map_err(|error| Error::Embedding {
                message: format!("embedding failed: {error}"),
            })?;
        Ok(embeddings
            .into_iter()
            .map(|vector| vector.to_vec())
            .collect())
    }
}

impl EmbeddingProvider for OnnxEmbeddingProvider {
    fn model_name(&self) -> &str {
        REFERENCE_MODEL
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts)
    }
}

/// Snapshot directory of the reference model inside a hub cache root.
///
/// The hub layout is `models--<org>--<name>/snapshots/<revision>/`; the newest
/// revision is chosen when several are cached.
pub fn reference_model_dir(cache_root: &Path) -> Option<PathBuf> {
    let snapshots = cache_root.join(REFERENCE_MODEL_REPO).join("snapshots");
    let mut revisions: Vec<PathBuf> = std::fs::read_dir(snapshots)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    revisions.sort();
    revisions
        .into_iter()
        .rev()
        .find(|revision| revision.join(MODEL_FILE).is_file())
}

/// Locate an ONNX Runtime shared library to load dynamically.
///
/// Checks `ORT_DYLIB_PATH` first, then the well-known locations on this machine
/// (the Python `onnxruntime` wheel that ships with the reference install, plus
/// the distribution copies under `/usr/lib`). Returns `None` when nothing usable
/// is found, which callers report as a clear runtime error.
pub fn find_onnx_runtime() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ORT_DYLIB_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let candidates = [
        home_dir()?.join(
            ".local/share/uv/tools/basic-memory/lib/python3.14/site-packages/onnxruntime/capi",
        ),
        PathBuf::from("/usr/lib/voxtype/cuda-13"),
    ];
    for directory in candidates {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut libraries: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("libonnxruntime.so"))
            })
            .collect();
        libraries.sort();
        if let Some(library) = libraries.pop() {
            return Some(library);
        }
    }
    None
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn read_model_file(model_dir: &Path) -> Result<Vec<u8>> {
    let optimized = model_dir.join(MODEL_FILE);
    if optimized.is_file() {
        return read_file(&optimized);
    }
    read_file(&model_dir.join("model.onnx"))
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|error| Error::Embedding {
        message: format!("failed to read {}: {error}", path.display()),
    })
}
