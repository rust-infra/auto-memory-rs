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

/// Python installations under `home` that may carry ONNX Runtime.
///
/// Split out from [`onnx_runtime_search_paths`] and parameterised by `home` so the layout
/// can be tested against a synthetic tree: a test that walked the real home would pass or
/// fail depending on what the machine happens to have installed, which is how the first
/// version of this shipped a bug (it appended `site-packages` twice and matched nothing).
fn python_onnx_search_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // uv tool environments: `~/.local/share/uv/tools/<tool>/lib/python<X.Y>/site-packages`.
    for environment in sorted_subdirectories(&home.join(".local/share/uv/tools"), 1) {
        for python in sorted_subdirectories(&environment.join("lib"), 1) {
            paths.push(python.join("site-packages/onnxruntime/capi"));
        }
    }

    // `pip install --user onnxruntime`, and virtualenvs under the home directory.
    for python in sorted_subdirectories(&home.join(".local/lib"), 1) {
        paths.push(python.join("site-packages/onnxruntime/capi"));
    }

    // A bare unpacked runtime.
    paths.push(home.join(".local/lib/onnxruntime"));
    paths
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
/// entries are the policy, not a filtered listing: a path may not exist, which is what
/// `auto-memory doctor` shows so a missing runtime is diagnosable rather than a silent
/// `None`. The platform directories are always listed for the same reason, even on a
/// machine where they are empty. On Windows they come from `PROGRAMFILES` / `APPDATA`
/// instead of the Unix directories — a native Windows install has no `HOME` and no
/// `/usr/local/lib`, so the layout there is searched explicitly.
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
        paths.extend(python_onnx_search_paths(&home));
    }

    if cfg!(windows) {
        // Manual installs of the official release zip extract `lib/onnxruntime.dll`
        // under Program Files.
        let program_files: Vec<PathBuf> = ["PROGRAMFILES", "PROGRAMFILES(X86)"]
            .into_iter()
            .filter_map(|var| std::env::var_os(var).map(PathBuf::from))
            .collect();
        paths.extend(windows_platform_onnx_directories(&program_files));

        // uv tool and `pip --user` wheels live under `%APPDATA%` on Windows.
        if let Some(appdata) = std::env::var_os("APPDATA") {
            paths.extend(windows_python_onnx_search_paths(Path::new(&appdata)));
        }
    } else {
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
    }

    paths
}

/// Manual installs of the official Windows release zip: `<root>\onnxruntime\lib`.
///
/// Parameterised by the Program Files roots so the layout stays testable; the caller
/// resolves `PROGRAMFILES` / `PROGRAMFILES(X86)` itself.
fn windows_platform_onnx_directories(program_files: &[PathBuf]) -> Vec<PathBuf> {
    program_files
        .iter()
        .map(|root| root.join("onnxruntime/lib"))
        .collect()
}

/// Python `onnxruntime` wheels in the Windows layout under the `%APPDATA%` root.
///
/// The Unix layout (`.local/...`) does not apply: `pip install --user` installs under
/// `%APPDATA%\Python\Python3.X`, and `uv tool install` creates its environments under
/// `%APPDATA%\uv\tools\<tool>` with a `Lib\site-packages` instead of
/// `lib/python<X.Y>/site-packages`. Parameterised by `appdata` so the layout can be
/// tested against a synthetic tree, mirroring [`python_onnx_search_paths`].
fn windows_python_onnx_search_paths(appdata: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // uv tool environments: `%APPDATA%\uv\tools\<tool>\Lib\site-packages\onnxruntime\capi`.
    for environment in sorted_subdirectories(&appdata.join("uv/tools"), 1) {
        paths.push(environment.join("Lib/site-packages/onnxruntime/capi"));
    }

    // `pip install --user onnxruntime`: `%APPDATA%\Python\Python3.X\site-packages`.
    for python in sorted_subdirectories(&appdata.join("Python"), 1) {
        paths.push(python.join("site-packages/onnxruntime/capi"));
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
                // Windows ships `onnxruntime.dll` — no `lib` prefix, `.dll` extension —
                // while Unix builds are `libonnxruntime.{so,dylib}` or versioned
                // `libonnxruntime.<version>.dylib` / `libonnxruntime.so.<version>`.
                (name.starts_with("libonnxruntime.")
                    && (name.ends_with(".so") || name.contains(".so.") || name.ends_with(".dylib")))
                    || name.starts_with("onnxruntime.dll")
            })
        })
        .collect();
    // Sorted so the choice is stable; the last is the highest version.
    libraries.sort();
    libraries.pop()
}

/// Resolve an explicitly requested runtime path before falling back to discovery.
///
/// `requested` may name either the shared library itself or a directory containing it.
/// This is the value behind `--onnx-runtime`; `ORT_DYLIB_PATH` keeps the same
/// file-or-directory semantics so CLI flags and the environment agree.
pub fn resolve_onnx_runtime(requested: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = requested {
        if let Some(runtime) = runtime_in_path(path) {
            return Some(runtime);
        }
        tracing::warn!(
            path = %path.display(),
            "explicit ONNX Runtime path does not name a library or a directory containing one; \
             falling back to discovery"
        );
    }

    if let Ok(path) = std::env::var(ONNX_RUNTIME_ENV) {
        let path = PathBuf::from(path);
        if let Some(runtime) = runtime_in_path(&path) {
            return Some(runtime);
        }
        tracing::warn!(
            path = %path.display(),
            "{ONNX_RUNTIME_ENV} does not name a library or a directory containing one; \
             falling back to the search path"
        );
    }

    onnx_runtime_search_paths()
        .iter()
        .find_map(|directory| onnx_library_in(directory))
}

/// Locate an ONNX Runtime shared library to load dynamically.
///
/// This is [`resolve_onnx_runtime`] without an explicit path: `ORT_DYLIB_PATH` first,
/// then [`onnx_runtime_search_paths`] in order. `None` means "not found", which callers
/// report as an actionable runtime error rather than a crash — semantic search is
/// optional.
pub fn find_onnx_runtime() -> Option<PathBuf> {
    resolve_onnx_runtime(None)
}

/// Resolve one explicit path as either a shared-library file or a directory.
fn runtime_in_path(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if path.is_dir() {
        return onnx_library_in(path);
    }
    None
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

/// The user's home directory: `$HOME`, or `%USERPROFILE%` on Windows, which does not
/// set `HOME` by default (the shell does, but PowerShell and cmd do not).
fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    }
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

    /// The Python candidates must follow the installed layout exactly — one entry per
    /// interpreter, with the tail appended once.
    ///
    /// This is the regression that started it: an earlier version appended
    /// `site-packages` twice, so the path never matched and the embedding test silently
    /// became a skip. It is checked against a synthetic home so it holds on a machine
    /// with no Python at all (CI).
    #[test]
    fn python_onnx_search_paths_follow_the_installed_layout() {
        let scratch = tempfile::tempdir().expect("scratch");
        let home = scratch.path();
        std::fs::create_dir_all(
            home.join(".local/share/uv/tools/basic-memory/lib/python3.14/site-packages"),
        )
        .expect("uv site-packages");
        std::fs::create_dir_all(home.join(".local/lib/python3.12/site-packages"))
            .expect("pip site-packages");

        let paths = python_onnx_search_paths(home);
        assert_eq!(
            paths,
            vec![
                home.join(
                    ".local/share/uv/tools/basic-memory/lib/python3.14/site-packages\
                     /onnxruntime/capi"
                ),
                home.join(".local/lib/python3.12/site-packages/onnxruntime/capi"),
                home.join(".local/lib/onnxruntime"),
            ],
            "{paths:?}"
        );
    }

    /// Several interpreters are all searched, in sorted order, so two installs cannot
    /// shadow each other by directory-iteration luck. Only the wheel entries carry an
    /// interpreter to name — the bare `~/.local/lib/onnxruntime` entry has no `capi`
    /// directory, so it is filtered out.
    #[test]
    fn python_onnx_search_paths_cover_every_installed_interpreter() {
        let scratch = tempfile::tempdir().expect("scratch");
        let home = scratch.path();
        for interpreter in ["python3.11", "python3.14"] {
            std::fs::create_dir_all(
                home.join(".local/lib")
                    .join(interpreter)
                    .join("site-packages"),
            )
            .expect("site-packages");
        }

        let paths = python_onnx_search_paths(home);
        let interpreters: Vec<String> = paths
            .iter()
            .filter(|path| path.ends_with("onnxruntime/capi"))
            .filter_map(|path| path.parent()?.parent()?.parent()?.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(interpreters, ["python3.11", "python3.14"], "{paths:?}");
    }

    /// The platform directories are part of the policy, so they are listed whether or
    /// not anything is installed there. `doctor` prints this list to explain a miss, and
    /// a machine-dependent list would make that explanation wrong. The Unix directories
    /// do not exist on Windows, so the check only applies off-Windows; the Windows
    /// layout is covered by the dedicated tests below.
    #[test]
    fn onnx_search_paths_include_the_platform_library_directories() {
        if cfg!(windows) {
            return;
        }
        let paths = onnx_runtime_search_paths();
        for expected in [
            "/usr/local/lib",
            "/usr/lib",
            "/opt/onnxruntime/lib",
            "/opt/homebrew/lib",
        ] {
            assert!(
                paths.iter().any(|path| path == Path::new(expected)),
                "{expected} is missing from {paths:?}"
            );
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

    /// Windows ships the runtime as `onnxruntime.dll` — no `lib` prefix, `.dll`
    /// extension — and the providers DLL must stay excluded, mirroring the `.so` test.
    /// String matching runs identically off-Windows, so the check holds on any host.
    #[test]
    fn onnx_library_in_finds_a_windows_dll() {
        let scratch = tempfile::tempdir().expect("scratch");
        std::fs::write(scratch.path().join("onnxruntime.dll"), "").expect("dll");
        std::fs::write(scratch.path().join("onnxruntime_providers_shared.dll"), "").expect("dll");
        std::fs::write(scratch.path().join("README"), "").expect("file");

        let found = onnx_library_in(scratch.path()).expect("a library");
        assert_eq!(found.file_name().unwrap(), "onnxruntime.dll");
    }

    /// macOS wheels ship `libonnxruntime.<version>.dylib`, not a stable unversioned
    /// name; the directory scan must still find it.
    #[test]
    fn onnx_library_in_finds_a_versioned_macos_dylib() {
        let scratch = tempfile::tempdir().expect("scratch");
        std::fs::write(scratch.path().join("libonnxruntime.1.30.0.dylib"), "").expect("dylib");
        std::fs::write(
            scratch.path().join("libonnxruntime_providers_shared.dylib"),
            "",
        )
        .expect("provider");

        let found = onnx_library_in(scratch.path()).expect("a library");
        assert_eq!(found.file_name().unwrap(), "libonnxruntime.1.30.0.dylib");
    }

    #[test]
    fn explicit_runtime_accepts_a_file_or_a_directory() {
        let scratch = tempfile::tempdir().expect("scratch");
        let file = scratch.path().join("libonnxruntime.dylib");
        std::fs::write(&file, "").expect("file");
        assert_eq!(runtime_in_path(&file), Some(file.clone()));

        let directory = scratch.path().join("bundle");
        std::fs::create_dir(&directory).expect("directory");
        let library = directory.join("libonnxruntime.dylib");
        std::fs::write(&library, "").expect("library");
        assert_eq!(runtime_in_path(&directory), Some(library));
    }

    /// The Windows Python candidates must follow `%APPDATA%` rather than the Unix
    /// `.local/...` layout: one entry per uv tool (with `Lib\site-packages`), and one
    /// per `pip --user` interpreter.
    #[test]
    fn windows_python_onnx_search_paths_follow_the_installed_layout() {
        let scratch = tempfile::tempdir().expect("scratch");
        let appdata = scratch.path();
        std::fs::create_dir_all(appdata.join("uv/tools/basic-memory/Lib/site-packages"))
            .expect("uv site-packages");
        std::fs::create_dir_all(appdata.join("Python/Python312/site-packages"))
            .expect("pip site-packages");

        let paths = windows_python_onnx_search_paths(appdata);
        assert_eq!(
            paths,
            vec![
                appdata.join("uv/tools/basic-memory/Lib/site-packages/onnxruntime/capi"),
                appdata.join("Python/Python312/site-packages/onnxruntime/capi"),
            ],
            "{paths:?}"
        );
    }

    /// The Program Files candidates map `<root>` to `<root>/onnxruntime/lib`, the
    /// layout of a manual extraction of the official Windows release zip. The expected
    /// paths are computed from the same roots so the separator style (`\` on Windows,
    /// `/` elsewhere) cannot leak into the assertion.
    #[test]
    fn windows_platform_directories_map_program_files() {
        let roots = vec![
            PathBuf::from(r"C:\Program Files"),
            PathBuf::from(r"C:\Program Files (x86)"),
        ];
        let expected: Vec<PathBuf> = roots
            .iter()
            .map(|root| root.join("onnxruntime").join("lib"))
            .collect();
        assert_eq!(windows_platform_onnx_directories(&roots), expected);
    }
}
