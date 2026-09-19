//! Semantic chunking — a port of `basic_memory.repository.semantic_chunking`.
//!
//! Markdown headers and bullets are natural boundaries; long sections are split
//! into overlapping character windows (900 chars, 120 overlap). Chunk keys are
//! `type:id:index`, and `source_hash` is the SHA-256 of the chunk text.

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
    /// `type:id:index`.
    pub chunk_key: String,
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
    let mut sections: Vec<String> = Vec::new();
    let mut current_section: Vec<&str> = Vec::new();
    for line in normalized.split('\n') {
        let is_boundary = is_header_line(line) || is_bullet_line(line);
        if is_boundary && !current_section.is_empty() {
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
        let is_bullet = is_bullet_line(&section);
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
        if is_bullet {
            if !current_chunk.is_empty() {
                chunked_sections.push(std::mem::take(&mut current_chunk));
            }
            chunked_sections.push(section);
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

fn split_long_section(section: &str) -> Vec<String> {
    let paragraphs = split_into_paragraphs(section);
    if paragraphs.is_empty() {
        return Vec::new();
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for paragraph in paragraphs {
        if char_len(&paragraph) > MAX_VECTOR_CHUNK_CHARS {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            chunks.extend(split_by_char_window(&paragraph));
            continue;
        }
        let candidate = if current.is_empty() {
            paragraph.clone()
        } else {
            format!("{current}\n\n{paragraph}")
        };
        if char_len(&candidate) <= MAX_VECTOR_CHUNK_CHARS {
            current = candidate;
        } else {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            current = paragraph;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn split_into_paragraphs(section: &str) -> Vec<String> {
    let mut result = Vec::new();
    for paragraph in section.split("\n\n") {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }
        let lines: Vec<&str> = paragraph.split('\n').collect();
        if !lines.iter().any(|line| is_bullet_line(line)) {
            result.push(paragraph.to_owned());
            continue;
        }
        let mut current_item: Vec<&str> = Vec::new();
        for line in lines {
            if is_bullet_line(line) && !current_item.is_empty() {
                result.push(current_item.join("\n").trim().to_owned());
                current_item = vec![line];
            } else {
                current_item.push(line);
            }
        }
        if !current_item.is_empty() {
            result.push(current_item.join("\n").trim().to_owned());
        }
    }
    result.into_iter().filter(|item| !item.is_empty()).collect()
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
    let mut index_by_key: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for row in rows {
        let source_text = compose_row_source_text(row);
        for (chunk_index, chunk_text) in
            split_text_into_chunks(&source_text).into_iter().enumerate()
        {
            let chunk_key = format!("{}:{}:{}", row.item_type, row.id, chunk_index);
            let source_hash = sha256_hex(&chunk_text);
            let record = ChunkRecord {
                chunk_key: chunk_key.clone(),
                chunk_text,
                source_hash,
            };
            match index_by_key.get(&chunk_key) {
                Some(index) => records[*index] = record,
                None => {
                    index_by_key.insert(chunk_key, records.len());
                    records.push(record);
                }
            }
        }
    }
    records
}

/// Reference entity fingerprint: SHA-256 over the sorted `(chunk_key, source_hash)` pairs.
pub fn entity_fingerprint(records: &[ChunkRecord]) -> String {
    let mut sorted: Vec<&ChunkRecord> = records.iter().collect();
    sorted.sort_by(|left, right| left.chunk_key.cmp(&right.chunk_key));
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

fn is_bullet_line(line: &str) -> bool {
    let mut chars = line.chars();
    matches!(chars.next(), Some('-' | '*')) && chars.next().is_some_and(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_headers_and_bullets() {
        let text = "# Title\n\nIntro line\n\n- bullet one\n- bullet two\n\nClosing line";
        let chunks = split_text_into_chunks(text);
        assert!(
            chunks.len() >= 3,
            "expected header/bullet splits, got {chunks:?}"
        );
        assert!(chunks.iter().any(|chunk| chunk.contains("bullet one")));
        assert!(chunks.iter().any(|chunk| chunk.contains("bullet two")));
    }

    #[test]
    fn long_paragraphs_use_overlapping_windows() {
        let text = "x".repeat(2500);
        let chunks = split_text_into_chunks(&text);
        assert_eq!(chunks.len(), 4);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= MAX_VECTOR_CHUNK_CHARS)
        );
        assert!(chunks[0].chars().count() == MAX_VECTOR_CHUNK_CHARS);
    }

    #[test]
    fn chunk_keys_use_type_id_index() {
        let rows = vec![SemanticRow {
            id: 7,
            item_type: "entity".to_owned(),
            title: Some("Title".to_owned()),
            permalink: Some("p/title".to_owned()),
            content_snippet: Some("Body".to_owned()),
            category: None,
            relation_type: None,
            entity_id: Some(7),
        }];
        let records = build_chunk_records(&rows);
        assert_eq!(records[0].chunk_key, "entity:7:0");
        assert_eq!(records[0].source_hash, sha256_hex(&records[0].chunk_text));
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let first = ChunkRecord {
            chunk_key: "entity:1:0".to_owned(),
            chunk_text: "a".to_owned(),
            source_hash: sha256_hex("a"),
        };
        let second = ChunkRecord {
            chunk_key: "entity:1:1".to_owned(),
            chunk_text: "b".to_owned(),
            source_hash: sha256_hex("b"),
        };
        let forward = entity_fingerprint(&[first.clone(), second.clone()]);
        let reverse = entity_fingerprint(&[second, first]);
        assert_eq!(forward, reverse);
    }
}
