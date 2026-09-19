//! The `search_notes` text surface and its guidance branches.
//!
//! Ports `_format_search_markdown`, `_format_search_error_response` (the
//! semantic-disabled and generic branches), and the tool's "no search criteria" reply.
//! `output_format` defaults to `text`, so these strings — not the JSON payload — are what
//! a caller sees unless it asks otherwise.

use serde_json::Value;
use strum::{EnumIter, EnumString, IntoEnumIterator, IntoStaticStr};

/// The reply for a request that carries no query and no filters.
pub fn no_criteria_guidance() -> &'static str {
    "# No Search Criteria\n\n\
     Please provide at least one of: `query`, `metadata_filters`, `tags`, `status`, \
     `note_types`, `entity_types`, `categories`, or `after_date`."
}

/// Render a search page as compact markdown.
pub fn render_search_markdown(
    payload: &Value,
    project: &str,
    query: Option<&str>,
    project_id: Option<&str>,
) -> String {
    let results = payload["results"].as_array().cloned().unwrap_or_default();
    let query = query.unwrap_or_default();

    if results.is_empty() {
        // An empty page usually means "no match", not "empty knowledge base", so the
        // getting-started guidance stays with `recent_activity`.
        let suggestion = if project == "all projects" {
            "call list_memory_projects() to see what exists across your projects".to_owned()
        } else if let Some(project_id) = project_id {
            format!(
                "call recent_activity(project_id=\"{project_id}\") to orient — if the project is \
                 empty it will guide creating a first note"
            )
        } else {
            format!(
                "call recent_activity(project=\"{project}\") to orient — if the project is empty \
                 it will guide creating a first note"
            )
        };
        return format!(
            "No results found for '{query}' in project '{project}'. Try broader or different \
             terms, or {suggestion}."
        );
    }

    let mut parts: Vec<String> = Vec::new();
    if query.is_empty() {
        parts.push("# Search Results".to_owned());
    } else {
        parts.push(format!("# Search Results: {query}"));
    }
    parts.push(format!("*project: {project}*"));
    parts.push(String::new());

    for result in &results {
        parts.push(format!(
            "### {}",
            result["title"].as_str().unwrap_or_default()
        ));
        parts.push(format!(
            "- permalink: {}",
            result["permalink"].as_str().unwrap_or_default()
        ));
        if let Some(external_id) = result["external_id"].as_str() {
            parts.push(format!("- external_id: {external_id}"));
        }
        parts.push(format!(
            "- score: {:.4}",
            result["score"].as_f64().unwrap_or_default()
        ));
        if let Some(matched) = result["matched_chunk"].as_str() {
            let truncated: String = matched.chars().take(200).collect();
            parts.push(format!("- match: {truncated}"));
        }
        parts.push(String::new());
    }

    parts.push("---".to_owned());
    let count = results.len();
    let plural = if count == 1 { "" } else { "s" };
    let more = if payload["has_more"].as_bool().unwrap_or(false) {
        " | more available"
    } else {
        ""
    };
    parts.push(format!(
        "*{count} result{plural} | page {}, page_size {}{more}*",
        payload["current_page"].as_u64().unwrap_or(1),
        payload["page_size"].as_u64().unwrap_or(10),
    ));

    parts.join("\n")
}

/// The reply when a semantic search type is requested without an embedding runtime.
pub fn semantic_disabled_guidance(project: &str, query: &str, search_type: &str) -> String {
    format!(
        "# Search Failed - Semantic Search Disabled\n\n\
         You requested `{search_type}` search for query '{query}', but semantic search is \
         disabled.\n\n\
         ## How to enable\n\
         1. Set `BASIC_MEMORY_SEMANTIC_SEARCH_ENABLED=true`\n\
         2. Restart the Basic Memory server/process\n\n\
         ## Alternative now\n\
         - Run FTS search instead:\n  \
         `search_notes(\"{project}\", \"{query}\", search_type=\"text\")`"
    )
}

/// The generic failure reply: an invalid `search_type`, a search error, and so on.
pub fn search_failed_guidance(project: &str, query: &str, error: &str) -> String {
    let terms: Vec<&str> = query.split_whitespace().collect();
    let boolean_variation = terms
        .iter()
        .take(2)
        .copied()
        .collect::<Vec<_>>()
        .join(" OR ");
    format!(
        "# Search Failed\n\n\
         Error searching for '{query}': {error}\n\n\
         ## Troubleshooting steps:\n\
         1. **Simplify your query**: Try basic words without special characters\n\
         2. **Check search syntax**: Ensure boolean operators are correctly formatted\n\
         3. **Verify project access**: Make sure you can access the current project\n\
         4. **Test with simple search**: Try `search_notes(\"test\")` to verify search is working\n\n\
         ## Alternative search approaches:\n\
         - **Different search types**: \n  \
         - Title only: `search_notes(\"{project}\",\"{query}\", search_type=\"title\")`\n  \
         - Permalink patterns: `search_notes(\"{project}\",\"{query}*\", search_type=\"permalink\")`\n\
         - **With filters**: `search_notes(\"{project}\",\"{query}\", note_types=[\"note\"])`\n\
         - **Recent content**: `search_notes(\"{project}\",\"{query}\", after_date=\"1 week\")`\n\
         - **Boolean variations**: `search_notes(\"{project}\",\"{boolean_variation}\")`\n\n\
         ## Explore your content:\n\
         - **Browse files**: `list_directory(\"{project}\",\"/\")` - See all available content\n\
         - **Recent activity**: `recent_activity(timeframe=\"7d\")` - Check what's been updated\n\
         - **All projects**: `list_projects()` \n\n\
         ## Search syntax reference:\n\
         - **Basic**: `keyword` or `multiple words`\n\
         - **Boolean**: `term1 AND term2`, `term1 OR term2`, `term1 NOT term2`\n\
         - **Phrases**: `\"exact phrase\"`\n\
         - **Grouping**: `(term1 OR term2) AND term3`\n\
         - **Tags**: `tag:example`\n\
         - **Observation categories**: `entity_types=[\"observation\"], categories=[\"requirement\"]`"
    )
}

/// The reference's `search_type` vocabulary.
///
/// The variants are declared in the order the invalid-type error lists them
/// (`Valid options: hybrid, permalink, semantic, text, title, vector`), which
/// [`SearchType::valid_options`] reproduces from the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum SearchType {
    /// FTS + vector fusion.
    Hybrid,
    /// Permalink filter on the query text (exact, or a `*` glob).
    Permalink,
    /// Vector-only retrieval; needs an embedding runtime.
    Semantic,
    /// FTS5 text search (the default).
    Text,
    /// Title filter on the query text.
    Title,
    /// Vector-only retrieval; needs an embedding runtime.
    Vector,
}

impl SearchType {
    /// The comma-separated vocabulary, spelled the way the reference's
    /// invalid-`search_type` error does.
    pub fn valid_options() -> String {
        Self::iter()
            .map(<&str>::from)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::{render_search_markdown, search_failed_guidance, semantic_disabled_guidance};
    use serde_json::json;

    #[test]
    fn empty_results_point_at_recent_activity() {
        let payload = json!({"results": [], "current_page": 1, "page_size": 10});
        assert_eq!(
            render_search_markdown(&payload, "oracle", Some("rust"), Some("uuid-1")),
            "No results found for 'rust' in project 'oracle'. Try broader or different terms, or \
             call recent_activity(project_id=\"uuid-1\") to orient — if the project is empty it \
             will guide creating a first note."
        );
    }

    #[test]
    fn results_render_as_blocks_with_a_pagination_footer() {
        let payload = json!({
            "results": [
                {"title": "Alpha", "permalink": "oracle/alpha", "external_id": "uuid-1",
                 "score": -1.5},
                {"title": "Beta", "permalink": "oracle/beta", "score": 0.25,
                 "matched_chunk": "chunk text"},
            ],
            "current_page": 2,
            "page_size": 5,
            "has_more": true,
        });
        assert_eq!(
            render_search_markdown(&payload, "oracle", Some("rust"), None),
            "# Search Results: rust\n\
             *project: oracle*\n\n\
             ### Alpha\n\
             - permalink: oracle/alpha\n\
             - external_id: uuid-1\n\
             - score: -1.5000\n\n\
             ### Beta\n\
             - permalink: oracle/beta\n\
             - score: 0.2500\n\
             - match: chunk text\n\n\
             ---\n\
             *2 results | page 2, page_size 5 | more available*"
        );
    }

    #[test]
    fn guidance_templates_name_the_request() {
        let disabled = semantic_disabled_guidance("oracle", "rust", "vector");
        assert!(disabled.starts_with("# Search Failed - Semantic Search Disabled\n\n"));
        assert!(disabled.contains("You requested `vector` search for query 'rust'"));
        assert!(disabled.ends_with("`search_notes(\"oracle\", \"rust\", search_type=\"text\")`"));

        let failed = search_failed_guidance("oracle", "rust", "Invalid search_type 'x'.");
        assert!(failed.starts_with("# Search Failed\n\nError searching for 'rust': "));
        assert!(failed.contains("- **Boolean variations**: `search_notes(\"oracle\",\"rust\")`"));
    }
}
