//! Runtime layer: the concrete embedding backend behind the search API, plus the
//! tokio executor the event-loop adapters run on.
//!
//! See `docs/auto-memory-rs-spec.md` §6 and
//! `docs/auto-memory-rs-execution-plan.md` for the planned responsibilities.

pub mod embedding;
pub mod executor;
pub mod rerank;

pub use embedding::{
    MODEL_CACHE_ENV, ONNX_RUNTIME_ENV, OnnxEmbeddingProvider, REFERENCE_MODEL_REPO,
    default_model_cache, find_onnx_runtime, model_cache_search_paths, onnx_runtime_search_paths,
    reference_model_dir, resolve_onnx_runtime,
};
pub use executor::{THREAD_NAME, block_on};
pub use rerank::{
    DEFAULT_RERANKER_CANDIDATES, DEFAULT_RERANKER_MAX_DOCUMENT_CHARS, OnnxRerankProvider,
    REFERENCE_RERANK_MODEL, RERANK_POOL_CHUNK_FANOUT, RerankProvider, RerankRequest,
    reference_rerank_dir,
};
