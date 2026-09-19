//! Cross-encoder reranking of retrieval candidates.
//!
//! Ports the repository's rerank flow: the top `reranker_candidates` rows are rescored by
//! the cross-encoder (their score becomes the squashed relevance), and the untouched tail
//! is demoted onto the same `[0, 1]` scale so a raw retrieval score can never outrank a
//! reranked row. The fixed prefix owns membership: pagination re-scores the same pool and
//! only the requested page is sliced out, so fetching page two cannot reshuffle page one.

use crate::domain::search::SearchResult;
use crate::error::Result;
use crate::runtime::rerank::{RerankProvider, RerankRequest};

/// Text handed to the cross-encoder for one candidate.
///
/// The matched body carries the retrieval signal, so it leads; the title follows so a
/// title-only candidate still has context. Long notes are truncated.
pub fn build_rerank_document(title: Option<&str>, body: Option<&str>, max_chars: usize) -> String {
    let title = title.unwrap_or_default();
    let body = body.unwrap_or_default();
    let text = if !title.is_empty() && !body.is_empty() {
        format!("{body}\n{title}")
    } else if body.is_empty() {
        title.to_owned()
    } else {
        body.to_owned()
    };
    if max_chars > 0 && text.chars().count() > max_chars {
        text.chars().take(max_chars).collect()
    } else {
        text
    }
}

/// Bounded tail scores at or below the reranked floor.
///
/// A zero floor stays zero (the public range has nothing smaller), so callers rely on the
/// pool-before-tail sequence rather than a numeric sentinel to keep the order.
pub fn demote_tail_scores(floor: f32, count: usize) -> Vec<f32> {
    (0..count)
        .map(|index| floor / (index as f32 + 2.0))
        .collect()
}

/// Deterministic reranker backed by captured scores.
///
/// The fixture is a JSON map of `query -> document -> relevance`, where the document key is
/// exactly the text [`build_rerank_document`] produces. That makes the test sensitive to the
/// document construction as well as to the ordering: a document the fixture does not know
/// scores zero.
pub struct FixtureRerankProvider {
    model: String,
    scores: std::collections::HashMap<String, std::collections::HashMap<String, f32>>,
}

impl FixtureRerankProvider {
    /// Load a fixture from its JSON form.
    pub fn from_json(json: &str) -> Result<Self> {
        let parsed: serde_json::Value =
            serde_json::from_str(json).map_err(crate::error::Error::Json)?;
        let model = parsed["model"].as_str().unwrap_or("fixture").to_owned();
        let mut scores = std::collections::HashMap::new();
        if let Some(queries) = parsed["scores"].as_object() {
            for (query, documents) in queries {
                let mut per_document = std::collections::HashMap::new();
                if let Some(documents) = documents.as_object() {
                    for (document, score) in documents {
                        per_document.insert(document.clone(), score.as_f64().unwrap_or(0.0) as f32);
                    }
                }
                scores.insert(query.clone(), per_document);
            }
        }
        Ok(Self { model, scores })
    }
}

impl RerankProvider for FixtureRerankProvider {
    fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        let empty = std::collections::HashMap::new();
        let known = self.scores.get(query).unwrap_or(&empty);
        Ok(documents
            .iter()
            .map(|document| known.get(document).copied().unwrap_or(0.0))
            .collect())
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

/// The candidate text for one row: the matched chunk, else the stored snippet.
fn document_for(row: &SearchResult, max_chars: usize) -> String {
    let body = row
        .matched_chunk
        .as_deref()
        .or(row.content.as_deref())
        .unwrap_or_default();
    build_rerank_document(Some(&row.title), Some(body), max_chars)
}

/// Rerank the prefix of `rows` and return the requested page.
///
/// `rows` must already be in retrieval order. When there is no pool or the requested page
/// starts past every row, the rows are sliced unchanged.
pub fn rerank_and_paginate(
    rows: Vec<SearchResult>,
    offset: usize,
    limit: usize,
    request: &RerankRequest<'_>,
) -> Result<Vec<SearchResult>> {
    let query = request.query;
    let page_end = offset + limit;
    if query.is_empty() || rows.is_empty() {
        return Ok(rows.into_iter().skip(offset).take(limit).collect());
    }

    let pool_size = request.candidates.min(rows.len());
    let pool: Vec<SearchResult> = rows[..pool_size].to_vec();
    let tail: Vec<SearchResult> = rows[pool_size..].to_vec();
    if pool.is_empty() || offset >= rows.len() {
        return Ok(rows.into_iter().skip(offset).take(limit).collect());
    }

    let documents: Vec<String> = pool
        .iter()
        .map(|row| document_for(row, request.max_document_chars))
        .collect();
    let scores = request.provider.rerank(query, &documents)?;
    if scores.len() != pool.len() {
        return Err(crate::error::Error::Embedding {
            message: format!(
                "reranker returned {} scores for {} documents",
                scores.len(),
                pool.len()
            ),
        });
    }
    for (index, score) in scores.iter().enumerate() {
        if !score.is_finite() || !(0.0..=1.0).contains(score) {
            return Err(crate::error::Error::Embedding {
                message: format!(
                    "reranker score at index {index} must be finite and in [0, 1], got {score}"
                ),
            });
        }
    }

    // Stable descending sort: equal scores keep retrieval order.
    let mut order: Vec<usize> = (0..pool.len()).collect();
    order.sort_by(|left, right| {
        scores[*right]
            .partial_cmp(&scores[*left])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let floor = order.last().map_or(0.0, |index| scores[*index]);
    let mut reranked: Vec<SearchResult> = order
        .into_iter()
        .map(|index| {
            let mut row = pool[index].clone();
            row.score = scores[index];
            row
        })
        .collect();

    let demoted = demote_tail_scores(floor, tail.len());
    let mut tail_rows = tail;
    for (row, score) in tail_rows.iter_mut().zip(demoted) {
        row.score = score;
    }
    reranked.extend(tail_rows);
    Ok(reranked
        .into_iter()
        .skip(offset)
        .take(page_end - offset)
        .collect())
}

/// The retrieval candidate budget when reranking is active.
///
/// `rerank_candidate_limit` is `max(semantic_vector_k, candidates * fanout)`; the tail
/// pages need their own chunk headroom, ten chunks per row like the reference's
/// `tail_size * 10`.
pub fn rerank_candidate_limit(
    semantic_vector_k: usize,
    candidates: usize,
    limit: usize,
    offset: usize,
) -> usize {
    let prefix = semantic_vector_k
        .max(candidates.saturating_mul(crate::runtime::rerank::RERANK_POOL_CHUNK_FANOUT));
    let tail_size = (limit + offset).saturating_sub(candidates);
    prefix + tail_size * 10
}

#[cfg(test)]
mod tests {
    use super::{
        build_rerank_document, demote_tail_scores, rerank_and_paginate, rerank_candidate_limit,
    };
    use crate::domain::search::{SearchItemType, SearchResult};
    use crate::runtime::rerank::{RerankProvider, RerankRequest};

    struct FixedProvider(Vec<f32>);

    impl RerankProvider for FixedProvider {
        fn rerank(&self, _query: &str, _documents: &[String]) -> crate::error::Result<Vec<f32>> {
            Ok(self.0.clone())
        }
        fn model_name(&self) -> &str {
            "fixed"
        }
    }

    fn row(title: &str, score: f32, chunk: &str) -> SearchResult {
        SearchResult {
            title: title.to_owned(),
            item_type: SearchItemType::Entity,
            score,
            entity: None,
            external_id: None,
            permalink: Some(title.to_owned()),
            content: Some(chunk.to_owned()),
            matched_chunk: None,
            file_path: format!("{title}.md"),
            updated_at: None,
            metadata: None,
            entity_id: None,
            observation_id: None,
            relation_id: None,
            category: None,
            from_entity: None,
            to_entity: None,
            relation_type: None,
        }
    }

    #[test]
    fn document_leads_with_the_body_and_truncates() {
        assert_eq!(build_rerank_document(Some("T"), Some("B"), 0), "B\nT");
        assert_eq!(build_rerank_document(Some("T"), None, 0), "T");
        assert_eq!(build_rerank_document(None, Some("B"), 0), "B");
        assert_eq!(
            build_rerank_document(Some("Title"), Some("Body"), 5),
            "Body\n"
        );
    }

    #[test]
    fn tail_scores_sit_below_the_floor() {
        assert_eq!(demote_tail_scores(0.4, 3), vec![0.2, 0.13333334, 0.1]);
        assert_eq!(demote_tail_scores(0.0, 2), vec![0.0, 0.0]);
    }

    #[test]
    fn reranking_reorders_the_pool_and_demotes_the_tail() {
        let rows = vec![
            row("a", 0.9, "a"),
            row("b", 0.8, "b"),
            row("c", 0.7, "c"),
            row("d", 0.6, "d"),
        ];
        let provider = FixedProvider(vec![0.1, 0.9]); // pool = first two rows
        let request = RerankRequest {
            query: "q",
            provider: &provider,
            candidates: 2,
            max_document_chars: 2000,
        };
        let page = rerank_and_paginate(rows, 0, 10, &request).expect("rerank");
        let titles: Vec<&str> = page.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(
            titles,
            ["b", "a", "c", "d"],
            "pool reordered, tail kept its order"
        );
        assert_eq!(page[0].score, 0.9);
        assert_eq!(page[1].score, 0.1);
        // The demoted tail stays below the pool's floor (0.1).
        assert!(page[2].score < 0.1 && page[3].score < page[2].score);
    }

    #[test]
    fn candidate_budget_adds_tail_headroom() {
        // max(100, 20*4) = 100 with no tail; page two of ten adds 10 rows of headroom.
        assert_eq!(rerank_candidate_limit(100, 20, 11, 0), 100);
        assert_eq!(rerank_candidate_limit(100, 20, 11, 11), 120);
    }
}
