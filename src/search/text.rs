//! FTS5 text search with reference-compatible filters, ordering, and pagination.
//!
//! The reference runs text search directly against the FTS5 table (no join) and
//! filters on entity columns through subqueries, because column-scoped `MATCH`
//! cannot be combined with a join in the same query level. `entity`/`external_id`
//! are filled in afterwards with a lookup.

use std::collections::{BTreeMap, HashMap};

use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};
use serde_json::{Map, Value as JsonValue};

use crate::domain::search::{SearchItemType, SearchResult};
use crate::error::{Error, Result};
use crate::search::query::{
    prepare_fts_query, prepare_single_term, prepare_title_query, relaxed_query,
};

/// Maximum display length of the reference `content` field.
const CONTENT_DISPLAY_LIMIT: usize = 4000;

/// Options for one text-search page.
#[derive(Debug, Clone)]
pub struct TextSearchOptions {
    /// Full-text query.
    pub query: Option<String>,
    /// Exact permalink filter.
    pub permalink: Option<String>,
    /// Glob permalink filter (`*` supported).
    pub permalink_match: Option<String>,
    /// Title substring filter.
    pub title: Option<String>,
    /// Note type filter (frontmatter `type`, case-insensitive).
    pub note_types: Vec<String>,
    /// Indexed row type filter (defaults to entity rows, like `search_notes`).
    pub entity_types: Vec<SearchItemType>,
    /// Observation category filter.
    pub categories: Vec<String>,
    /// Frontmatter/observation tag filter.
    pub tags: Vec<String>,
    /// Frontmatter `status` filter.
    pub status: Option<String>,
    /// Additional frontmatter key/value filters.
    pub metadata_filters: BTreeMap<String, String>,
    /// Only rows updated after this timestamp.
    pub after_date: Option<String>,
    /// One-based page number.
    pub page: u32,
    /// Page size.
    pub page_size: u32,
}

impl Default for TextSearchOptions {
    fn default() -> Self {
        Self {
            query: None,
            permalink: None,
            permalink_match: None,
            title: None,
            note_types: Vec::new(),
            entity_types: vec![SearchItemType::Entity],
            categories: Vec::new(),
            tags: Vec::new(),
            status: None,
            metadata_filters: BTreeMap::new(),
            after_date: None,
            page: 1,
            page_size: 10,
        }
    }
}

/// The implicit `entity_types` default for a query.
///
/// The reference applies this after deciding a request has criteria: entity rows
/// otherwise, but **observation** rows when a category filter was supplied, because
/// categories only exist on observation rows — defaulting to entity rows would AND the
/// category against a `NULL` column and return nothing.
pub fn default_entity_types(categories: &[String]) -> Vec<SearchItemType> {
    if categories.is_empty() {
        vec![SearchItemType::Entity]
    } else {
        vec![SearchItemType::Observation]
    }
}

/// One page of search results.
#[derive(Debug, Clone)]
pub struct SearchPage {
    /// Result rows in reference order.
    pub results: Vec<SearchResult>,
    /// Total matching rows.
    pub total: usize,
    /// Whether the total is exact.
    pub total_is_exact: bool,
    /// Whether more rows exist after this page.
    pub has_more: bool,
    /// Echoed page number.
    pub current_page: u32,
    /// Echoed page size.
    pub page_size: u32,
}

/// Run a text search against the derived index.
pub fn search_text(
    conn: &Connection,
    project_id: i64,
    options: &TextSearchOptions,
) -> Result<SearchPage> {
    let page = options.page.max(1);
    let page_size = options.page_size.max(1);

    let strict = options.query.as_deref().and_then(prepare_fts_query);
    let result = run_query(
        conn,
        project_id,
        options,
        strict.as_deref(),
        page,
        page_size,
    )?;
    if result.total == 0 {
        if let Some(input) = options.query.as_deref() {
            if let Some(relaxed) = relaxed_query(input) {
                return run_query(conn, project_id, options, Some(&relaxed), page, page_size);
            }
        }
    }
    Ok(result)
}

fn run_query(
    conn: &Connection,
    project_id: i64,
    options: &TextSearchOptions,
    match_query: Option<&str>,
    page: u32,
    page_size: u32,
) -> Result<SearchPage> {
    let (where_clause, mut params) = build_filters(project_id, options, match_query);
    let from_clause = "FROM search_index";

    let count_sql = format!("SELECT count(*) {from_clause} WHERE {where_clause}");
    let total: i64 = conn.query_row(&count_sql, params_from_iter(params.iter()), |row| {
        row.get(0)
    })?;

    let has_match = match_query.is_some() || options.title.is_some();
    let score_expr = if has_match {
        "bm25(search_index)"
    } else {
        "0.0"
    };
    let order_clause = if has_match {
        if options.after_date.is_some() {
            "score ASC, search_index.updated_at DESC"
        } else {
            "score ASC"
        }
    } else {
        "search_index.updated_at DESC"
    };

    let offset = i64::from(page.saturating_sub(1)) * i64::from(page_size);
    let sql = format!(
        "SELECT search_index.id, search_index.title, search_index.type, search_index.permalink,
                search_index.file_path, search_index.content_snippet, search_index.metadata,
                search_index.entity_id, search_index.category, search_index.relation_type,
                search_index.updated_at, {score_expr} AS score
         {from_clause}
         WHERE {where_clause}
         ORDER BY {order_clause}
         LIMIT ? OFFSET ?"
    );
    params.push(Value::Integer(i64::from(page_size)));
    params.push(Value::Integer(offset));

    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(params.iter()), |row| {
        Ok(RawRow {
            id: row.get(0)?,
            title: row.get(1)?,
            item_type: row.get(2)?,
            permalink: row.get(3)?,
            file_path: row.get(4)?,
            snippet: row.get(5)?,
            metadata: row.get(6)?,
            entity_id: row.get(7)?,
            category: row.get(8)?,
            relation_type: row.get(9)?,
            updated_at: row.get(10)?,
            score: row.get(11)?,
        })
    })?;
    let raw_rows = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);

    let entity_ids: Vec<i64> = raw_rows.iter().filter_map(|row| row.entity_id).collect();
    let entity_info = lookup_entities(conn, &entity_ids)?;

    let mut results = Vec::with_capacity(raw_rows.len());
    for row in &raw_rows {
        let parsed_type =
            row.item_type
                .parse::<SearchItemType>()
                .map_err(|_| Error::Frontmatter {
                    message: format!("unknown search row type: {}", row.item_type),
                })?;
        let (entity_permalink, entity_external_id) = row
            .entity_id
            .and_then(|id| entity_info.get(&id).cloned())
            .unwrap_or_default();
        let content = row.snippet.as_deref().map(truncate_content);
        results.push(SearchResult {
            title: row.title.clone().unwrap_or_default(),
            item_type: parsed_type,
            score: row.score as f32,
            entity: entity_permalink,
            external_id: entity_external_id,
            permalink: row.permalink.clone(),
            content: content.clone(),
            // The reference only fills `matched_chunk` for vector/hybrid hits; FTS
            // results omit the field entirely.
            matched_chunk: None,
            file_path: row.file_path.clone(),
            updated_at: row.updated_at.clone(),
            metadata: row
                .metadata
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Map<String, JsonValue>>(raw).ok()),
            entity_id: row.entity_id,
            observation_id: (parsed_type == SearchItemType::Observation).then_some(row.id),
            relation_id: (parsed_type == SearchItemType::Relation).then_some(row.id),
            category: row.category.clone(),
            from_entity: None,
            to_entity: None,
            relation_type: row.relation_type.clone(),
        });
    }

    let total = usize::try_from(total).unwrap_or(usize::MAX);
    let has_more = (offset as usize + results.len()) < total;
    Ok(SearchPage {
        results,
        total,
        total_is_exact: true,
        has_more,
        current_page: page,
        page_size,
    })
}

struct RawRow {
    id: i64,
    title: Option<String>,
    item_type: String,
    permalink: Option<String>,
    file_path: String,
    snippet: Option<String>,
    metadata: Option<String>,
    entity_id: Option<i64>,
    category: Option<String>,
    relation_type: Option<String>,
    updated_at: Option<String>,
    score: f64,
}

/// Truncate a snippet to the reference display limit.
pub(crate) fn truncate_content(text: &str) -> String {
    if text.chars().count() > CONTENT_DISPLAY_LIMIT {
        text.chars().take(CONTENT_DISPLAY_LIMIT).collect()
    } else {
        text.to_owned()
    }
}

/// `(permalink, external_id)` for one entity.
type EntityInfo = (Option<String>, Option<String>);

fn lookup_entities(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, EntityInfo>> {
    let mut info = HashMap::new();
    let mut unique: Vec<i64> = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    for id in unique {
        let row = conn
            .query_row(
                "SELECT permalink, external_id FROM entity WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .ok();
        if let Some(value) = row {
            info.insert(id, value);
        }
    }
    Ok(info)
}

/// One `MATCH` predicate, kept symbolic until every filter is known.
///
/// FTS5 rejects a query that puts an OR-connected `MATCH` group beside a second `MATCH`
/// constraint ("unable to use function MATCH in the requested context"), which is what
/// `--title`/`--permalink-match` combined with a text query used to produce. Collecting
/// the legs first lets [`match_filter`] decide between the historical one-clause form and
/// a single table-level `MATCH`.
enum MatchPredicate {
    /// The text leg, which the reference scopes to three columns with OR.
    Text(String),
    /// A title-only leg.
    Title(String),
    /// A single-term permalink leg.
    Permalink(String),
}

impl MatchPredicate {
    /// The FTS5 expression reproducing this leg's scope inside a shared `MATCH`.
    fn fts_expression(&self) -> String {
        match self {
            Self::Text(query) => format!("{{title content_stems content_snippet}} : ({query})"),
            Self::Title(title) => format!("{{title}} : ({title})"),
            Self::Permalink(permalink) => format!("{{permalink}} : ({permalink})"),
        }
    }
}

/// Fold the collected predicates into one WHERE fragment plus its parameters.
///
/// A single predicate keeps the SQL the goldens pin verbatim. Two or more become one
/// `search_index MATCH ?` whose column filters (`{title} : (…)`) carry the scopes the
/// separate clauses carried, because FTS5 only accepts that shape.
fn match_filter(predicates: &[MatchPredicate]) -> Option<(String, Vec<Value>)> {
    match predicates {
        [] => None,
        [MatchPredicate::Text(query)] => Some((
            "(search_index.title MATCH ? OR search_index.content_stems MATCH ? OR \
             search_index.content_snippet MATCH ?)"
                .to_owned(),
            vec![Value::Text(query.clone()); 3],
        )),
        [MatchPredicate::Title(title)] => Some((
            "search_index.title MATCH ?".to_owned(),
            vec![Value::Text(title.clone())],
        )),
        [MatchPredicate::Permalink(permalink)] => Some((
            "search_index.permalink MATCH ?".to_owned(),
            vec![Value::Text(permalink.clone())],
        )),
        many => {
            let expression = many
                .iter()
                .map(MatchPredicate::fts_expression)
                .collect::<Vec<_>>()
                .join(" AND ");
            Some((
                "search_index MATCH ?".to_owned(),
                vec![Value::Text(expression)],
            ))
        }
    }
}

fn build_filters(
    project_id: i64,
    options: &TextSearchOptions,
    match_query: Option<&str>,
) -> (String, Vec<Value>) {
    let mut clauses = vec!["search_index.project_id = ?".to_owned()];
    let mut params: Vec<Value> = vec![Value::Integer(project_id)];
    let mut matches: Vec<MatchPredicate> = Vec::new();

    if let Some(query) = match_query {
        matches.push(MatchPredicate::Text(query.to_owned()));
    }

    let entity_types: Vec<&str> = if options.entity_types.is_empty() {
        vec!["entity"]
    } else {
        options
            .entity_types
            .iter()
            .map(|item| <&str>::from(*item))
            .collect()
    };
    clauses.push(format!(
        "search_index.type IN ({})",
        placeholders(entity_types.len())
    ));
    params.extend(
        entity_types
            .iter()
            .map(|value| Value::Text((*value).to_owned())),
    );

    if !options.note_types.is_empty() {
        clauses.push(format!(
            "LOWER(json_extract(search_index.metadata, '$.note_type')) IN ({})",
            placeholders(options.note_types.len())
        ));
        params.extend(
            options
                .note_types
                .iter()
                .map(|value| Value::Text(value.to_lowercase())),
        );
    }

    if !options.categories.is_empty() {
        clauses.push(format!(
            "search_index.category IN ({})",
            placeholders(options.categories.len())
        ));
        params.extend(options.categories.iter().cloned().map(Value::Text));
    }

    if !options.tags.is_empty() {
        clauses.push(
            "(search_index.entity_id IN (SELECT id FROM entity WHERE project_id = ? \
              AND EXISTS (SELECT 1 FROM json_each(COALESCE(json_extract(entity_metadata, '$.tags'), '[]')) \
                          WHERE json_each.value = ?)) \
              OR EXISTS (SELECT 1 FROM json_each(COALESCE(json_extract(search_index.metadata, '$.tags'), '[]')) \
                         WHERE json_each.value = ?))"
                .to_owned(),
        );
        params.push(Value::Integer(project_id));
        params.push(Value::Text(options.tags[0].clone()));
        params.push(Value::Text(options.tags[0].clone()));
    }

    if let Some(status) = &options.status {
        clauses.push(
            "search_index.entity_id IN (SELECT id FROM entity WHERE project_id = ? \
             AND json_extract(entity_metadata, '$.status') = ?)"
                .to_owned(),
        );
        params.push(Value::Integer(project_id));
        params.push(Value::Text(status.clone()));
    }

    for (key, value) in &options.metadata_filters {
        if key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            clauses.push(format!(
                "search_index.entity_id IN (SELECT id FROM entity WHERE project_id = ? \
                 AND json_extract(entity_metadata, '$.{key}') = ?)"
            ));
            params.push(Value::Integer(project_id));
            params.push(Value::Text(value.clone()));
        }
    }

    if let Some(title) = &options.title {
        if let Some(prepared) = prepare_title_query(title) {
            matches.push(MatchPredicate::Title(prepared));
        }
    }

    if let Some(permalink) = &options.permalink {
        clauses.push("search_index.permalink = ?".to_owned());
        params.push(Value::Text(permalink.clone()));
    }

    if let Some(permalink_match) = &options.permalink_match {
        let pattern = permalink_match.to_lowercase();
        if pattern.contains('*') {
            clauses.push("search_index.permalink GLOB ?".to_owned());
            params.push(Value::Text(pattern));
        } else if pattern.contains('/') {
            clauses.push("search_index.permalink = ?".to_owned());
            params.push(Value::Text(pattern));
        } else {
            matches.push(MatchPredicate::Permalink(prepare_single_term(
                &pattern, false,
            )));
        }
    }

    if let Some(after_date) = &options.after_date {
        clauses.push("datetime(search_index.updated_at) > datetime(?)".to_owned());
        params.push(Value::Text(after_date.clone()));
    }

    if let Some((clause, match_params)) = match_filter(&matches) {
        // The MATCH predicate sits directly after the project filter, so its parameters
        // sit directly after the project id.
        clauses.insert(1, clause);
        params.splice(1..1, match_params);
    }

    (clauses.join(" AND "), params)
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_sql_defaults_to_entity_rows() {
        let (clause, params) = build_filters(1, &TextSearchOptions::default(), None);
        assert!(clause.contains("search_index.type IN (?)"));
        assert_eq!(params[0], Value::Integer(1));
        assert_eq!(params[1], Value::Text("entity".to_owned()));
    }

    #[test]
    fn metadata_filter_rejects_unsafe_keys() {
        let mut options = TextSearchOptions::default();
        options
            .metadata_filters
            .insert("status".to_owned(), "active".to_owned());
        options
            .metadata_filters
            .insert("bad-key".to_owned(), "x".to_owned());
        let (clause, params) = build_filters(1, &options, None);
        assert!(clause.contains("$.status"));
        assert!(!clause.contains("bad-key"));
        assert!(params.contains(&Value::Text("active".to_owned())));
    }

    #[test]
    fn text_match_uses_column_scoped_or() {
        let (clause, params) = build_filters(1, &TextSearchOptions::default(), Some("rust*"));
        assert!(clause.contains("content_stems MATCH"));
        assert_eq!(
            params
                .iter()
                .filter(|value| **value == Value::Text("rust*".to_owned()))
                .count(),
            3
        );
    }

    /// `--title`/`--permalink-match` beside a text query used to emit two MATCH
    /// predicates, and FTS5 rejects that shape with "unable to use function MATCH in the
    /// requested context" (the OR group is what it cannot combine). The two legs now
    /// share one table-level MATCH whose column filters keep their scopes.
    #[test]
    fn title_and_text_legs_share_one_match_constraint() {
        let options = TextSearchOptions {
            title: Some("Alpha".to_owned()),
            ..TextSearchOptions::default()
        };
        let (clause, params) = build_filters(1, &options, Some("rust*"));
        assert_eq!(clause.matches("MATCH").count(), 1, "{clause}");
        assert!(clause.contains("search_index MATCH ?"), "{clause}");
        assert_eq!(params[0], Value::Integer(1));
        assert_eq!(
            params[1],
            Value::Text(
                "{title content_stems content_snippet} : (rust*) AND {title} : (Alpha)".to_owned()
            )
        );
    }

    #[test]
    fn permalink_match_and_text_legs_share_one_match_constraint() {
        let options = TextSearchOptions {
            permalink_match: Some("alpha".to_owned()),
            ..TextSearchOptions::default()
        };
        let (clause, params) = build_filters(1, &options, Some("rust*"));
        assert_eq!(clause.matches("MATCH").count(), 1, "{clause}");
        assert_eq!(
            params[1],
            Value::Text(
                "{title content_stems content_snippet} : (rust*) AND {permalink} : (alpha)"
                    .to_owned()
            )
        );
    }
}
