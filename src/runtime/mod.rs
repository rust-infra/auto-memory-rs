//! Runtime layer: the concrete embedding backend behind the search API, plus the
//! tokio executor the event-loop adapters run on.
//!
//! See `docs/basic-memory-rs-spec.md` §6 and
//! `docs/basic-memory-rs-execution-plan.md` for the planned responsibilities.

pub mod embedding;
pub mod executor;
pub mod rerank;

pub use embedding::{OnnxEmbeddingProvider, find_onnx_runtime, reference_model_dir};
pub use executor::{THREAD_NAME, block_on};
pub use rerank::{
    DEFAULT_RERANKER_CANDIDATES, DEFAULT_RERANKER_MAX_DOCUMENT_CHARS, OnnxRerankProvider,
    REFERENCE_RERANK_MODEL, RERANK_POOL_CHUNK_FANOUT, RerankProvider, RerankRequest,
    reference_rerank_dir,
};
