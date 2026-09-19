//! `build_context`: resolve a `memory://` URI and assemble bounded graph context.
//!
//! Mirrors reference `ContextService.build_context` plus the API hydration layer
//! (`api/v2/utils.py`) that turns traversal rows into response summaries: the
//! related set keeps the traversal order, relation targets are hydrated by id,
//! observations are owned by the primary entity, and metadata counts are derived
//! from the rows that survived the traversal limit.

use std::collections::{BTreeSet, HashMap};

use serde::Serialize;
use serde_json::Value;

use crate::domain::timeframe::{self, Instant};
use crate::error::{Error, Result};
use crate::graph::{memory_url_path, normalize_memory_url, resolve_entity_path};
use crate::search::index_rows::observation_permalink_suffix;
use crate::storage::{EntityRow, RelatedRow, Store};

/// Reference `DEFAULT_CONTEXT_PAGE_SIZE`.
pub const DEFAULT_PAGE_SIZE: u32 = 10;
/// Reference `MAX_CONTEXT_PAGE_SIZE`.
pub const MAX_PAGE_SIZE: u32 = 50;
/// Reference `DEFAULT_CONTEXT_RELATED_RESULTS`.
pub const DEFAULT_MAX_RELATED: u32 = 10;
/// Reference `MAX_CONTEXT_RELATED_RESULTS`.
pub const MAX_RELATED_RESULTS: u32 = 100;

/// Context assembly options (reference defaults: depth 1, page 1, page size 10,
/// max related 10, timeframe `7d` applied by the CLI/MCP entry points).
#[derive(Debug, Clone, Copy)]
pub struct ContextOptions {
    /// Logical traversal depth (internally doubled: relation → entity).
    pub depth: u32,
    /// Maximum related rows.
    pub max_related: u32,
    /// One-based page number.
    pub page: u32,
    /// Page size.
    pub page_size: u32,
    /// Resolved `since` bound; `None` disables the timeframe filter.
    pub since: Option<Instant>,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            depth: 1,
            max_related: DEFAULT_MAX_RELATED,
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
            since: None,
        }
    }
}

impl ContextOptions {
    /// Validate pagination and traversal bounds the way the MCP tool does.
    pub fn validate(&self) -> Result<()> {
        if self.page < 1 {
            return Err(invalid_arg(format!("page must be >= 1, got {}", self.page)));
        }
        if self.page_size < 1 {
            return Err(invalid_arg(format!(
                "page_size must be >= 1, got {}",
                self.page_size
            )));
        }
        if self.page_size > MAX_PAGE_SIZE {
            return Err(invalid_arg(format!(
                "page_size must be <= {MAX_PAGE_SIZE}, got {}. build_context is a bounded traversal \
                 starting point; request another page or follow the returned memory:// links.",
                self.page_size
            )));
        }
        if self.max_related > MAX_RELATED_RESULTS {
            return Err(invalid_arg(format!(
                "max_related must be <= {MAX_RELATED_RESULTS}, got {}. build_context is a bounded \
                 traversal starting point; follow the returned memory:// links for more context.",
                self.max_related
            )));
        }
        Ok(())
    }
}

/// One entity summary in a context result.
#[derive(Debug, Clone, Serialize)]
pub struct EntitySummary {
    /// Always `entity`.
    #[serde(rename = "type")]
    pub item_type: &'static str,
    /// Stable external id.
    pub external_id: String,
    /// Numeric entity id.
    pub entity_id: i64,
    /// Permalink.
    pub permalink: Option<String>,
    /// Title.
    pub title: String,
    /// Note body; related entities carry no body (reference hydration leaves it null).
    pub content: Option<String>,
    /// Project-relative file path.
    pub file_path: String,
    /// Indexed creation timestamp.
    pub created_at: String,
}

/// One observation summary.
#[derive(Debug, Clone, Serialize)]
pub struct ObservationSummary {
    /// Always `observation`.
    #[serde(rename = "type")]
    pub item_type: &'static str,
    /// Observation id.
    pub observation_id: i64,
    /// Owning entity id.
    pub entity_id: i64,
    /// Owning entity external id.
    pub entity_external_id: String,
    /// Owning entity title (the reference hydrates `title` from the entity).
    pub title: Option<String>,
    /// Project-relative file path.
    pub file_path: String,
    /// Synthetic observation permalink.
    pub permalink: String,
    /// Category.
    pub category: String,
    /// Content.
    pub content: String,
    /// Indexed creation timestamp.
    pub created_at: String,
}

/// One related relation summary.
#[derive(Debug, Clone, Serialize)]
pub struct RelationSummary {
    /// Always `relation`.
    #[serde(rename = "type")]
    pub item_type: &'static str,
    /// Relation id.
    pub relation_id: i64,
    /// Owning search row's entity id (the traversal leaves this null for relations).
    pub entity_id: Option<i64>,
    /// `relation_type: to_name`.
    pub title: String,
    /// Source file path.
    pub file_path: String,
    /// Relation permalink (the reference renders an empty string here).
    pub permalink: String,
    /// Relation label.
    pub relation_type: String,
    /// Source entity title.
    pub from_entity: Option<String>,
    /// Source entity id.
    pub from_entity_id: Option<i64>,
    /// Source entity external id.
    pub from_entity_external_id: Option<String>,
    /// Target entity title.
    pub to_entity: Option<String>,
    /// Raw target text.
    pub to_name: Option<String>,
    /// Target entity id.
    pub to_entity_id: Option<i64>,
    /// Target entity external id.
    pub to_entity_external_id: Option<String>,
    /// Indexed creation timestamp.
    pub created_at: String,
}

/// One primary item with its observations and related summaries.
#[derive(Debug, Clone, Serialize)]
pub struct ContextResult {
    /// Primary entity summary.
    pub primary_result: EntitySummary,
    /// Observations owned by the primary entity.
    pub observations: Vec<ObservationSummary>,
    /// Related summaries in traversal order: relation rows and the entities they
    /// connect, as returned by the reference traversal.
    pub related_results: Vec<Value>,
}

/// Context metadata (reference `MemoryMetadata`).
#[derive(Debug, Clone, Serialize)]
pub struct MemoryMetadata {
    /// Normalized memory URL path; `None` for a request with no URL (recent activity).
    pub uri: Option<String>,
    /// Requested item types.
    pub types: Option<Vec<String>>,
    /// Requested depth.
    pub depth: u32,
    /// Timeframe bound echoed back to the caller (ISO 8601).
    pub timeframe: Option<String>,
    /// Generation timestamp (RFC 3339 in the reference).
    pub generated_at: String,
    /// Primary item count.
    pub primary_count: usize,
    /// Related item count.
    pub related_count: usize,
    /// Total results (`primary_count + related_count`).
    pub total_results: usize,
    /// Related relation count.
    pub total_relations: usize,
    /// Observation count across the primary and related entities.
    pub total_observations: usize,
}

/// Context graph returned by `build_context`.
#[derive(Debug, Clone, Serialize)]
pub struct GraphContext {
    /// Context results (one per primary item).
    pub results: Vec<ContextResult>,
    /// Metadata.
    pub metadata: MemoryMetadata,
    /// Page number.
    pub page: u32,
    /// Page size.
    pub page_size: u32,
    /// Whether more results exist.
    pub has_more: bool,
}

/// Build context for a `memory://` URL.
pub fn build_context(
    store: &Store,
    project_id: i64,
    url: &str,
    options: &ContextOptions,
) -> Result<GraphContext> {
    options.validate()?;

    let normalized = normalize_memory_url(url)?;
    let path = memory_url_path(&normalized).to_owned();
    let offset = (options.page - 1) * options.page_size;

    // The reference resolves the requested path through the link resolver first and
    // then loads `search(permalink, limit=page_size+1, offset=offset)`; an unresolved
    // URL — or any page past the first for a direct lookup — is an empty result set
    // rather than an error, and the metadata still echoes the resolved permalink.
    let resolved = resolve_entity_path(store, project_id, &path)?;
    let resolved_uri = resolved
        .as_ref()
        .and_then(|entity| entity.permalink.clone())
        .unwrap_or_else(|| path.clone());
    let Some(entity) = resolved.filter(|_| offset == 0) else {
        return Ok(GraphContext {
            results: Vec::new(),
            metadata: empty_metadata(&resolved_uri, options),
            page: options.page,
            page_size: options.page_size,
            has_more: false,
        });
    };

    let since = options
        .since
        .map(|instant| timeframe::since_bound(&instant));
    let rows = store.find_related(
        project_id,
        &[entity.id],
        options.depth,
        options.max_related,
        since.as_deref(),
    )?;
    let entity_lookup = hydration_lookup(store, &[entity.id], &rows)?;

    let observations = observation_summaries(store, &entity, &entity_lookup)?;
    let mut total_observations = observations.len();

    let mut related_results = Vec::with_capacity(rows.len());
    let mut related_entity_ids: BTreeSet<i64> = BTreeSet::new();
    let mut total_relations = 0_usize;
    for row in &rows {
        match row.item_type.as_str() {
            "relation" => {
                total_relations += 1;
                related_results.push(serde_json::to_value(relation_summary(row, &entity_lookup))?);
            }
            _ => {
                if related_entity_ids.insert(row.id) {
                    total_observations += store.observations_for_entity(row.id)?.len();
                }
                related_results.push(serde_json::to_value(entity_summary(row, &entity_lookup))?);
            }
        }
    }

    let related_count = related_results.len();
    let primary_result = EntitySummary {
        item_type: "entity",
        external_id: entity.external_id.clone(),
        entity_id: entity.id,
        permalink: entity.permalink.clone(),
        title: entity.title.clone(),
        content: Some(store.entity_content(entity.id).unwrap_or_default()),
        file_path: entity.file_path.clone(),
        created_at: store.entity_created_at(entity.id)?.unwrap_or_default(),
    };

    Ok(GraphContext {
        results: vec![ContextResult {
            primary_result,
            observations,
            related_results,
        }],
        metadata: MemoryMetadata {
            // The reference echoes the resolved permalink once the link resolver
            // has redirected the requested path.
            uri: Some(entity.permalink.clone().unwrap_or(path)),
            types: None,
            depth: options.depth,
            timeframe: since,
            generated_at: timeframe::utc_now_iso(),
            primary_count: 1,
            related_count,
            total_results: 1 + related_count,
            total_relations,
            total_observations,
        },
        page: options.page,
        page_size: options.page_size,
        has_more: false,
    })
}

/// Metadata for a request whose primary item does not resolve to anything.
fn empty_metadata(path: &str, options: &ContextOptions) -> MemoryMetadata {
    MemoryMetadata {
        uri: Some(path.to_owned()),
        types: None,
        depth: options.depth,
        timeframe: options
            .since
            .map(|instant| timeframe::since_bound(&instant)),
        generated_at: timeframe::utc_now_iso(),
        primary_count: 0,
        related_count: 0,
        total_results: 0,
        total_relations: 0,
        total_observations: 0,
    }
}

/// Entity titles and external ids for every id the response has to hydrate.
pub(crate) fn hydration_lookup(
    store: &Store,
    primary_ids: &[i64],
    rows: &[RelatedRow],
) -> Result<HashMap<i64, (String, String)>> {
    let mut ids: BTreeSet<i64> = primary_ids.iter().copied().collect();
    for row in rows {
        ids.insert(row.id);
        if let Some(from_id) = row.from_id {
            ids.insert(from_id);
        }
        if let Some(to_id) = row.to_id {
            ids.insert(to_id);
        }
    }
    store.entity_titles_and_external_ids(&ids.into_iter().collect::<Vec<_>>())
}

/// Build the observation summaries owned by the primary entity.
pub(crate) fn observation_summaries(
    store: &Store,
    entity: &EntityRow,
    lookup: &HashMap<i64, (String, String)>,
) -> Result<Vec<ObservationSummary>> {
    let created_at = store.entity_created_at(entity.id)?.unwrap_or_default();
    let entity_external_id = lookup.get(&entity.id).map_or_else(
        || entity.external_id.clone(),
        |(_, external)| external.clone(),
    );
    let entity_title = lookup
        .get(&entity.id)
        .map_or_else(|| entity.title.clone(), |(title, _)| title.clone());

    let mut summaries = Vec::new();
    for observation in store.observations_for_entity(entity.id)? {
        let permalink = entity.permalink.as_ref().map_or_else(String::new, |base| {
            crate::domain::permalink::generate_permalink(&format!(
                "{base}/observations/{}/{}",
                observation.category,
                observation_permalink_suffix(&observation.content)
            ))
        });
        summaries.push(ObservationSummary {
            item_type: "observation",
            observation_id: observation.id,
            entity_id: entity.id,
            entity_external_id: entity_external_id.clone(),
            title: Some(entity_title.clone()),
            file_path: entity.file_path.clone(),
            permalink,
            category: observation.category.clone(),
            content: observation.content.clone(),
            created_at: created_at.clone(),
        });
    }
    Ok(summaries)
}

/// Shape one traversal row into an entity summary (no body: reference hydration
/// only carries `content` for the primary search row).
pub(crate) fn entity_summary(
    row: &RelatedRow,
    lookup: &HashMap<i64, (String, String)>,
) -> EntitySummary {
    EntitySummary {
        item_type: "entity",
        external_id: lookup
            .get(&row.id)
            .map(|(_, external)| external.clone())
            .unwrap_or_default(),
        entity_id: row.id,
        permalink: Some(row.permalink.clone()),
        title: row.title.clone(),
        content: None,
        file_path: row.file_path.clone(),
        created_at: row.created_at.clone(),
    }
}

/// Shape one traversal row into a relation summary.
pub(crate) fn relation_summary(
    row: &RelatedRow,
    lookup: &HashMap<i64, (String, String)>,
) -> RelationSummary {
    let from = row.from_id.and_then(|id| lookup.get(&id));
    let to = row.to_id.and_then(|id| lookup.get(&id));
    RelationSummary {
        item_type: "relation",
        relation_id: row.id,
        entity_id: None,
        title: row.title.clone(),
        file_path: row.file_path.clone(),
        permalink: row.permalink.clone(),
        relation_type: row.relation_type.clone().unwrap_or_default(),
        from_entity: from.map(|(title, _)| title.clone()),
        from_entity_id: row.from_id,
        from_entity_external_id: from.map(|(_, external)| external.clone()),
        to_entity: to.map(|(title, _)| title.clone()),
        to_name: row.to_name.clone(),
        to_entity_id: row.to_id,
        to_entity_external_id: to.map(|(_, external)| external.clone()),
        created_at: row.created_at.clone(),
    }
}

fn invalid_arg(message: String) -> Error {
    Error::InvalidArgument { message }
}

/// Render a context response as the reference CLI's undecorated outline.
///
/// Mirrors `cli/commands/tool.py::_plain_build_context`, including the two-space
/// indentation, the 120-character observation truncation, and the
/// `relation_type  type  title` related-item line. `response` is the serialized
/// `build_context` payload, which is also what the reference renderer receives.
pub fn render_plain(response: &Value) -> String {
    let value = response;
    let uri = value["metadata"]["uri"].as_str().unwrap_or_default();
    let results = value["results"].as_array().cloned().unwrap_or_default();

    let mut lines = Vec::new();
    lines.push(if uri.is_empty() {
        "Context".to_owned()
    } else {
        format!("Context: {uri}")
    });
    if results.is_empty() {
        lines.push("No related content found.".to_owned());
        return lines.join("\n");
    }

    for result in &results {
        let primary = &result["primary_result"];
        let title = primary["title"]
            .as_str()
            .filter(|title| !title.is_empty())
            .or_else(|| primary["permalink"].as_str())
            .unwrap_or_default();
        let item_type = primary["type"].as_str().unwrap_or_default();
        lines.push(if item_type.is_empty() {
            title.to_owned()
        } else {
            format!("{item_type}  {title}")
        });

        if let Some(content) = primary["content"].as_str() {
            let content = content.trim();
            if !content.is_empty() {
                for line in content.lines() {
                    lines.push(format!("  {line}"));
                }
            }
        }

        for observation in result["observations"].as_array().into_iter().flatten() {
            let category = observation["category"].as_str().unwrap_or_default();
            let content = observation["content"].as_str().unwrap_or_default();
            lines.push(format!("  [{category}] {}", truncate_chars(content, 120)));
        }

        for related in result["related_results"].as_array().into_iter().flatten() {
            let title = related["title"]
                .as_str()
                .filter(|title| !title.is_empty())
                .or_else(|| related["permalink"].as_str())
                .unwrap_or_default();
            let item_type = related["type"].as_str().unwrap_or_default();
            let relation = related["relation_type"].as_str().unwrap_or_default();
            let parts: Vec<&str> = [relation, item_type, title]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect();
            lines.push(format!("  {}", parts.join("  ")));
        }
    }

    lines.join("\n")
}

/// Render a context response as the MCP `output_format="text"` markdown.
///
/// Mirrors `mcp/tools/build_context.py::_format_context_markdown`.
pub fn render_markdown(value: &Value, project: &str) -> String {
    let metadata = &value["metadata"];
    let uri = metadata["uri"].as_str().unwrap_or_default();
    let results = value["results"].as_array().cloned().unwrap_or_default();
    if results.is_empty() {
        return format!("No results found for '{uri}' in project '{project}'.");
    }

    let mut parts = Vec::new();
    let first_title = results[0]["primary_result"]["title"]
        .as_str()
        .unwrap_or_default();
    if results.len() == 1 {
        parts.push(format!("# Context: {first_title}"));
    } else {
        parts.push(format!("# Context: {uri}"));
    }
    parts.push(String::new());
    let blocks: Vec<String> = results.iter().map(format_entity_block).collect();
    parts.push(blocks.join("\n\n---\n\n"));
    parts.push(String::new());
    parts.push("---".to_owned());
    parts.push(format!(
        "*{} primary, {} related | depth={} | project: {}*",
        metadata["primary_count"].as_u64().unwrap_or_default(),
        metadata["related_count"].as_u64().unwrap_or_default(),
        metadata["depth"].as_u64().unwrap_or_default(),
        project
    ));
    parts.join("\n")
}

/// Format one context result as a markdown block (reference `_format_entity_block`).
fn format_entity_block(result: &Value) -> String {
    let primary = &result["primary_result"];
    let mut lines = Vec::new();

    lines.push(format!(
        "## {}",
        primary["title"].as_str().unwrap_or_default()
    ));
    if let Some(permalink) = primary["permalink"]
        .as_str()
        .filter(|permalink| !permalink.is_empty())
    {
        lines.push(format!("permalink: {permalink}"));
    }
    if let Some(content) = primary["content"]
        .as_str()
        .filter(|content| !content.is_empty())
    {
        lines.push(String::new());
        lines.push(content.to_owned());
    }

    let observations = result["observations"].as_array().into_iter().flatten();
    let observations: Vec<&Value> = observations.collect();
    if !observations.is_empty() {
        lines.push(String::new());
        lines.push("### Observations".to_owned());
        for observation in observations {
            lines.push(format!(
                "- [{}] {}",
                observation["category"].as_str().unwrap_or_default(),
                observation["content"].as_str().unwrap_or_default()
            ));
        }
    }

    let related: Vec<&Value> = result["related_results"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let relations: Vec<&&Value> = related
        .iter()
        .filter(|item| item["type"].as_str() == Some("relation"))
        .collect();
    if !relations.is_empty() {
        lines.push(String::new());
        lines.push("### Relations".to_owned());
        for relation in relations {
            // Unresolved forward references fall back to the literal target text.
            let target = relation["to_entity"]
                .as_str()
                .or_else(|| relation["to_name"].as_str())
                .unwrap_or_default();
            lines.push(format!(
                "- {} [[{target}]]",
                relation["relation_type"].as_str().unwrap_or_default()
            ));
        }
    }

    let entities: Vec<&&Value> = related
        .iter()
        .filter(|item| item["type"].as_str() != Some("relation"))
        .collect();
    if !entities.is_empty() {
        lines.push(String::new());
        lines.push("### Related".to_owned());
        for entity in entities {
            lines.push(format!(
                "- [[{}]] ({})",
                entity["title"].as_str().unwrap_or_default(),
                entity["permalink"].as_str().unwrap_or_default()
            ));
        }
    }

    lines.join("\n")
}

/// Reference `_plain_build_context` truncates observation content to 120 chars.
fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut truncated: String = text.chars().take(limit - 3).collect();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexing::{RebuildOptions, rebuild_vault};
    use std::path::PathBuf;

    fn vault() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault")
    }

    fn store() -> (Store, i64) {
        let mut store = Store::open_in_memory().expect("store");
        let vault = vault();
        let project_id = store
            .upsert_project("oracle", "oracle", &vault.to_string_lossy())
            .expect("project");
        rebuild_vault(
            &mut store,
            project_id,
            &vault,
            &RebuildOptions::new("oracle"),
        )
        .expect("rebuild");
        (store, project_id)
    }

    #[test]
    fn options_are_validated_like_the_reference_tool() {
        let base = ContextOptions::default();
        assert!(base.validate().is_ok());
        assert!(
            ContextOptions {
                page_size: 0,
                ..base
            }
            .validate()
            .is_err()
        );
        assert!(
            ContextOptions {
                page_size: MAX_PAGE_SIZE + 1,
                ..base
            }
            .validate()
            .is_err()
        );
        assert!(
            ContextOptions {
                max_related: MAX_RELATED_RESULTS + 1,
                ..base
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn observation_summaries_use_the_entity_title_and_synthetic_permalink() {
        let (store, project_id) = store();
        let context = build_context(
            &store,
            project_id,
            "memory://notes/simple",
            &ContextOptions::default(),
        )
        .expect("context");
        let observation = &context.results[0].observations[0];
        assert_eq!(observation.title.as_deref(), Some("simple"));
        assert_eq!(
            observation.permalink,
            "oracle/notes/simple/observations/note/created-as-a-baseline-fixture"
        );
    }

    #[test]
    fn a_timeframe_can_exclude_fixture_notes() {
        let (store, project_id) = store();
        // `notes/frontmatter.md` declares `created: 2026-01-02`, so a 7-day window
        // drops it from the related entities of `notes/relations`.
        let since = timeframe::parse_timeframe("7d").expect("since");
        let context = build_context(
            &store,
            project_id,
            "memory://notes/relations",
            &ContextOptions {
                since: Some(since),
                ..ContextOptions::default()
            },
        )
        .expect("context");
        let permalinks: Vec<&str> = context.results[0]
            .related_results
            .iter()
            .filter(|item| item["type"] == "entity")
            .filter_map(|item| item["permalink"].as_str())
            .collect();
        assert!(
            !permalinks.contains(&"notes/frontmatter-note"),
            "frontmatter note is older than the window: {permalinks:?}"
        );
    }

    #[test]
    fn pages_past_the_first_are_empty_like_a_direct_lookup() {
        let (store, project_id) = store();
        let context = build_context(
            &store,
            project_id,
            "memory://notes/simple",
            &ContextOptions {
                page: 2,
                ..ContextOptions::default()
            },
        )
        .expect("context");
        assert!(context.results.is_empty());
        assert_eq!(context.metadata.primary_count, 0);
        assert_eq!(context.metadata.uri.as_deref(), Some("oracle/notes/simple"));
    }
}
