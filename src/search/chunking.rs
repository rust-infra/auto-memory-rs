//! Semantic chunking — a port of `basic_memory.repository.semantic_chunking`.
//!
//! Markdown headings and the size budget are the boundaries (upstream 0.24.0 #1335); an
//! oversized section breaks at line boundaries, and a single line over the budget falls
//! back to an overlapping character window (900 chars, 120 overlap).
//!
//! A chunk key is content-addressed — `type:source_hash:index` — so rewriting a note does
//! not change the key of text it did not touch, and the stored vector is reused (upstream
//! 0.24.0 #1691). The row a chunk belongs to is carried separately as `row_key`, because a
//! content key cannot encode a row id that every rewrite replaces. `source_hash` remains
//! the SHA-256 of the chunk text.

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Maximum characters per chunk (reference: `MAX_VECTOR_CHUNK_CHARS`).
pub const MAX_VECTOR_CHUNK_CHARS: usize = 900;
/// Overlap used when a single paragraph exceeds the chunk size.
pub const VECTOR_CHUNK_OVERLAP_CHARS: usize = 120;

/// One indexed search row used as chunking input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticRow {
    /// Row id (entity, observation, or relation id).
    pub id: i64,
    /// `entity`, `observation`, or `relation`.
    pub item_type: String,
    /// Display title.
    pub title: Option<String>,
    /// Row permalink.
    pub permalink: Option<String>,
    /// Display content.
    pub content_snippet: Option<String>,
    /// Observation category.
    pub category: Option<String>,
    /// Relation label.
    pub relation_type: Option<String>,
    /// Owning entity id.
    pub entity_id: Option<i64>,
}

/// One deterministic chunk record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChunkRecord {
    /// `type:source_hash:index` — content-addressed, so unchanged text keeps its vector.
    pub chunk_key: String,
    /// `type:id` of the search row the chunk was cut from.
    ///
    /// Stored beside the vector rather than derived from [`Self::chunk_key`]: the key is
    /// content-addressed and cannot name a row id that a rewrite replaces.
    pub row_key: String,
    /// Chunk text that gets embedded.
    pub chunk_text: String,
    /// SHA-256 of the chunk text.
    pub source_hash: String,
}

/// Build the text embedded for one search row (reference `compose_row_source_text`).
pub fn compose_row_source_text(row: &SemanticRow) -> String {
    let parts: Vec<&str> = if row.item_type == "entity" {
        vec![
            row.title.as_deref().unwrap_or_default(),
            row.permalink.as_deref().unwrap_or_default(),
            row.content_snippet.as_deref().unwrap_or_default(),
        ]
    } else if row.item_type == "observation" {
        vec![
            row.title.as_deref().unwrap_or_default(),
            row.permalink.as_deref().unwrap_or_default(),
            row.category.as_deref().unwrap_or_default(),
            row.content_snippet.as_deref().unwrap_or_default(),
        ]
    } else {
        vec![
            row.title.as_deref().unwrap_or_default(),
            row.permalink.as_deref().unwrap_or_default(),
            row.relation_type.as_deref().unwrap_or_default(),
            row.content_snippet.as_deref().unwrap_or_default(),
        ]
    };
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Split source text at markdown-aware boundaries.
pub fn split_text_into_chunks(text: &str) -> Vec<String> {
    let normalized = text.trim();
    if normalized.is_empty() {
        return Vec::new();
    }
    let normalized = normalized.replace("\r\n", "\n");
    // A bullet is a fact, and per-fact vectors come from the observation rows; making each
    // bullet its own body chunk reproduced every fact as a near-duplicate entity vector.
    let mut sections: Vec<String> = Vec::new();
    let mut current_section: Vec<&str> = Vec::new();
    for line in normalized.split('\n') {
        if is_header_line(line) && !current_section.is_empty() {
            sections.push(current_section.join("\n").trim().to_owned());
            current_section = vec![line];
        } else {
            current_section.push(line);
        }
    }
    if !current_section.is_empty() {
        sections.push(current_section.join("\n").trim().to_owned());
    }

    let mut chunked_sections: Vec<String> = Vec::new();
    let mut current_chunk = String::new();
    for section in sections {
        // A heading starts a new chunk: packing purely by size would merge two short
        // sections into one vector and lose the structure the split is for.
        if is_header_line(section.lines().next().unwrap_or_default()) && !current_chunk.is_empty() {
            chunked_sections.push(std::mem::take(&mut current_chunk));
        }
        if char_len(&section) > MAX_VECTOR_CHUNK_CHARS {
            if !current_chunk.is_empty() {
                chunked_sections.push(std::mem::take(&mut current_chunk));
            }
            let mut long_chunks = split_long_section(&section);
            if !long_chunks.is_empty() {
                let last = long_chunks.pop().unwrap_or_default();
                chunked_sections.extend(long_chunks);
                current_chunk = last;
            }
            continue;
        }
        let candidate = if current_chunk.is_empty() {
            section.clone()
        } else {
            format!("{current_chunk}\n\n{section}")
        };
        if char_len(&candidate) <= MAX_VECTOR_CHUNK_CHARS {
            current_chunk = candidate;
        } else {
            chunked_sections.push(std::mem::take(&mut current_chunk));
            current_chunk = section;
        }
    }
    if !current_chunk.is_empty() {
        chunked_sections.push(current_chunk);
    }
    chunked_sections
        .into_iter()
        .filter(|chunk| !chunk.trim().is_empty())
        .collect()
}

/// Pack an oversized section into chunks, breaking at line boundaries.
///
/// A section over the budget used to be re-split into one chunk per bullet, which is the
/// rule #1335 removed. Lines are the boundary now, and a single line longer than the
/// budget falls back to a character window.
fn split_long_section(section: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in section.split('\n') {
        if char_len(line) > MAX_VECTOR_CHUNK_CHARS {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            chunks.extend(split_by_char_window(line));
            continue;
        }
        let candidate = if current.is_empty() {
            line.to_owned()
        } else {
            format!("{current}\n{line}")
        };
        if char_len(&candidate) <= MAX_VECTOR_CHUNK_CHARS {
            current = candidate;
        } else {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            current = line.to_owned();
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    // Trailing blank lines belong to the section separator, not to the chunk: the text is
    // what gets embedded and hashed, so it is trimmed at the boundary.
    chunks
        .into_iter()
        .map(|chunk| chunk.trim().to_owned())
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

fn split_by_char_window(paragraph: &str) -> Vec<String> {
    let text: Vec<char> = paragraph.trim().chars().collect();
    if text.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut start = 0usize;
    loop {
        let end = (start + MAX_VECTOR_CHUNK_CHARS).min(text.len());
        let chunk: String = text[start..end].iter().collect();
        let chunk = chunk.trim().to_owned();
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        if end >= text.len() {
            break;
        }
        start = end.saturating_sub(VECTOR_CHUNK_OVERLAP_CHARS);
    }
    chunks
}

/// Build deterministic chunk records for a set of search rows.
pub fn build_chunk_records(rows: &[SemanticRow]) -> Vec<ChunkRecord> {
    let mut records: Vec<ChunkRecord> = Vec::new();
    for row in rows {
        let source_text = compose_row_source_text(row);
        let row_key = format!("{}:{}", row.item_type, row.id);
        for (chunk_index, chunk_text) in
            split_text_into_chunks(&source_text).into_iter().enumerate()
        {
            // Upstream 0.24.0 (#1691): the key names the content, not the row. A rewrite
            // recreates the search rows with new ids, so an id-keyed chunk never matched
            // its predecessor and every edit re-embedded the whole note.
            let source_hash = sha256_hex(&chunk_text);
            records.push(ChunkRecord {
                chunk_key: format!("{}:{}:{}", row.item_type, source_hash, chunk_index),
                row_key: row_key.clone(),
                chunk_text,
                source_hash,
            });
        }
    }
    records
}

/// Reference entity fingerprint: SHA-256 over the sorted `(chunk_key, source_hash)` pairs.
pub fn entity_fingerprint(records: &[ChunkRecord]) -> String {
    let mut sorted: Vec<&ChunkRecord> = records.iter().collect();
    // Content-addressed keys can repeat across rows, so the row key breaks the tie: a
    // fingerprint has to be stable for the same set of chunks in any order.
    sorted.sort_by(|left, right| {
        left.chunk_key
            .cmp(&right.chunk_key)
            .then_with(|| left.row_key.cmp(&right.row_key))
            .then_with(|| left.source_hash.cmp(&right.source_hash))
    });
    let payload = sorted
        .iter()
        .map(|record| {
            format!(
                "{{\"chunk_key\":{},\"source_hash\":{}}}",
                json_string(&record.chunk_key),
                json_string(&record.source_hash)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    sha256_hex(&format!("[{payload}]"))
}

fn json_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn is_header_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && trimmed.chars().nth(hashes).is_some_and(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64) -> SemanticRow {
        SemanticRow {
            id,
            item_type: "entity".to_owned(),
            title: Some("Title".to_owned()),
            permalink: Some("p/title".to_owned()),
            content_snippet: Some("Body".to_owned()),
            category: None,
            relation_type: None,
            entity_id: Some(id),
        }
    }

    #[test]
    fn bullets_are_not_chunk_boundaries() {
        // Upstream 0.24.0 (#1335). One bullet per chunk reproduced every fact as a
        // near-duplicate entity vector; the per-fact vectors come from the observation
        // rows, so the body packs its bullets into the budget instead.
        let text = "# Title\n\n- bullet one\n- bullet two\n- bullet three";
        let chunks = split_text_into_chunks(text);
        assert_eq!(chunks.len(), 1, "{chunks:?}");
        assert!(chunks[0].contains("bullet one"), "{chunks:?}");
        assert!(chunks[0].contains("bullet three"), "{chunks:?}");
    }

    #[test]
    fn a_heading_starts_a_new_chunk() {
        let text = "# Alpha\n\ntext a\n\n# Beta\n\ntext b";
        let chunks = split_text_into_chunks(text);
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert!(chunks[0].starts_with("# Alpha"), "{chunks:?}");
        assert!(chunks[1].starts_with("# Beta"), "{chunks:?}");
    }

    #[test]
    fn an_oversized_section_breaks_at_line_boundaries() {
        // Every line survives whole; none is cut in half.
        let line = "abcdefghij".repeat(9);
        let text = format!("# Big\n\n{}", vec![line; 40].join("\n"));
        let chunks = split_text_into_chunks(&text);
        assert!(chunks.len() > 1, "{chunks:?}");
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= MAX_VECTOR_CHUNK_CHARS,
                "over budget: {}",
                chunk.chars().count()
            );
            for piece in chunk.lines().filter(|piece| piece.starts_with('a')) {
                assert_eq!(piece.chars().count(), 90, "a line was cut: {piece:?}");
            }
        }
    }

    #[test]
    fn a_single_line_over_the_budget_falls_back_to_a_window() {
        let text = "x".repeat(2500);
        let chunks = split_text_into_chunks(&text);
        assert_eq!(chunks.len(), 4);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= MAX_VECTOR_CHUNK_CHARS)
        );
        assert_eq!(chunks[0].chars().count(), MAX_VECTOR_CHUNK_CHARS);
    }

    #[test]
    fn a_chunk_carries_no_trailing_blank_lines() {
        // The chunk text is what gets hashed and embedded, so the section separator must
        // not leak into it.
        let text = "# A\n\nshort\n\n\n";
        for chunk in split_text_into_chunks(text) {
            assert_eq!(chunk, chunk.trim(), "{chunk:?}");
        }
    }

    #[test]
    fn branch_tour_empty_input_returns_no_chunks() {
        assert!(split_text_into_chunks("").is_empty());
        assert!(split_text_into_chunks(" \n\t\r\n ").is_empty());
    }

    #[test]
    fn branch_tour_long_section_flushes_current_chunk() {
        let short = "short line";
        let long = "y".repeat(MAX_VECTOR_CHUNK_CHARS + 100);
        let text = format!("{short}\n\n{long}");
        let chunks = split_text_into_chunks(&text);
        assert_eq!(chunks[0], short);
        assert!(chunks.len() >= 2, "{chunks:?}");
    }

    #[test]
    fn branch_tour_boundary_classification_edges() {
        assert!(is_header_line("# title"));
        assert!(is_header_line("  ## title"));
        assert!(!is_header_line("#title"));
        assert!(!is_header_line("#"));
        assert!(!is_header_line("####### title"));
        assert!(!is_header_line("plain text"));
    }

    #[test]
    fn branch_tour_walkthrough_example() {
        let text = "# One\n\n- a\n- b\n\n# Two\n\n- c\n\n# Three\n\n- d";
        let chunks = split_text_into_chunks(text);
        assert_eq!(chunks.len(), 3, "{chunks:?}");
        assert!(chunks[0].contains("- a") && chunks[0].contains("- b"));
        assert!(chunks[1].contains("- c"));
        assert!(chunks[2].contains("- d"));
    }

    #[test]
    fn chunk_keys_are_content_addressed_and_name_their_row() {
        let records = build_chunk_records(&[row(7)]);
        // Upstream 0.24.0 (#1691): the key is `type:source_hash:index`, so it survives the
        // search rows being recreated; the row is named by `row_key`.
        assert_eq!(records[0].row_key, "entity:7");
        assert_eq!(
            records[0].chunk_key,
            format!("entity:{}:0", records[0].source_hash)
        );
        assert_eq!(records[0].source_hash, sha256_hex(&records[0].chunk_text));
    }

    #[test]
    fn an_unchanged_note_keeps_its_chunk_keys_across_a_rebuild() {
        // The property #1691 exists for: the vector store's identity is the chunk key, so
        // re-embedding is skipped exactly when the key is unchanged.
        let before = build_chunk_records(&[row(7)]);
        // The rewrite recreates the row with a new id but the same text.
        let after = build_chunk_records(&[row(99)]);
        let keys = |records: &[ChunkRecord]| {
            records
                .iter()
                .map(|record| record.chunk_key.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&before), keys(&after), "the text did not change");
        assert_ne!(before[0].row_key, after[0].row_key);
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let first = ChunkRecord {
            chunk_key: format!("entity:{}:0", sha256_hex("a")),
            row_key: "entity:1".to_owned(),
            chunk_text: "a".to_owned(),
            source_hash: sha256_hex("a"),
        };
        let second = ChunkRecord {
            chunk_key: format!("entity:{}:0", sha256_hex("b")),
            row_key: "entity:1".to_owned(),
            chunk_text: "b".to_owned(),
            source_hash: sha256_hex("b"),
        };
        let forward = entity_fingerprint(&[first.clone(), second.clone()]);
        let reverse = entity_fingerprint(&[second, first]);
        assert_eq!(forward, reverse);
    }
}
