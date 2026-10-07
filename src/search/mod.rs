//! Search: FTS5 text retrieval, filters, and (later) vector/hybrid fusion.

pub mod chunking;
pub mod embedding;
pub mod index_rows;
pub mod query;
pub mod relaxation;
pub mod rerank;
pub mod text;
pub mod vector;

pub use chunking::{
    ChunkRecord, MAX_VECTOR_CHUNK_CHARS, SemanticRow, VECTOR_CHUNK_OVERLAP_CHARS,
    build_chunk_records, compose_row_source_text, entity_fingerprint, split_text_into_chunks,
};
pub use embedding::{
    EmbeddingProvider, FixtureEmbeddingProvider, REFERENCE_DIMENSIONS, REFERENCE_MODEL,
    cosine_similarity, normalize,
};
pub use index_rows::{
    MAX_CONTENT_STEMS_SIZE, SearchIndexWriteRow, entity_row, observation_row, relation_row,
    text_variants,
};
pub use relaxation::{relaxed_query, relaxed_query_words};
pub use rerank::{
    build_rerank_document, demote_tail_scores, rerank_and_paginate, rerank_candidate_limit,
};
pub use text::{SearchPage, TextSearchOptions, default_entity_types, search_text};
pub use vector::{
    DEFAULT_MIN_SIMILARITY, DEFAULT_VECTOR_K, FUSION_BONUS, FUSION_FORMULA_VERSION,
    SMALL_NOTE_CONTENT_LIMIT, ScoredChunk, SearchKey, TOP_CHUNKS_PER_RESULT, fuse_hybrid,
    matched_chunk_text, normalize_fts_scores, rank_chunks, row_key_from_chunk_key,
};
