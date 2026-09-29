//! Build FTS5 `search_index` rows from parsed documents.
//!
//! Mirrors `SearchService.index_entity_markdown` in Basic Memory 0.23.2:
//! entity rows carry title/permalink/file-path variants plus the note body and tags;
//! observation rows carry the observation text; relation rows carry the relation title.
//! `content_stems` is a legacy name — the reference concatenates text variants and
//! does not stem, so the FTS5 `unicode61` tokenizer does the rest.

use std::collections::BTreeSet;

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::domain::observation::Observation;
use crate::domain::permalink::generate_permalink;

/// Maximum size of the reference `content_stems` value.
pub const MAX_CONTENT_STEMS_SIZE: usize = 6000;

/// One row to insert into the FTS5 `search_index` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchIndexWriteRow {
    /// Row id (entity, observation, or relation id).
    pub id: i64,
    /// Item type: `entity`, `observation`, or `relation`.
    pub item_type: String,
    /// Display title.
    pub title: String,
    /// Searchable text (variant concatenation).
    pub content_stems: String,
    /// Display content.
    pub content_snippet: Option<String>,
    /// Row permalink.
    pub permalink: Option<String>,
    /// Project-relative file path.
    pub file_path: String,
    /// Source entity (relation rows).
    pub from_id: Option<i64>,
    /// Target entity (relation rows).
    pub to_id: Option<i64>,
    /// Relation label (relation rows).
    pub relation_type: Option<String>,
    /// Owning entity id.
    pub entity_id: i64,
    /// Observation category (observation rows).
    pub category: Option<String>,
    /// JSON metadata.
    pub metadata: String,
}

/// Reference `_generate_variants`: original, lowercase, path segments, words.
pub fn text_variants(text: &str) -> Vec<String> {
    let mut variants = BTreeSet::new();
    variants.insert(text.to_owned());
    variants.insert(text.to_lowercase());
    if text.contains('/') {
        for part in text.split('/') {
            let part = part.trim();
            if !part.is_empty() {
                variants.insert(part.to_owned());
            }
        }
    }
    for word in text.to_lowercase().split_whitespace() {
        let word = word.trim();
        if !word.is_empty() {
            variants.insert(word.to_owned());
        }
    }
    variants.into_iter().collect()
}

fn join_variants(parts: impl IntoIterator<Item = String>) -> String {
    let mut stems = parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if stems.len() > MAX_CONTENT_STEMS_SIZE {
        stems.truncate(MAX_CONTENT_STEMS_SIZE);
    }
    stems
}

/// Build the entity-level search row.
#[allow(clippy::too_many_arguments)]
pub fn entity_row(
    entity_id: i64,
    title: &str,
    note_type: &str,
    permalink: Option<&str>,
    file_path: &str,
    content: &str,
    tags: &[String],
) -> SearchIndexWriteRow {
    let mut parts: Vec<String> = text_variants(title);
    if !content.is_empty() {
        parts.push(content.to_owned());
    }
    if let Some(permalink) = permalink {
        parts.extend(text_variants(permalink));
    }
    parts.extend(text_variants(file_path));
    parts.extend(tags.iter().cloned());

    SearchIndexWriteRow {
        id: entity_id,
        item_type: "entity".to_owned(),
        title: title.to_owned(),
        content_stems: join_variants(parts),
        content_snippet: (!content.is_empty()).then(|| content.to_owned()),
        permalink: permalink.map(str::to_owned),
        file_path: file_path.to_owned(),
        from_id: None,
        to_id: None,
        relation_type: None,
        entity_id,
        category: None,
        metadata: json!({ "note_type": note_type }).to_string(),
    }
}

/// Build one observation-level search row.
pub fn observation_row(
    observation_id: i64,
    entity_id: i64,
    entity_permalink: Option<&str>,
    file_path: &str,
    observation: &Observation,
) -> SearchIndexWriteRow {
    let category = observation
        .category
        .clone()
        .unwrap_or_else(|| "note".to_owned());
    let title = format!(
        "{category}: {}...",
        truncate_chars(&observation.content, 100)
    );
    let permalink = entity_permalink.map(|permalink| {
        generate_permalink(&format!(
            "{permalink}/observations/{category}/{}",
            observation_permalink_suffix(&observation.content)
        ))
    });
    SearchIndexWriteRow {
        id: observation_id,
        item_type: "observation".to_owned(),
        title,
        content_stems: join_variants(text_variants(&observation.content)),
        content_snippet: Some(observation.content.clone()),
        permalink,
        file_path: file_path.to_owned(),
        from_id: None,
        to_id: None,
        relation_type: None,
        entity_id,
        category: Some(category),
        metadata: json!({ "tags": observation.tags }).to_string(),
    }
}

/// Build one relation-level search row.
#[allow(clippy::too_many_arguments)]
pub fn relation_row(
    relation_id: i64,
    entity_id: i64,
    file_path: &str,
    from_title: &str,
    to_title: Option<&str>,
    permalink: Option<&str>,
    from_id: i64,
    to_id: Option<i64>,
    relation_type: &str,
) -> SearchIndexWriteRow {
    let title = match to_title {
        Some(to_title) => format!("{from_title} -> {to_title}"),
        None => from_title.to_owned(),
    };
    SearchIndexWriteRow {
        id: relation_id,
        item_type: "relation".to_owned(),
        title: title.clone(),
        content_stems: join_variants(text_variants(&title)),
        content_snippet: None,
        permalink: permalink.map(str::to_owned),
        file_path: file_path.to_owned(),
        from_id: Some(from_id),
        to_id,
        relation_type: Some(relation_type.to_owned()),
        entity_id,
        category: None,
        metadata: "{}".to_owned(),
    }
}

/// Reference observation permalink suffix: content, or `content[:200]-digest`.
pub fn observation_permalink_suffix(content: &str) -> String {
    if content.chars().count() > 200 {
        let prefix: String = content.chars().take(200).collect();
        let digest = Sha256::digest(content.as_bytes());
        let digest_hex = digest
            .iter()
            .take(6)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("{prefix}-{digest_hex}")
    } else {
        content.to_owned()
    }
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_owned()
    } else {
        text.chars().take(limit).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_include_path_segments_and_words() {
        let variants = text_variants("Notes/My Note");
        assert!(variants.contains(&"Notes/My Note".to_owned()));
        assert!(variants.contains(&"notes/my note".to_owned()));
        // Path segments keep their original casing in the reference implementation.
        assert!(variants.contains(&"Notes".to_owned()));
        assert!(variants.contains(&"My Note".to_owned()));
        // Words are lowercased.
        assert!(variants.contains(&"notes/my".to_owned()));
        assert!(variants.contains(&"note".to_owned()));
    }

    #[test]
    fn entity_row_matches_reference_shape() {
        let row = entity_row(
            7,
            "Frontmatter Demo",
            "reference",
            Some("notes/frontmatter-note"),
            "notes/frontmatter.md",
            "# Frontmatter Demo\n\nKeep Markdown.",
            &["rust".to_owned()],
        );
        assert_eq!(row.id, 7);
        assert_eq!(row.item_type, "entity");
        assert!(row.content_stems.contains("Keep Markdown."));
        assert!(row.content_stems.contains("rust"));
        assert_eq!(row.metadata, r#"{"note_type":"reference"}"#);
    }

    #[test]
    fn observation_title_truncates_at_100_chars() {
        let long = "x".repeat(150);
        let observation = Observation {
            category: Some("fact".to_owned()),
            content: long,
            tags: vec![],
            context: None,
        };
        let row = observation_row(1, 1, Some("p/a"), "a.md", &observation);
        assert_eq!(row.title, format!("fact: {}...", "x".repeat(100)));
        assert_eq!(row.category.as_deref(), Some("fact"));
    }

    #[test]
    fn relation_title_uses_arrow_when_resolved() {
        let row = relation_row(
            1,
            1,
            "a.md",
            "A",
            Some("B"),
            Some("a/rel/b"),
            1,
            Some(2),
            "rel",
        );
        assert_eq!(row.title, "A -> B");
        assert_eq!(row.relation_type.as_deref(), Some("rel"));
    }
}
