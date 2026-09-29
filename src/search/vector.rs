//! Vector ranking and hybrid fusion.
//!
//! Fusion is pinned to the reference formula and constants:
//! `max(vector, fts) + FUSION_BONUS * min(vector, fts)` with `FUSION_BONUS = 0.3`
//! (`FUSION_FORMULA_VERSION = "max+0.3*min/v1"`). FTS scores are normalized to
//! `[0, 1]` by absolute value over the page maximum; vector similarities are
//! already calibrated (`1 - L2² / 2`).

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::domain::search::{SearchItemType, SearchResult};
use crate::error::Result;
use crate::runtime::rerank::RerankRequest;
use crate::search::chunking::ChunkRecord;
use crate::search::embedding::cosine_similarity;
use crate::search::rerank::{rerank_and_paginate, rerank_candidate_limit};
use crate::search::text::{SearchPage, TextSearchOptions, truncate_content};
use crate::storage::{SearchRowView, Store, VectorChunkRow};

/// Default minimum cosine similarity (reference `semantic_min_similarity`).
pub const DEFAULT_MIN_SIMILARITY: f32 = 0.55;
/// Default vector candidate count (reference `semantic_vector_k`).
pub const DEFAULT_VECTOR_K: usize = 100;
/// Reference fusion bonus.
pub const FUSION_BONUS: f32 = 0.3;
/// Reference fusion formula version.
pub const FUSION_FORMULA_VERSION: &str = "max+0.3*min/v1";
/// Reference FTS gate threshold (scores below this are zeroed).
pub const FTS_GATE_THRESHOLD: f32 = 0.0;
/// Reference `SMALL_NOTE_CONTENT_LIMIT`: short notes return their whole body.
pub const SMALL_NOTE_CONTENT_LIMIT: usize = 2000;
/// Reference `TOP_CHUNKS_PER_RESULT`: large notes return this many best chunks.
pub const TOP_CHUNKS_PER_RESULT: usize = 5;
/// Reference `semantic_vector_k` default: candidate chunks per retrieval.
pub const DEFAULT_VECTOR_CANDIDATES: usize = 100;
/// Reference `SQLITE_VEC_MAX_K`: hard cap on candidate chunks.
pub const MAX_VECTOR_K: usize = 4096;

/// Reference `matched_chunk_text` for one vector hit.
///
/// Small notes (`content_snippet` at most [`SMALL_NOTE_CONTENT_LIMIT`] characters)
/// report their full body, because that is what the caller wants to read; larger
/// notes report the best [`TOP_CHUNKS_PER_RESULT`] chunk texts joined with `---`.
/// `ranked_chunk_texts` must already be ordered by descending similarity.
pub fn matched_chunk_text(
    content_snippet: Option<&str>,
    ranked_chunk_texts: &[String],
) -> Option<String> {
    let content = content_snippet.unwrap_or_default();
    if !content.is_empty() && content.chars().count() <= SMALL_NOTE_CONTENT_LIMIT {
        return Some(content.to_owned());
    }
    let top: Vec<&str> = ranked_chunk_texts
        .iter()
        .take(TOP_CHUNKS_PER_RESULT)
        .map(String::as_str)
        .collect();
    if top.is_empty() {
        None
    } else {
        Some(top.join("\n---\n"))
    }
}

/// Hybrid/vector key: `(type, id)` — ids collide across row types.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SearchKey {
    /// `entity`, `observation`, or `relation`.
    pub item_type: String,
    /// Row id.
    pub id: i64,
}

/// One scored chunk before mapping back to a search row.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChunk {
    /// Winning chunk key.
    pub chunk_key: String,
    /// Owning search row key.
    pub key: SearchKey,
    /// Cosine similarity.
    pub score: f32,
}

/// Extract the `(type, id)` search-row key from a `type:id:index` chunk key.
pub fn row_key_from_chunk_key(chunk_key: &str) -> Option<SearchKey> {
    let mut parts = chunk_key.split(':');
    let item_type = parts.next()?;
    let id = parts.next()?.parse::<i64>().ok()?;
    Some(SearchKey {
        item_type: item_type.to_owned(),
        id,
    })
}

/// Rank chunks by cosine similarity, keeping the best chunk per search row.
pub fn rank_chunks(
    chunks: &[ChunkRecord],
    vectors: &HashMap<String, Vec<f32>>,
    query_vector: &[f32],
    min_similarity: f32,
    limit: usize,
) -> Vec<ScoredChunk> {
    let mut best: BTreeMap<SearchKey, ScoredChunk> = BTreeMap::new();
    for chunk in chunks {
        let Some(vector) = vectors.get(&chunk.chunk_key) else {
            continue;
        };
        let score = cosine_similarity(query_vector, vector);
        if score < min_similarity {
            continue;
        }
        let Some(key) = row_key_from_chunk_key(&chunk.chunk_key) else {
            continue;
        };
        let candidate = ScoredChunk {
            chunk_key: chunk.chunk_key.clone(),
            key: key.clone(),
            score,
        };
        match best.get(&key) {
            Some(existing) if existing.score >= score => {}
            _ => {
                best.insert(key, candidate);
            }
        }
    }
    let mut ranked: Vec<ScoredChunk> = best.into_values().collect();
    ranked.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.chunk_key.cmp(&right.chunk_key))
    });
    ranked.truncate(limit);
    ranked
}

/// Normalize FTS scores to `[0, 1]` by absolute value over the maximum.
///
/// Handles the SQLite sign convention (bm25 is negative) and the Postgres one by
/// using absolute values, then applies the reference gate: scores below
/// [`FTS_GATE_THRESHOLD`] contribute nothing to fusion.
pub fn normalize_fts_scores(scores: &[f32]) -> Vec<f32> {
    let maximum = scores
        .iter()
        .map(|score| score.abs())
        .fold(0.0_f32, f32::max);
    scores
        .iter()
        .map(|score| {
            let normalized = if maximum > 0.0 {
                score.abs() / maximum
            } else {
                0.0
            };
            if normalized < FTS_GATE_THRESHOLD {
                0.0
            } else {
                normalized
            }
        })
        .collect()
}

/// Fuse FTS and vector results with the reference formula.
///
/// Rows are keyed on `(type, id)` — bare ids collide across row types — and scored
/// with `max(vector, fts) + FUSION_BONUS * min(vector, fts)`. Rows present in only
/// one leg keep that leg's score (the missing leg counts as `0.0`). The result is
/// ordered by descending fused score, with ties following the FTS order first and
/// then the vector-only order.
pub fn fuse_hybrid(fts: &[(SearchKey, f32)], vector: &[(SearchKey, f32)]) -> Vec<(SearchKey, f32)> {
    let mut order: Vec<SearchKey> = Vec::with_capacity(fts.len() + vector.len());
    let mut fts_scores: HashMap<SearchKey, f32> = HashMap::new();
    for (key, score) in fts {
        if fts_scores.insert(key.clone(), *score).is_none() {
            order.push(key.clone());
        }
    }
    let mut vector_scores: HashMap<SearchKey, f32> = HashMap::new();
    for (key, score) in vector {
        if vector_scores.insert(key.clone(), *score).is_none() && !fts_scores.contains_key(key) {
            order.push(key.clone());
        }
    }

    let mut fused: Vec<(SearchKey, f32)> = order
        .into_iter()
        .map(|key| {
            let fts_score = fts_scores.get(&key).copied().unwrap_or(0.0);
            let vector_score = vector_scores.get(&key).copied().unwrap_or(0.0);
            let fts_score = if fts_score < FTS_GATE_THRESHOLD {
                0.0
            } else {
                fts_score
            };
            let score = fts_score.max(vector_score) + FUSION_BONUS * fts_score.min(vector_score);
            (key, score)
        })
        .collect();
    fused.sort_by(|left, right| right.1.total_cmp(&left.1));
    fused
}

/// Options for one vector or hybrid page.
#[derive(Debug, Clone)]
pub struct VectorSearchOptions {
    /// Minimum cosine similarity (reference default `0.55`; `0.0` disables).
    pub min_similarity: f32,
    /// One-based page number.
    pub page: u32,
    /// Page size.
    pub page_size: u32,
    /// Indexed row types to return (reference default: entity rows).
    pub entity_types: Vec<SearchItemType>,
    /// Exact permalink filter.
    pub permalink: Option<String>,
    /// Glob permalink filter.
    pub permalink_match: Option<String>,
    /// Title filter.
    pub title: Option<String>,
    /// Note-type filter (frontmatter `type`).
    pub note_types: Vec<String>,
    /// Observation-category filter.
    pub categories: Vec<String>,
    /// Tag filter.
    pub tags: Vec<String>,
    /// Frontmatter `status` filter.
    pub status: Option<String>,
    /// Additional frontmatter key/value filters.
    pub metadata_filters: BTreeMap<String, String>,
    /// Only rows updated after this timestamp.
    pub after_date: Option<String>,
}

impl Default for VectorSearchOptions {
    fn default() -> Self {
        Self {
            min_similarity: DEFAULT_MIN_SIMILARITY,
            page: 1,
            page_size: 10,
            entity_types: vec![SearchItemType::Entity],
            permalink: None,
            permalink_match: None,
            title: None,
            note_types: Vec::new(),
            categories: Vec::new(),
            tags: Vec::new(),
            status: None,
            metadata_filters: BTreeMap::new(),
            after_date: None,
        }
    }
}

impl VectorSearchOptions {
    /// Whether any row filter was requested.
    ///
    /// Mirrors the reference's `filter_requested` check, which decides whether the
    /// vector candidates get intersected with a filter-only FTS scan. `entity_types`
    /// is handled by the type filter on the ranked rows instead, which is equivalent:
    /// the scan's rows and the candidates share the same `(type, id)` space.
    pub fn filter_requested(&self) -> bool {
        self.permalink.is_some()
            || self.permalink_match.is_some()
            || self.title.is_some()
            || !self.note_types.is_empty()
            || !self.categories.is_empty()
            || !self.tags.is_empty()
            || self.status.is_some()
            || !self.metadata_filters.is_empty()
            || self.after_date.is_some()
    }

    /// The filter set as `search_text` options, for the filter-only scan.
    fn filter_options(&self) -> TextSearchOptions {
        TextSearchOptions {
            permalink: self.permalink.clone(),
            permalink_match: self.permalink_match.clone(),
            title: self.title.clone(),
            note_types: self.note_types.clone(),
            entity_types: self.entity_types.clone(),
            categories: self.categories.clone(),
            tags: self.tags.clone(),
            status: self.status.clone(),
            metadata_filters: self.metadata_filters.clone(),
            after_date: self.after_date.clone(),
            page: 1,
            page_size: VECTOR_FILTER_SCAN_LIMIT,
            query: None,
        }
    }
}

/// Row cap for the filter-only scan the vector leg intersects against.
///
/// Reference `VECTOR_FILTER_SCAN_LIMIT`.
pub const VECTOR_FILTER_SCAN_LIMIT: u32 = 50_000;

/// One search row with its aggregated chunk matches.
#[derive(Debug, Clone, PartialEq)]
pub struct RowMatch {
    /// Owning search row.
    pub key: SearchKey,
    /// Best chunk similarity for the row.
    pub score: f32,
    /// Every matched chunk of the row as `(similarity, chunk_text)`.
    pub chunks: Vec<(f32, String)>,
}

/// Rank stored chunks and aggregate them per search row.
///
/// Mirrors the reference `vec0` KNN plus its Python-side aggregation: the `k`
/// nearest chunks by similarity (ties broken by entity id, then chunk key) are
/// grouped by their parsed `type:id:index` key, keeping the best similarity per row
/// and the matched chunks needed for `matched_chunk_text`.
pub fn aggregate_matches(
    chunks: &[VectorChunkRow],
    query_vector: &[f32],
    k: usize,
) -> Vec<RowMatch> {
    let mut scored: Vec<(f32, i64, &str, String)> = chunks
        .iter()
        .map(|chunk| {
            (
                // 相识度
                cosine_similarity(query_vector, &chunk.embedding),
                chunk.entity_id,
                chunk.chunk_key.as_str(),
                chunk.chunk_text.clone(),
            )
        })
        .collect();
    scored.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(right.2))
    });
    scored.truncate(k);

    let mut order: Vec<SearchKey> = Vec::new();
    let mut rows: HashMap<SearchKey, RowMatch> = HashMap::new();
    for (score, _, chunk_key, chunk_text) in scored {
        let Some(key) = row_key_from_chunk_key(chunk_key) else {
            continue;
        };
        // entity:7:0 ─┐
        // entity:7:1 ─┼─> key: entity:7
        // entity:7:2 ─┘
        if !rows.contains_key(&key) {
            order.push(key.clone());
            rows.insert(
                key.clone(),
                RowMatch {
                    key: key.clone(),
                    score,
                    chunks: Vec::new(),
                },
            );
        }
        if let Some(row) = rows.get_mut(&key) {
            if score > row.score {
                row.score = score;
            }
            row.chunks.push((score, chunk_text));
        }
    }
    order
        .into_iter()
        .filter_map(|key| rows.remove(&key))
        .collect()
}

/// Candidate chunk window for a page (`semantic_vector_k` with ten times the page).
fn candidate_limit(limit: u32, offset: u32) -> u32 {
    (limit + offset)
        .saturating_mul(10)
        .max(DEFAULT_VECTOR_CANDIDATES as u32)
}

/// Rank the stored index for one query, applying the similarity threshold.
async fn ranked_matches(
    store: &Store,
    project_id: i64,
    query_vector: &[f32],
    model: &str,
    k: usize,
    min_similarity: f32,
) -> Result<Vec<RowMatch>> {
    // Get the vector chunks for the project and model.（向量数据）
    let chunks = store.vector_chunks(project_id, model).await?;
    // Aggregate the chunks into candidate matches.
    // 聚合 chunk 到结果行
    let mut matches = aggregate_matches(&chunks, query_vector, k.min(MAX_VECTOR_K));
    if min_similarity > 0.0 {
        matches.retain(|row| row.score >= min_similarity);
    }
    // The reference sorts by score descending; `sort_by` is stable, so rows with
    // equal scores keep the first-seen (KNN) order.
    matches.sort_by(|left, right| right.score.total_cmp(&left.score));
    Ok(matches)
}

/// One row queued for hydration.
struct HydrationEntry {
    /// Owning search row.
    key: SearchKey,
    /// Score to report (similarity, or the fused hybrid score).
    score: f32,
    /// Matched chunks in descending similarity order.
    chunks: Vec<(f32, String)>,
    /// Whether a row without chunks reports its content snippet as `matched_chunk`
    /// (the reference does this for FTS-only rows inside a hybrid page).
    fallback_to_content: bool,
}

/// Hydrate entries into the reference result shape.
async fn hydrate_entries(
    store: &Store,
    project_id: i64,
    entries: Vec<HydrationEntry>,
) -> Result<Vec<SearchResult>> {
    let mut ids: Vec<i64> = entries.iter().map(|entry| entry.key.id).collect();
    ids.sort_unstable();
    ids.dedup();
    let rows = store.search_rows_by_ids(project_id, &ids).await?;
    let lookup: HashMap<(String, i64), &SearchRowView> = rows
        .iter()
        .map(|row| ((row.item_type.clone(), row.id), row))
        .collect();

    let mut results = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(row) = lookup.get(&(entry.key.item_type.clone(), entry.key.id)) else {
            continue;
        };
        let Some(item_type) = parse_item_type(&row.item_type) else {
            continue;
        };
        let mut chunks = entry.chunks;
        chunks.sort_by(|left, right| right.0.total_cmp(&left.0));
        let texts: Vec<String> = chunks.into_iter().map(|(_, text)| text).collect();
        let snippet = row.content_snippet.as_deref();
        let matched_chunk = matched_chunk_text(snippet, &texts).or_else(|| {
            (entry.fallback_to_content && !texts.is_empty())
                .then(|| snippet.map(str::to_owned))
                .flatten()
        });
        let hydration = entity_hydration(store, row.entity_id).await?;
        results.push(SearchResult {
            title: row.title.clone().unwrap_or_default(),
            item_type,
            score: entry.score,
            entity: hydration.0,
            external_id: hydration.1,
            permalink: row.permalink.clone(),
            content: snippet.map(truncate_content),
            matched_chunk,
            file_path: row.file_path.clone(),
            metadata: row.metadata.as_deref().and_then(|raw| {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw).ok()
            }),
            entity_id: row.entity_id,
            observation_id: (item_type == SearchItemType::Observation).then_some(row.id),
            relation_id: (item_type == SearchItemType::Relation).then_some(row.id),
            category: row.category.clone(),
            from_entity: None,
            to_entity: None,
            relation_type: row.relation_type.clone(),
            updated_at: row.updated_at.clone(),
        });
    }
    Ok(results)
}

/// Run one vector search page against the stored chunk index.
///
/// Semantic searches cannot be counted exactly, so the page is a probe: the caller
/// asks for `page_size + 1` rows, derives `has_more` from the extra row, and the
/// response reports `total = 0`, `total_is_exact = false` (reference behavior).
pub async fn search_vector(
    store: &Store,
    project_id: i64,
    query_vector: &[f32],
    model: &str,
    options: &VectorSearchOptions,
    rerank: Option<&RerankRequest<'_>>,
) -> Result<SearchPage> {
    let page = options.page.max(1);
    let page_size = options.page_size.max(1);
    let offset = (page - 1) * page_size;
    let limit = page_size + 1;

    // A reranker needs the whole candidate pool hydrated: it rescores the fixed prefix
    // and every row it returns has to exist, so the page cannot be sliced before it.
    let candidate_chunks = match rerank {
        Some(request) => rerank_candidate_limit(
            DEFAULT_VECTOR_K,
            request.candidates,
            limit as usize,
            offset as usize,
        ),
        None => candidate_limit(limit, offset) as usize,
    };
    // the vector search matches are ranked by similarity, so we need to hydrate
    // the candidate pool before reranking to get the full set of results.
    // 向量数据
    let matches = ranked_matches(
        store,
        project_id, // filter
        query_vector,
        model,
        candidate_chunks,
        options.min_similarity,
    )
    .await?;
    // matches: the vector search matches are ranked by similarity, so we need to
    // apply row filters to the candidate pool before reranking to get the full set of results.
    // 向量数据经过相似度排序后，应用行过滤器获取完整结果集（全文检索）
    let matches = apply_row_filters(store, project_id, matches, options).await?;
    let matches: Vec<RowMatch> = matches
        .into_iter()
        .filter(|row| type_allowed(&row.key.item_type, options))
        .collect();

    let entries: Vec<HydrationEntry> = matches
        .into_iter()
        .map(|row| HydrationEntry {
            key: row.key,
            score: row.score,
            chunks: row.chunks,
            fallback_to_content: false,
        })
        .collect();
    let results = hydrate_entries(store, project_id, entries).await?;
    let mut results = match rerank {
        Some(request) => rerank_and_paginate(results, offset as usize, limit as usize, request)?,
        None => results
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect(),
    };
    let has_more = results.len() > page_size as usize;
    results.truncate(page_size as usize);
    Ok(SearchPage {
        results,
        total: 0,
        total_is_exact: false,
        has_more,
        current_page: page,
        page_size,
    })
}

/// Run one hybrid page: FTS and vector legs fused with the reference formula.
pub async fn search_hybrid(
    store: &Store,
    project_id: i64,
    text: &str,
    query_vector: &[f32],
    model: &str,
    options: &VectorSearchOptions,
    rerank: Option<&RerankRequest<'_>>,
) -> Result<SearchPage> {
    let page = options.page.max(1);
    let page_size = options.page_size.max(1);
    let offset = (page - 1) * page_size;
    let limit = page_size + 1;
    // With a reranker the fusion window is already sized for the pool plus its tail
    // headroom; without one the vector leg expands its own chunk pool.
    let candidate_window = match rerank {
        Some(request) => rerank_candidate_limit(
            DEFAULT_VECTOR_K,
            request.candidates,
            limit as usize,
            offset as usize,
        ) as u32,
        None => candidate_limit(limit, offset),
    };
    let vector_chunk_pool = if rerank.is_some() {
        candidate_window as usize
    } else {
        candidate_window as usize * 10
    };

    // The vector leg expands its chunk pool when no reranker narrows it
    // (`candidate_limit * 10`), then reports at most the fusion window.
    let vector_matches = ranked_matches(
        store,
        project_id,
        query_vector,
        model,
        vector_chunk_pool,
        options.min_similarity,
    )
    .await?;
    let vector_matches = apply_row_filters(store, project_id, vector_matches, options).await?;
    let vector_matches: Vec<RowMatch> = vector_matches
        .into_iter()
        .filter(|row| type_allowed(&row.key.item_type, options))
        .take(candidate_window as usize)
        .collect();
    let vector_scores: Vec<(SearchKey, f32)> = vector_matches
        .iter()
        .map(|row| (row.key.clone(), row.score))
        .collect();

    // The FTS leg carries the same filters natively (the reference passes them into
    // both legs; only the vector leg needs the extra intersection).
    let mut fts_options = options.filter_options();
    fts_options.query = Some(text.to_owned());
    fts_options.page = 1;
    fts_options.page_size = candidate_window;
    // 全文检索
    let fts_page = store.search_text(project_id, &fts_options).await?;
    let fts_raw: Vec<f32> = fts_page.results.iter().map(|result| result.score).collect();
    let normalized = normalize_fts_scores(&fts_raw);
    let fts_scores: Vec<(SearchKey, f32)> = fts_page
        .results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            (
                SearchKey {
                    item_type: <&str>::from(result.item_type).to_owned(),
                    id: result
                        .relation_id
                        .or(result.observation_id)
                        .or(result.entity_id)
                        .unwrap_or_default(),
                },
                normalized.get(index).copied().unwrap_or_default(),
            )
        })
        .collect();

    // 融合全文检索和向量搜索的结果
    let fused = fuse_hybrid(&fts_scores, &vector_scores);
    let entries: Vec<HydrationEntry> = fused
        .into_iter()
        .map(|(key, score)| {
            let chunks = vector_matches
                .iter()
                .find(|row| row.key == key)
                .map(|row| row.chunks.clone())
                .unwrap_or_default();
            HydrationEntry {
                key,
                score,
                chunks,
                fallback_to_content: true,
            }
        })
        .collect();
    let results = hydrate_entries(store, project_id, entries).await?;
    let mut page_results = match rerank {
        Some(request) => rerank_and_paginate(results, offset as usize, limit as usize, request)?,
        None => results
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect(),
    };
    let has_more = page_results.len() > page_size as usize;
    page_results.truncate(page_size as usize);
    Ok(SearchPage {
        results: page_results,
        total: 0,
        total_is_exact: false,
        has_more,
        current_page: page,
        page_size,
    })
}

fn type_allowed(item_type: &str, options: &VectorSearchOptions) -> bool {
    options.entity_types.is_empty()
        || options
            .entity_types
            .iter()
            .any(|allowed| <&str>::from(*allowed) == item_type)
}

/// Keep only the ranked rows a filter-only scan also returns.
///
/// Mirrors the reference: when any row filter is requested, the vector candidates are
/// intersected with a filter-only FTS scan keyed on `(type, id)` — reusing the text
/// path's filter semantics (including the legacy note-type spellings) — and the
/// similarity ordering of the survivors is preserved.
async fn apply_row_filters(
    store: &Store,
    project_id: i64,
    matches: Vec<RowMatch>,
    options: &VectorSearchOptions,
) -> Result<Vec<RowMatch>> {
    if !options.filter_requested() {
        return Ok(matches);
    }
    // 向量数据经过相似度排序后，应用行过滤器获取完整结果集（全文检索）
    // 即向量数据要在全文检索里（前缀检索，bm25排序）
    let page = store
        .search_text(project_id, &options.filter_options())
        .await?;
    let allowed: HashSet<(String, i64)> = page
        .results
        .iter()
        .map(|row| (<&str>::from(row.item_type).to_owned(), search_row_id(row)))
        .collect();
    Ok(matches
        .into_iter()
        .filter(|row| allowed.contains(&(row.key.item_type.clone(), row.key.id)))
        .collect())
}

/// The numeric id of a search row, whichever kind of row it is.
fn search_row_id(row: &SearchResult) -> i64 {
    row.relation_id
        .or(row.observation_id)
        .or(row.entity_id)
        .unwrap_or_default()
}

fn parse_item_type(value: &str) -> Option<SearchItemType> {
    value.parse().ok()
}

/// `(permalink, external_id)` for one entity, when the row has an owning entity.
async fn entity_hydration(
    store: &Store,
    entity_id: Option<i64>,
) -> Result<(Option<String>, Option<String>)> {
    let Some(entity_id) = entity_id else {
        return Ok((None, None));
    };
    let lookup = store
        .entity_permalinks_and_external_ids(&[entity_id])
        .await?;
    Ok(lookup
        .get(&entity_id)
        .map_or((None, None), |(permalink, external)| {
            (permalink.clone(), Some(external.clone()))
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(chunk_key: &str, chunk_text: &str) -> ChunkRecord {
        ChunkRecord {
            chunk_key: chunk_key.to_owned(),
            chunk_text: chunk_text.to_owned(),
            source_hash: format!("hash-{chunk_key}"),
        }
    }

    #[test]
    fn chunk_keys_parse_into_search_row_keys() {
        let key = row_key_from_chunk_key("relation:12:0").expect("key");
        assert_eq!(key.item_type, "relation");
        assert_eq!(key.id, 12);
        assert!(row_key_from_chunk_key("entity:not-a-number:0").is_none());
        assert!(row_key_from_chunk_key("entity").is_none());
    }

    #[test]
    fn rank_chunks_keeps_the_best_chunk_per_row() {
        let chunks = vec![
            chunk("entity:1:0", "weak"),
            chunk("entity:1:1", "strong"),
            chunk("entity:2:0", "mid"),
        ];
        let mut vectors = HashMap::new();
        vectors.insert("entity:1:0".to_owned(), vec![0.6, 0.8]);
        vectors.insert("entity:1:1".to_owned(), vec![1.0, 0.0]);
        vectors.insert("entity:2:0".to_owned(), vec![0.9, 0.1]);

        let ranked = rank_chunks(&chunks, &vectors, &[1.0, 0.0], 0.55, 10);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].key.id, 1);
        assert!(ranked[0].score > ranked[1].score);
        assert_eq!(ranked[0].chunk_key, "entity:1:1", "best chunk wins");

        let filtered = rank_chunks(&chunks, &vectors, &[0.0, 1.0], 0.95, 10);
        assert!(filtered.is_empty(), "threshold filters weak chunks");
    }

    #[test]
    fn normalize_fts_scores_uses_the_page_maximum() {
        let normalized = normalize_fts_scores(&[-4.0, -2.0, -1.0]);
        assert!((normalized[0] - 1.0).abs() < 1e-6);
        assert!((normalized[1] - 0.5).abs() < 1e-6);
        assert!((normalized[2] - 0.25).abs() < 1e-6);
        assert!(normalize_fts_scores(&[]).is_empty());
    }

    #[test]
    fn fuse_hybrid_rewards_dual_source_rows() {
        let key = SearchKey {
            item_type: "entity".to_owned(),
            id: 7,
        };
        let fts_only = SearchKey {
            item_type: "entity".to_owned(),
            id: 8,
        };
        let fused = fuse_hybrid(
            &[(key.clone(), 0.5), (fts_only.clone(), 0.25)],
            &[(key.clone(), 1.0)],
        );
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].0, key);
        assert!((fused[0].1 - (1.0 + FUSION_BONUS * 0.5)).abs() < 1e-6);
        assert_eq!(fused[1].0, fts_only);
        assert!(
            (fused[1].1 - 0.25).abs() < 1e-6,
            "single-source keeps its score"
        );
    }

    #[test]
    fn matched_chunk_switches_on_note_size() {
        let small = vec!["chunk one".to_owned()];
        assert_eq!(
            matched_chunk_text(Some("short body"), &small).as_deref(),
            Some("short body"),
            "small notes report their body"
        );
        let long = "x".repeat(SMALL_NOTE_CONTENT_LIMIT + 1);
        let chunks: Vec<String> = (0..6).map(|index| format!("chunk {index}")).collect();
        let matched = matched_chunk_text(Some(&long), &chunks).expect("matched");
        assert_eq!(matched.matches("chunk ").count(), TOP_CHUNKS_PER_RESULT);
        assert!(matched.contains("\n---\n"));
    }
}
