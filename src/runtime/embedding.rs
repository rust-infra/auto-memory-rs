//! Local ONNX embedding runtime (reference model parity).
//!
//! The reference computes semantic vectors with the Python `fastembed` runtime and
//! the quantized `qdrant/bge-small-en-v1.5-onnx-q` model (384 dimensions, CLS
//! pooling, L2-normalized). This module runs the *same* ONNX graph and tokenizer
//! through `fastembed`'s Rust bindings, loading the model files from a local cache
//! so no download is required.
//!
//! Both halves of the runtime are optional and discovered at run time:
//!
//! * ONNX Runtime is loaded dynamically (`ort/load-dynamic`), so builds do not need a
//!   bundled runtime and the released binaries stay portable. [`find_onnx_runtime`]
//!   searches [`onnx_runtime_search_paths`] for one, and `ORT_DYLIB_PATH` overrides
//!   the search entirely.
//! * The model files come from [`default_model_cache`], a fastembed/hub cache root
//!   the user can point at with `--model-cache`.
//!
//! Semantic search is *unavailable* rather than broken when either half is missing:
//! [`OnnxEmbeddingProvider::load`] and `load_from_cache` return a typed error, and
//! `auto-memory doctor` reports exactly what was searched and what to do about it.
//! Text search, context, schema, and the MCP server never need either half.
//! `tests/embedding_runtime.rs` verifies the produced vectors against
//! `tests/golden/vector/embeddings-reference.json`.

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

/// Environment override for the ONNX Runtime shared library.
pub const ONNX_RUNTIME_ENV: &str = "ORT_DYLIB_PATH";

/// Environment override for the model cache root.
pub const MODEL_CACHE_ENV: &str = "AUTO_MEMORY_MODEL_CACHE";

/// Cache the *reference* implementation keeps, and the one that makes "no download
/// required" true on a machine that already ran `basic-memory`.
fn reference_model_cache(home: &Path) -> PathBuf {
    home.join(".config/basic-memory/fastembed_cache")
}

/// Cache this port owns, used when the reference's is not there.
fn own_model_cache(home: &Path) -> PathBuf {
    home.join(".cache/auto-memory/models")
}

/// Model cache roots to try, in order.
///
/// The reference's cache comes first so an existing installation keeps working without
/// a download; the port's own cache is the fallback, and it is what a fresh install
/// would be primed with.
pub fn model_cache_search_paths() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return vec![PathBuf::from(".config/basic-memory/fastembed_cache")];
    };
    vec![reference_model_cache(&home), own_model_cache(&home)]
}

/// The model cache to read from when the caller passes no `--model-cache`.
///
/// The first root that actually holds a usable snapshot wins; when none does, the
/// reference's path is reported so the error names the location a user is most likely
/// to have.
pub fn default_model_cache() -> PathBuf {
    let candidates = model_cache_search_paths();
    candidates
        .iter()
        .find(|root| reference_model_dir(root).is_some())
        .or_else(|| candidates.first())
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Directories searched for a dynamically loadable ONNX Runtime.
///
/// Ordered from most specific to most generic: a copy beside the executable (which is
/// how a managed or vendored install ships one), then the Python wheels that carry
/// `onnxruntime`, then the platform library directories. The order is fixed and the
/// directory walks are sorted, so the result does not depend on directory iteration
/// order.
///
/// The Python entries deliberately glob the tool environment instead of naming one:
/// pinning `python3.14` (as this search used to) silently stopped working on the next
/// interpreter bump, and pinned the *reference implementation's* private layout.
///
/// Entries are the policy, not a filtered listing: a path may not exist, which is what
/// `auto-memory doctor` shows so a missing runtime is diagnosable rather than a silent
/// `None`.
pub fn onnx_runtime_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Beside the running binary: `<prefix>/bin/auto-memory` next to `<prefix>/lib/`.
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        paths.push(directory.to_path_buf());
        paths.push(directory.join("../lib"));
    }

    if let Some(home) = home_dir() {
        // uv tool environments: `~/.local/share/uv/tools/<tool>/lib/python<X.Y>/site-packages`
        let tools = home.join(".local/share/uv/tools");
        for environment in sorted_subdirectories(&tools, 1) {
            for python in sorted_subdirectories(&environment.join("lib"), 1) {
                paths.push(python.join("site-packages/onnxruntime/capi"));
            }
        }
        // `pip install --user onnxruntime`, and virtualenvs under the home directory.
        for python in sorted_subdirectories(&home.join(".local/lib"), 1) {
            paths.push(python.join("site-packages/onnxruntime/capi"));
        }
        paths.push(home.join(".local/lib/onnxruntime"));
    }

    for directory in [
        "/usr/local/lib",
        "/usr/lib",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/opt/onnxruntime/lib",
        "/opt/homebrew/lib",
    ] {
        paths.push(PathBuf::from(directory));
    }

    paths
}

/// Subdirectories of `root`, one level per `depth`, in sorted order.
///
/// Sorting keeps the search deterministic; a missing directory is simply empty.
fn sorted_subdirectories(root: &Path, depth: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut directories: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    directories.sort();
    if depth <= 1 {
        return directories;
    }
    directories
        .into_iter()
        .flat_map(|directory| sorted_subdirectories(&directory, depth - 1))
        .collect()
}

/// The newest `libonnxruntime` shared library in `directory`, if any.
fn onnx_library_in(directory: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(directory).ok()?;
    let mut libraries: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with("libonnxruntime.so") || name.starts_with("libonnxruntime.dylib")
            })
        })
        .collect();
    // Sorted so the choice is stable; the last is the highest version.
    libraries.sort();
    libraries.pop()
}

/// Locate an ONNX Runtime shared library to load dynamically.
///
/// `ORT_DYLIB_PATH` wins outright when it names a file. Otherwise
/// [`onnx_runtime_search_paths`] is walked in order and the first directory holding a
/// library wins. `None` means "not found", which callers report as an actionable
/// runtime error rather than a crash — semantic search is optional.
pub fn find_onnx_runtime() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(ONNX_RUNTIME_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
        tracing::warn!(
            path = %path.display(),
            "{ONNX_RUNTIME_ENV} does not name a file; falling back to the search path"
        );
    }
    onnx_runtime_search_paths()
        .iter()
        .find_map(|directory| onnx_library_in(directory))
}

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
                "no {REFERENCE_MODEL_REPO} snapshot under {}; point --model-cache (or \
                 {MODEL_CACHE_ENV}) at a fastembed cache root, or run `auto-memory doctor` to \
                 see what was searched",
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

#[cfg(test)]
mod runtime_discovery_tests {
    use super::*;

    /// The reference's cache is tried first so an existing installation keeps working
    /// without a download; this port's own cache is the fallback that a fresh install
    /// would be primed with.
    #[test]
    fn model_cache_prefers_the_reference_then_this_port() {
        let paths = model_cache_search_paths();
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(
            paths[0].ends_with(".config/basic-memory/fastembed_cache"),
            "{paths:?}"
        );
        assert!(paths[1].ends_with(".cache/auto-memory/models"), "{paths:?}");
    }

    /// `default_model_cache` must never return a path that holds no snapshot when one of
    /// the candidates does — the fallback order is the whole point.
    #[test]
    fn default_model_cache_picks_a_candidate_that_exists() {
        let chosen = default_model_cache();
        assert!(
            model_cache_search_paths().contains(&chosen),
            "{chosen:?} is not one of the search paths"
        );
    }

    /// The Python entries must follow what is installed, not a literal: naming one
    /// interpreter version (as this search used to) stops matching on the next bump.
    /// Asserting against the directory walk is also what catches the glob being wrong —
    /// an early version of this code appended `site-packages` twice and found nothing,
    /// which no string assertion would have noticed.
    #[test]
    fn onnx_search_paths_glob_the_installed_interpreters() {
        let Some(home) = home_dir() else {
            return;
        };
        let tools = home.join(".local/share/uv/tools");
        if !tools.is_dir() {
            return; // nothing to glob on this machine
        }
        let expected: Vec<PathBuf> = sorted_subdirectories(&tools, 1)
            .into_iter()
            .flat_map(|environment| sorted_subdirectories(&environment.join("lib"), 1))
            .map(|python| python.join("site-packages/onnxruntime/capi"))
            .collect();
        assert!(
            !expected.is_empty(),
            "a uv tools directory with no interpreter under lib/"
        );

        let paths = onnx_runtime_search_paths();
        for path in expected {
            assert!(paths.contains(&path), "{path:?} is missing from {paths:?}");
        }
    }

    /// A managed install ships the runtime beside the binary, so that directory has to
    /// be searched before anything global.
    #[test]
    fn onnx_search_paths_start_beside_the_executable() {
        let paths = onnx_runtime_search_paths();
        let executable = std::env::current_exe().expect("current exe");
        let directory = executable.parent().expect("parent");
        assert_eq!(paths.first(), Some(&directory.to_path_buf()), "{paths:?}");
    }

    /// The Python wheels are the most common source of the library.
    #[test]
    fn onnx_search_paths_include_python_wheels() {
        let paths = onnx_runtime_search_paths();
        assert!(
            paths
                .iter()
                .any(|path| path.to_string_lossy().contains("onnxruntime/capi")),
            "{paths:?}"
        );
    }

    /// `sorted_subdirectories` must be sorted (the search is order-sensitive) and must
    /// tolerate a missing directory instead of panicking.
    #[test]
    fn sorted_subdirectories_is_sorted_and_tolerates_a_missing_root() {
        let scratch = tempfile::tempdir().expect("scratch");
        for name in ["b", "a", "c"] {
            std::fs::create_dir(scratch.path().join(name)).expect("dir");
        }
        std::fs::write(scratch.path().join("a-file"), "").expect("file");

        let names: Vec<String> = sorted_subdirectories(scratch.path(), 1)
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a", "b", "c"], "directories only, sorted");

        assert!(sorted_subdirectories(&scratch.path().join("missing"), 1).is_empty());
    }

    /// Only shared libraries are candidates, and the highest version wins so the choice
    /// is stable rather than directory-iteration dependent.
    #[test]
    fn onnx_library_in_takes_the_highest_version_and_ignores_other_files() {
        let scratch = tempfile::tempdir().expect("scratch");
        std::fs::write(scratch.path().join("libonnxruntime.so.1.24.4"), "").expect("lib");
        std::fs::write(scratch.path().join("libonnxruntime.so.1.29.0"), "").expect("lib");
        std::fs::write(
            scratch.path().join("libonnxruntime_providers_shared.so"),
            "",
        )
        .expect("lib");
        std::fs::write(scratch.path().join("README"), "").expect("file");

        let found = onnx_library_in(scratch.path()).expect("a library");
        assert_eq!(found.file_name().unwrap(), "libonnxruntime.so.1.29.0");

        // An empty (or absent) directory is not an error, just no candidate.
        assert!(onnx_library_in(&scratch.path().join("missing")).is_none());
    }
}
