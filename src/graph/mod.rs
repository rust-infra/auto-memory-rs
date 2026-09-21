//! `memory://` handling, entity resolution, and bounded graph traversal.
//!
//! Every indexed Markdown file is a node ("entity"); every `[[...]]` reference in
//! its body is a directed edge ("relation") pointing at another entity. Edges are
//! stored once, in the direction they were written, and the target may be
//! unresolved (`to_id IS NULL`) until a matching file exists.
//!
//! This module owns the traversal side: resolving a `memory://` URL to a starting
//! entity and walking the edges outward. The actual SQL lives in
//! [`crate::storage::Store::find_related`]; the storage shape is in
//! `src/storage/schema.rs` and `src/storage/records.rs`.
//!
//! New to the graph? Read `docs/knowledge-graph.md` first — it explains the
//! concepts with real fixture examples. `docs/context-spec.md` is the precise
//! traversal contract.

use std::collections::{HashSet, VecDeque};

use crate::error::{Error, Result};

/// One relation edge with both endpoints resolved where possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationEdge {
    /// Relation row id.
    pub id: i64,
    /// Source entity id.
    pub from_id: i64,
    /// Target entity id, when resolved.
    pub to_id: Option<i64>,
    /// Raw target text.
    pub to_name: String,
    /// Relation label.
    pub relation_type: String,
    /// Optional `(context)`.
    pub context: Option<String>,
    /// Source entity title.
    pub from_title: String,
    /// Source entity permalink.
    pub from_permalink: Option<String>,
    /// Source entity file path.
    pub from_file_path: String,
    /// Source entity external id.
    pub from_external_id: String,
    /// Target entity title.
    pub to_title: Option<String>,
    /// Target entity permalink.
    pub to_permalink: Option<String>,
    /// Target entity external id.
    pub to_external_id: Option<String>,
}

/// Normalize a `memory://` URL, mirroring `schemas/memory.py`.
pub fn normalize_memory_url(url: &str) -> Result<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(Error::Frontmatter {
            message: "Memory URL cannot be empty".to_owned(),
        });
    }
    let path = trimmed.strip_prefix("memory://").unwrap_or(trimmed);
    if path.is_empty() || path.trim().is_empty() {
        return Err(Error::Frontmatter {
            message: "Memory URL cannot be empty".to_owned(),
        });
    }
    if path.contains("://") {
        return Err(Error::Frontmatter {
            message: format!("Invalid memory URL path: '{path}' contains protocol scheme"),
        });
    }
    if path.contains("//") {
        return Err(Error::Frontmatter {
            message: format!("Invalid memory URL path: '{path}' contains double slashes"),
        });
    }
    if path
        .chars()
        .any(|c| matches!(c, '<' | '>' | '"' | '|' | '?'))
    {
        return Err(Error::Frontmatter {
            message: format!("Invalid memory URL path: '{path}' contains invalid characters"),
        });
    }
    if path.len() > 2028 {
        return Err(Error::Frontmatter {
            message: "Memory URL is too long".to_owned(),
        });
    }
    Ok(format!("memory://{path}"))
}

/// Strip the `memory://` prefix from a normalized URL.
pub fn memory_url_path(url: &str) -> &str {
    url.strip_prefix("memory://").unwrap_or(url)
}

/// One traversed relation with the depth at which it was discovered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversedRelation {
    /// The relation edge.
    pub edge: RelationEdge,
    /// Logical hop depth (1 = directly connected to the primary entity).
    pub depth: usize,
}

/// Breadth-first traversal over relation edges.
///
/// Each logical hop walks relation → entity, so the reference doubles the
/// requested depth; callers pass `max_depth = depth * 2`. Results are deduplicated
/// by relation id, cycles terminate because entities are visited once, and the
/// result set is capped by `max_results`.
pub fn traverse_relations(
    edges: &[RelationEdge],
    start_entity_ids: &[i64],
    max_depth: usize,
    max_results: usize,
) -> Vec<TraversedRelation> {
    let mut visited_entities: HashSet<i64> = start_entity_ids.iter().copied().collect();
    let mut visited_relations: HashSet<i64> = HashSet::new();
    let mut results = Vec::new();
    let mut frontier: VecDeque<(i64, usize)> = start_entity_ids.iter().map(|id| (*id, 0)).collect();

    while let Some((entity_id, depth)) = frontier.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for edge in edges {
            let connected = edge.from_id == entity_id || edge.to_id == Some(entity_id);
            if !connected || !visited_relations.insert(edge.id) {
                continue;
            }
            if results.len() >= max_results {
                return results;
            }
            results.push(TraversedRelation {
                edge: edge.clone(),
                depth: depth + 1,
            });
            let next = if edge.from_id == entity_id {
                edge.to_id
            } else {
                Some(edge.from_id)
            };
            if let Some(next) = next {
                if visited_entities.insert(next) {
                    frontier.push_back((next, depth + 1));
                }
            }
        }
    }
    results
}

/// Resolve a normalized memory URL path to an entity id.
///
/// Matches, in order: exact permalink, file path, file path without `.md`, and a
/// project-prefixed permalink followed by the bare path.
pub fn resolve_entity_path(
    store: &crate::storage::Store,
    project_id: i64,
    path: &str,
) -> Result<Option<crate::storage::EntityRow>> {
    if let Some(entity) = store.entity_by_permalink(project_id, path)? {
        return Ok(Some(entity));
    }
    if let Some(entity) = store.entity_by_file_path(project_id, path)? {
        return Ok(Some(entity));
    }
    let with_md = format!("{path}.md");
    if let Some(entity) = store.entity_by_file_path(project_id, &with_md)? {
        return Ok(Some(entity));
    }
    for entity in store.entities(project_id)? {
        let matches = entity
            .permalink
            .as_deref()
            .is_some_and(|permalink| permalink == path || permalink.ends_with(&format!("/{path}")));
        if matches {
            return Ok(Some(entity));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_bare_and_prefixed_urls() {
        assert_eq!(
            normalize_memory_url("specs/search").as_deref().ok(),
            Some("memory://specs/search")
        );
        assert_eq!(
            normalize_memory_url("memory://specs/search")
                .as_deref()
                .ok(),
            Some("memory://specs/search")
        );
    }

    #[test]
    fn rejects_malformed_urls() {
        assert!(normalize_memory_url("").is_err());
        assert!(normalize_memory_url("memory//test").is_err());
        assert!(normalize_memory_url("invalid://test").is_err());
        assert!(normalize_memory_url("bad?char").is_err());
    }

    fn edge(id: i64, from: i64, to: Option<i64>) -> RelationEdge {
        RelationEdge {
            id,
            from_id: from,
            to_id: to,
            to_name: format!("p/{to:?}"),
            relation_type: "links_to".to_owned(),
            context: None,
            from_title: format!("E{from}"),
            from_permalink: Some(format!("p/e{from}")),
            from_file_path: format!("e{from}.md"),
            from_external_id: format!("x{from}"),
            to_title: to.map(|id| format!("E{id}")),
            to_permalink: to.map(|id| format!("p/e{id}")),
            to_external_id: to.map(|id| format!("x{id}")),
        }
    }

    #[test]
    fn traversal_is_bounded_and_cycle_safe() {
        let edges = vec![
            edge(1, 1, Some(2)),
            edge(2, 2, Some(1)),
            edge(3, 2, Some(3)),
        ];
        let direct = traverse_relations(&edges, &[1], 1, 100);
        assert_eq!(direct.len(), 2, "cycle edge reached through entity 2");
        let limited = traverse_relations(&edges, &[1], 10, 1);
        assert_eq!(limited.len(), 1, "max_results caps traversal");
        let visited: HashSet<i64> = direct.iter().map(|r| r.edge.id).collect();
        assert_eq!(visited.len(), direct.len(), "relations are deduplicated");
    }
}
