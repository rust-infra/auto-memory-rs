//! Recent activity (`recent_activity`).
//!
//! The reference serves this from the same `ContextService.build_context` it uses for
//! `memory://` URLs, called with no `memory_url`: the primary rows come from a filtered
//! search (`search_item_types` + `after_date`, `LIMIT page_size + 1`), the traversal and
//! hydration are identical, and only the metadata differs — `uri` is null and `types`
//! echoes the requested set.
//!
//! Two ordering details are load-bearing. The search is ordered by
//! `score ASC, updated_at DESC`, which with no query text means "most recently modified
//! first" — and `updated_at` is the note's *file mtime* (or frontmatter `modified`), not
//! the index time. So the result is a recency list over edit times, not over index order.

use serde::Serialize;
use serde_json::Value;

use crate::application::context::{
    EntitySummary, MemoryMetadata, ObservationSummary, RelationSummary, entity_summary,
    hydration_lookup, observation_summaries, relation_summary,
};
use crate::domain::search::SearchItemType;
use crate::domain::timeframe;
use crate::error::Result;
use crate::search::text::TextSearchOptions;
use crate::storage::{RelatedRow, Store};

/// Arguments for [`recent_context`].
#[derive(Debug, Clone)]
pub struct ActivityOptions {
    /// Item types to include; defaults to entity-only like the reference tool.
    pub types: Vec<SearchItemType>,
    /// Relation hops to traverse.
    pub depth: u32,
    /// One-indexed page.
    pub page: u32,
    /// Items per page.
    pub page_size: u32,
    /// Related items per page.
    pub max_related: u32,
    /// Timeframe bound resolved from the caller's expression.
    pub since: Option<chrono::DateTime<chrono::FixedOffset>>,
    /// Whether the caller passed an explicit `type` filter (drives the empty text output).
    pub type_filter_applied: bool,
}

impl Default for ActivityOptions {
    fn default() -> Self {
        Self {
            types: vec![SearchItemType::Entity],
            depth: 1,
            page: 1,
            page_size: 10,
            max_related: 10,
            since: None,
            type_filter_applied: false,
        }
    }
}

/// One primary activity item with its observations and related items.
#[derive(Debug, Clone, Serialize)]
pub struct ActivityResult {
    /// Primary entity, relation, or observation summary.
    pub primary_result: Value,
    /// Observations owned by the primary entity.
    pub observations: Vec<ObservationSummary>,
    /// Related summaries reached by the traversal.
    pub related_results: Vec<Value>,
}

/// Activity graph (`GraphContext` with a null `uri`).
#[derive(Debug, Clone, Serialize)]
pub struct ActivityContext {
    /// One entry per primary item.
    pub results: Vec<ActivityResult>,
    /// Metadata.
    pub metadata: MemoryMetadata,
    /// Page number.
    pub page: u32,
    /// Page size.
    pub page_size: u32,
    /// Whether more pages exist.
    pub has_more: bool,
}

/// Build the recent-activity context for one project.
pub fn recent_context(
    store: &Store,
    project_id: i64,
    options: &ActivityOptions,
) -> Result<ActivityContext> {
    let since = options
        .since
        .map(|instant| timeframe::since_bound(&instant));
    let offset = (options.page.saturating_sub(1)) as usize * options.page_size as usize;

    // The reference asks for `limit + 1` rows at `offset` so it can detect a further
    // page without a second query. Our search takes a page index rather than an
    // offset, so fetch one page that covers `offset + limit + 1` and slice.
    let fetch = (offset + options.page_size as usize + 1) as u32;
    let page = store.search_text(
        project_id,
        &TextSearchOptions {
            entity_types: options.types.clone(),
            after_date: since.clone(),
            page: 1,
            page_size: fetch,
            ..TextSearchOptions::default()
        },
    )?;
    let mut primary: Vec<_> = page.results.into_iter().skip(offset).collect();
    let has_more = primary.len() > options.page_size as usize;
    primary.truncate(options.page_size as usize);

    // `find_related` is seeded with the search row ids the reference pairs with the
    // row type; for entity rows `search_index.id` is the entity id.
    let roots: Vec<i64> = primary
        .iter()
        .filter_map(|row| match row.item_type {
            SearchItemType::Entity => row.entity_id,
            SearchItemType::Observation => row.observation_id,
            SearchItemType::Relation => row.relation_id,
        })
        .collect();
    let related = store.find_related(
        project_id,
        &roots,
        options.depth,
        options.max_related,
        since.as_deref(),
    )?;

    let primary_entity_ids: Vec<i64> = primary
        .iter()
        .filter(|row| row.item_type == SearchItemType::Entity)
        .filter_map(|row| row.entity_id)
        .collect();
    let lookup = hydration_lookup(store, &primary_entity_ids, &related)?;

    // The reference loads observations for every entity id the response mentions —
    // primary *and* related — and counts them once per distinct entity.
    let mut counted_observations: std::collections::BTreeSet<i64> =
        std::collections::BTreeSet::new();
    let mut total_observations = 0_usize;
    let mut total_relations = 0_usize;
    let mut results = Vec::with_capacity(primary.len());
    for row in &primary {
        let root_id = match row.item_type {
            SearchItemType::Entity => row.entity_id,
            SearchItemType::Observation => row.observation_id,
            SearchItemType::Relation => row.relation_id,
        };
        let related_to_primary: Vec<&RelatedRow> = related
            .iter()
            .filter(|candidate| Some(candidate.root_id) == root_id)
            .collect();

        let mut observations = Vec::new();
        if row.item_type == SearchItemType::Entity
            && let Some(entity_id) = row.entity_id
        {
            observations = observation_summaries_for(store, entity_id, &lookup)?;
            if counted_observations.insert(entity_id) {
                total_observations += observations.len();
            }
        }

        let mut related_results = Vec::with_capacity(related_to_primary.len());
        for candidate in &related_to_primary {
            if candidate.item_type == "relation" {
                total_relations += 1;
                related_results.push(serde_json::to_value(relation_summary(candidate, &lookup))?);
            } else {
                if counted_observations.insert(candidate.id) {
                    total_observations += store.observations_for_entity(candidate.id)?.len();
                }
                related_results.push(serde_json::to_value(entity_summary(candidate, &lookup))?);
            }
        }

        results.push(ActivityResult {
            primary_result: primary_summary(store, row)?,
            observations,
            related_results,
        });
    }

    let related_count = results
        .iter()
        .map(|result| result.related_results.len())
        .sum();
    let metadata = MemoryMetadata {
        uri: None,
        types: Some(
            options
                .types
                .iter()
                .map(|kind| <&str>::from(*kind).to_owned())
                .collect(),
        ),
        depth: options.depth,
        timeframe: since,
        generated_at: timeframe::utc_now_iso(),
        primary_count: results.len(),
        related_count,
        total_results: results.len() + related_count,
        total_relations,
        total_observations,
    };

    Ok(ActivityContext {
        results,
        metadata,
        page: options.page,
        page_size: options.page_size,
        has_more,
    })
}

/// Shape one primary search row into its `ContextResultRow` payload.
fn primary_summary(store: &Store, row: &crate::domain::search::SearchResult) -> Result<Value> {
    let created_at = row
        .entity_id
        .and_then(|id| store.entity_created_at(id).ok().flatten())
        .unwrap_or_default();
    let value = match row.item_type {
        SearchItemType::Entity => serde_json::to_value(EntitySummary {
            item_type: "entity",
            external_id: row.external_id.clone().unwrap_or_default(),
            entity_id: row.entity_id.unwrap_or_default(),
            permalink: row.permalink.clone(),
            title: row.title.clone(),
            content: row.content.clone(),
            file_path: row.file_path.clone(),
            created_at,
        })?,
        SearchItemType::Observation => serde_json::to_value(ObservationSummary {
            item_type: "observation",
            observation_id: row.observation_id.unwrap_or_default(),
            entity_id: row.entity_id.unwrap_or_default(),
            entity_external_id: String::new(),
            title: Some(row.title.clone()),
            file_path: row.file_path.clone(),
            permalink: row.permalink.clone().unwrap_or_default(),
            category: row.category.clone().unwrap_or_default(),
            content: row.content.clone().unwrap_or_default(),
            created_at,
        })?,
        SearchItemType::Relation => serde_json::to_value(RelationSummary {
            item_type: "relation",
            relation_id: row.relation_id.unwrap_or_default(),
            entity_id: row.entity_id,
            title: row.title.clone(),
            file_path: row.file_path.clone(),
            permalink: row.permalink.clone().unwrap_or_default(),
            relation_type: row.relation_type.clone().unwrap_or_default(),
            from_entity: row.from_entity.clone(),
            from_entity_id: None,
            from_entity_external_id: None,
            to_entity: row.to_entity.clone(),
            to_name: None,
            to_entity_id: None,
            to_entity_external_id: None,
            created_at,
        })?,
    };
    Ok(value)
}

/// Observation summaries for one entity id, or an empty list when the row is gone.
fn observation_summaries_for(
    store: &Store,
    entity_id: i64,
    lookup: &std::collections::HashMap<i64, (String, String)>,
) -> Result<Vec<ObservationSummary>> {
    match store.entity_by_id(entity_id)? {
        Some(entity) => observation_summaries(store, &entity, lookup),
        None => Ok(Vec::new()),
    }
}

/// Flatten an activity graph into the reference's JSON rows (`_extract_recent_rows`).
pub fn recent_rows(activity: &ActivityContext) -> Vec<Value> {
    activity
        .results
        .iter()
        .map(|result| {
            let primary = &result.primary_result;
            serde_json::json!({
                "type": primary["type"],
                "title": primary["title"],
                "permalink": primary["permalink"],
                "file_path": primary["file_path"],
                "created_at": primary["created_at"],
            })
        })
        .collect()
}

/// Render the reference's project-scoped activity text (`_format_project_output`).
pub fn render_activity_text(
    project_name: &str,
    activity: &ActivityContext,
    timeframe: &str,
    project_id: Option<&str>,
    type_filter_applied: bool,
) -> String {
    let mut lines = vec![format!("## Recent Activity: {project_name} ({timeframe})")];

    if activity.results.is_empty() {
        if activity.page > 1 {
            lines.push(format!(
                "\nNo recent activity was found on page {} for '{project_name}' within {timeframe}.",
                activity.page
            ));
            lines.push(format!(
                "Try page={} or return to page=1.",
                activity.page - 1
            ));
            return lines.join("\n");
        }
        if type_filter_applied {
            lines.push(format!(
                "\nNo recent activity matched the requested type filter in '{project_name}' within {timeframe}."
            ));
            lines.push(
                "Try another type or omit `type` to see recent notes and documents.".to_owned(),
            );
            return lines.join("\n");
        }
        lines.push(format!(
            "\nNo recent activity in '{project_name}' within {timeframe}."
        ));
        let route = project_id.map_or_else(
            || format!("project=\"{project_name}\""),
            |id| format!("project_id=\"{id}\""),
        );
        lines.push(String::new());
        lines.push(
            "If the user is just getting started and has no notes yet, briefly explain that \
             Basic Memory keeps notes that persist across conversations and are shared between \
             the user and their AI, then offer to save something useful from this conversation \
             as their first note — wait for them to agree before writing:"
                .to_owned(),
        );
        lines.push("```".to_owned());
        lines.push(format!(
            "write_note({route}, title=\"...\", content=\"...\", directory=\"notes\")"
        ));
        lines.push("```".to_owned());
        lines.push(format!(
            "Otherwise, widen the window with recent_activity({route}, timeframe=\"30d\") or \
             find a topic with search_notes({route}, query=\"...\")."
        ));
        return lines.join("\n");
    }

    let by_type = |wanted: &str| {
        activity
            .results
            .iter()
            .filter(|result| result.primary_result["type"] == wanted)
            .collect::<Vec<_>>()
    };
    let entities = by_type("entity");
    let relations = by_type("relation");
    let observations = by_type("observation");

    if !entities.is_empty() {
        lines.push(format!(
            "\n**📄 Recent Notes & Documents ({}):**",
            entities.len()
        ));
        for entity in &entities {
            let primary = &entity.primary_result;
            let title = primary["title"]
                .as_str()
                .filter(|t| !t.is_empty())
                .unwrap_or("Untitled");
            let folder = primary["file_path"]
                .as_str()
                .and_then(|path| path.rsplit_once('/').map(|(dir, _)| dir.to_owned()))
                .filter(|dir| !dir.is_empty())
                .map_or_else(String::new, |dir| format!(" ({dir})"));
            let external_id = primary["external_id"].as_str().unwrap_or_default();
            lines.push(format!("  • {title}{folder} [id: {external_id}]"));
        }
    }

    if !observations.is_empty() {
        lines.push(format!(
            "\n**🔍 Recent Observations ({}):**",
            observations.len()
        ));
        let mut order: Vec<String> = Vec::new();
        let mut grouped: std::collections::HashMap<String, Vec<&Value>> =
            std::collections::HashMap::new();
        for observation in observations.iter().take(10) {
            let category = observation.primary_result["category"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if !grouped.contains_key(&category) {
                order.push(category.clone());
            }
            grouped
                .entry(category)
                .or_default()
                .push(&observation.primary_result);
        }
        for category in order.iter().take(5) {
            let entries = &grouped[category];
            lines.push(format!("  **{category}:** {} items", entries.len()));
            for entry in entries.iter().take(2) {
                let content = entry["content"].as_str().unwrap_or_default();
                lines.push(format!("    - {}", truncate_at_word(content, 80)));
            }
        }
    }

    if !relations.is_empty() {
        lines.push(format!(
            "\n**🔗 Recent Connections ({}):**",
            relations.len()
        ));
        for relation in &relations {
            let primary = &relation.primary_result;
            let relation_type = primary["relation_type"].as_str().unwrap_or_default();
            let from = primary["from_entity"].as_str().unwrap_or("Unknown");
            let to = primary["to_entity"].as_str();
            let from_link = if from == "Unknown" {
                from.to_owned()
            } else {
                format!("[[{from}]]")
            };
            let to_link = to.map_or_else(|| "[Missing Link]".to_owned(), |to| format!("[[{to}]]"));
            lines.push(format!("  • {from_link} → {relation_type} → {to_link}"));
        }
    }

    let total = activity.results.len();
    if activity.has_more {
        lines.push(format!(
            "\n**Activity Summary:** Showing {total} items (page {}). Use page={} to see more.",
            activity.page,
            activity.page + 1
        ));
    } else {
        lines.push(format!("\n**Activity Summary:** {total} items found."));
    }

    lines.join("\n")
}

/// Port of `_truncate_at_word`.
fn truncate_at_word(text: &str, max_length: usize) -> String {
    if text.chars().count() <= max_length {
        return text.to_owned();
    }
    let truncated: String = text.chars().take(max_length).collect();
    match truncated.rfind(' ') {
        Some(index) if index > max_length * 7 / 10 => format!("{}...", &truncated[..index]),
        _ => format!(
            "{}...",
            text.chars().take(max_length - 3).collect::<String>()
        ),
    }
}
