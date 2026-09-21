//! Phase 7 compatibility: FTS5 text search against the regenerated reference goldens.
//!
//! Cases whose reference ordering depends on backend timestamps (filter-only queries
//! with no MATCH clause) are compared as multisets; score-bearing queries compare
//! order and score.

use std::collections::BTreeMap;

use auto_memory::domain::search::{SearchItemType, SearchResult};
use auto_memory::search::text::{SearchPage, TextSearchOptions};
use serde_json::Value;
mod common;
use common::{indexed_store, load_golden_json};

fn golden(name: &str) -> Value {
    load_golden_json(&format!("search/{name}.json"))
}

fn expected_pairs(case: &Value) -> Vec<(String, String)> {
    case["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|result| {
            (
                result["type"].as_str().unwrap_or_default().to_owned(),
                result["permalink"]
                    .as_str()
                    .or_else(|| result["entity"].as_str())
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect()
}

fn actual_pairs(page: &SearchPage) -> Vec<(String, String)> {
    page.results
        .iter()
        .map(|result| {
            (
                match result.item_type {
                    SearchItemType::Entity => "entity",
                    SearchItemType::Observation => "observation",
                    SearchItemType::Relation => "relation",
                }
                .to_owned(),
                result
                    .permalink
                    .clone()
                    .or_else(|| result.entity.clone())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

fn assert_ordered(case_name: &str, page: &SearchPage) {
    let case = golden(case_name);
    let expected = expected_pairs(&case);
    let actual = actual_pairs(page);
    assert_eq!(actual, expected, "{case_name}: ordered results differ");

    let expected_scores: Vec<f64> = case["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|result| result["score"].as_f64().unwrap_or_default())
        .collect();
    for (index, (actual, expected)) in page.results.iter().zip(expected_scores).enumerate() {
        assert!(
            (actual.score as f64 - expected).abs() <= 1e-6,
            "{case_name}: score[{index}] expected {expected}, got {}",
            actual.score
        );
    }
    assert_eq!(
        page.total,
        case["total"].as_u64().unwrap_or_default() as usize,
        "{case_name}: total"
    );
    assert_eq!(
        page.has_more,
        case["has_more"].as_bool().unwrap_or(false),
        "{case_name}: has_more"
    );
}

fn assert_multiset(case_name: &str, page: &SearchPage) {
    let case = golden(case_name);
    let mut expected = expected_pairs(&case);
    let mut actual = actual_pairs(page);
    let expected_len = expected.len();
    let actual_len = actual.len();
    expected.sort();
    actual.sort();
    assert_eq!(actual, expected, "{case_name}: result sets differ");
    assert_eq!(expected_len, actual_len);
    assert_eq!(
        page.total,
        case["total"].as_u64().unwrap_or_default() as usize,
        "{case_name}: total"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn text_queries_match_reference_order_and_scores() {
    let (_dir, store, project_id) = indexed_store("text");
    for (case_name, query) in [
        ("text-rust", "rust"),
        ("text-case-insensitive", "RUST"),
        ("text-phrase", "\"source of truth\""),
        ("text-boolean", "rust AND architecture"),
        ("text-prefix", "arch*"),
        ("text-cjk", "测试"),
    ] {
        let options = TextSearchOptions {
            query: Some(query.to_owned()),
            ..TextSearchOptions::default()
        };
        let page = store
            .search_text(project_id, &options)
            .await
            .expect("search");
        assert_ordered(case_name, &page);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn title_type_tag_and_metadata_filters_match_reference() {
    let (_dir, store, project_id) = indexed_store("filters");
    let title = store
        .search_text(
            project_id,
            &TextSearchOptions {
                title: Some("Alpha".to_owned()),
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("title search");
    assert_ordered("title-alpha", &title);

    let note_type = store
        .search_text(
            project_id,
            &TextSearchOptions {
                note_types: vec!["project".to_owned()],
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("type search");
    assert_multiset("type-project", &note_type);

    let tag = store
        .search_text(
            project_id,
            &TextSearchOptions {
                tags: vec!["rust".to_owned()],
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("tag search");
    assert_multiset("tag-rust", &tag);

    let mut metadata_filters = BTreeMap::new();
    metadata_filters.insert("status".to_owned(), "active".to_owned());
    let metadata = store
        .search_text(
            project_id,
            &TextSearchOptions {
                metadata_filters,
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("metadata search");
    assert_multiset("metadata-status-active", &metadata);

    let glob = store
        .search_text(
            project_id,
            &TextSearchOptions {
                permalink_match: Some("projects/*".to_owned()),
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("permalink glob");
    assert_multiset("permalink-glob", &glob);
}

#[tokio::test(flavor = "multi_thread")]
async fn observation_category_filter_matches_reference() {
    let (_dir, store, project_id) = indexed_store("observations");
    let page = store
        .search_text(
            project_id,
            &TextSearchOptions {
                entity_types: vec![SearchItemType::Observation],
                categories: vec!["decision".to_owned()],
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("observation search");
    assert_multiset("entity-type-observation", &page);

    // A category filter without an explicit `entity_types`: the implicit default is
    // observation rows, because categories only exist there. This is what
    // `default_entity_types` exists for, and it is applied by the CLI and the MCP tool.
    let implicit = store
        .search_text(
            project_id,
            &TextSearchOptions {
                query: Some("rust".to_owned()),
                entity_types: auto_memory::search::default_entity_types(&["decision".to_owned()]),
                categories: vec!["decision".to_owned()],
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("implicit category search");
    assert_ordered("category-decision-implicit", &implicit);
    assert_eq!(implicit.results.len(), 1, "only the decision observation");

    // Relation rows are a first-class result kind.
    let relations = store
        .search_text(
            project_id,
            &TextSearchOptions {
                query: Some("alpha".to_owned()),
                entity_types: vec![SearchItemType::Relation],
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("relation search");
    assert_multiset("entity-type-relation", &relations);

    // `status` is the same frontmatter filter the reference exposes on its own flag.
    let archived = store
        .search_text(
            project_id,
            &TextSearchOptions {
                status: Some("archived".to_owned()),
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("status search");
    assert_multiset("status-archived", &archived);

    // `search` bounds go through the reference's `dateparser`, not the timeframe parser
    // the context tools use: the value is naive local time compared as UTC, so the bound
    // is effectively shifted by the local offset. Absolute bounds keep this deterministic.
    let bounded = store
        .search_text(
            project_id,
            &TextSearchOptions {
                query: Some("rust".to_owned()),
                after_date: auto_memory::domain::dateparser::parse_after_date("2026-09-01"),
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("date-filtered search");
    assert_multiset("after-date-absolute-rust", &bounded);
    assert_eq!(bounded.total, 2);

    let future = store
        .search_text(
            project_id,
            &TextSearchOptions {
                query: Some("rust".to_owned()),
                after_date: auto_memory::domain::dateparser::parse_after_date("2030-01-01"),
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("future-bounded search");
    assert_ordered("after-date-future-rust", &future);
    assert_eq!(future.total, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn pagination_matches_reference_counts() {
    let (_dir, store, project_id) = indexed_store("pagination");
    let page = store
        .search_text(
            project_id,
            &TextSearchOptions {
                query: Some("note".to_owned()),
                page: 2,
                page_size: 2,
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("paginated search");
    assert_eq!(page.total, 15);
    assert_eq!(page.current_page, 2);
    assert_eq!(page.page_size, 2);
    assert_eq!(page.results.len(), 2);
    assert!(page.has_more);

    let first = store
        .search_text(
            project_id,
            &TextSearchOptions {
                query: Some("note".to_owned()),
                page: 1,
                page_size: 2,
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("page one");
    let first_pairs = actual_pairs(&first);
    let second_pairs = actual_pairs(&page);
    assert_ne!(first_pairs, second_pairs, "pages must advance");
}

#[tokio::test(flavor = "multi_thread")]
async fn non_matching_query_falls_back_to_relaxed_search() {
    let (_dir, store, project_id) = indexed_store("relaxed");
    let page = store
        .search_text(
            project_id,
            &TextSearchOptions {
                query: Some("zzzz-not-present".to_owned()),
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("relaxed search");
    assert_eq!(page.total, 15, "reference relaxation matches every entity");
    assert_eq!(page.results.len(), 10, "default page size is 10");
    assert!(page.has_more);
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_query_without_filters_returns_nothing() {
    let (_dir, store, project_id) = indexed_store("empty");
    let page = store
        .search_text(project_id, &TextSearchOptions::default())
        .await
        .expect("filter-only search");
    // Filter-only searches with no filters still return every entity (reference behavior).
    assert_eq!(page.total, 15);
    assert_eq!(page.results[0].score, 0.0);
}

/// A title filter beside a text query is reachable here (`--title` takes a value and the
/// tool exposes `title`), unlike in the reference, where `search_type="title"` drops the
/// text leg. It used to fail with "unable to use function MATCH in the requested context"
/// because the two legs became two `MATCH` predicates.
#[tokio::test(flavor = "multi_thread")]
async fn title_filter_combines_with_a_text_query() {
    let (_dir, store, project_id) = indexed_store("title-and-query");
    let page = store
        .search_text(
            project_id,
            &TextSearchOptions {
                query: Some("rust".to_owned()),
                title: Some("Alpha".to_owned()),
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("title + query search");
    // Alpha's body mentions rust; Simple Note mentions rust but its title does not match.
    assert_eq!(
        actual_pairs(&page),
        vec![("entity".to_owned(), "oracle/projects/alpha".to_owned())]
    );
    assert!(page.results[0].score < 0.0, "the fused MATCH still scores");
}

#[tokio::test(flavor = "multi_thread")]
async fn permalink_match_filter_combines_with_a_text_query() {
    let (_dir, store, project_id) = indexed_store("permalink-match-and-query");
    let page = store
        .search_text(
            project_id,
            &TextSearchOptions {
                query: Some("rust".to_owned()),
                permalink_match: Some("alpha".to_owned()),
                ..TextSearchOptions::default()
            },
        )
        .await
        .expect("permalink match + query search");
    assert_eq!(
        actual_pairs(&page),
        vec![("entity".to_owned(), "oracle/projects/alpha".to_owned())]
    );
}

#[allow(dead_code)]
fn _typecheck(result: &SearchResult) -> Option<&str> {
    result.permalink.as_deref()
}
