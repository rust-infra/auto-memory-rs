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
    fn branch_tour_empty_input_returns_no_chunks() {
        // C01 / B01: the public entry returns before sectioning.
        assert!(split_text_into_chunks("").is_empty());
        assert!(split_text_into_chunks(" \n\t\r\n ").is_empty());
    }

    #[test]
    fn branch_tour_boundaries_bullets_and_empty_tail() {
        // C02 / B03-B05, B11-B19.
        let text = "# First Heading\n\nintro one\n\n## Second Heading\n\nintro two\n\n- bullet one\n- bullet two\n\ntail";
        assert_eq!(
            split_text_into_chunks(text),
            vec![
                "# First Heading\n\nintro one\n\n## Second Heading\n\nintro two",
                "- bullet one",
                "- bullet two\n\ntail",
            ]
        );
    }

    #[test]
    fn branch_tour_candidate_limit_flushes_before_merging() {
        // C03 / B03-B05, B14-B18.
        let a = "a".repeat(600);
        let b = "b".repeat(400);
        let first = format!("# A\n{a}");
        let second = format!("# B\n{b}");
        let third = "# C\ntail";
        let text = format!("{first}\n{second}\n{third}");
        assert_eq!(
            split_text_into_chunks(&text),
            vec![first, format!("{second}\n\n{third}")]
        );
    }

    #[test]
    fn branch_tour_long_section_flushes_current_chunk() {
        // C04 / B03-B10, B18; includes split_long_section merge/flush paths.
        let short = "short prose";
        let a = "a".repeat(500);
        let b = "b".repeat(500);
        let long = format!("# Long\n{a}\n\n{b}");
        let text = format!("{short}\n{long}");
        assert_eq!(
            split_text_into_chunks(&text),
            vec![short.to_owned(), format!("# Long\n{a}"), b]
        );
    }

    #[test]
    fn branch_tour_long_paragraph_uses_overlapping_windows() {
        // C05 / B02, B06-B10, B18; covers split_by_char_window loop and overlap.
        let text: String = (0..2500)
            .map(|index| char::from(b'a' + (index % 26) as u8))
            .collect();
        let chunks = split_text_into_chunks(&text);
        let lengths: Vec<usize> = chunks.iter().map(|chunk| chunk.chars().count()).collect();
        assert_eq!(lengths, vec![900, 900, 900, 160]);

        let c0: Vec<char> = chunks[0].chars().collect();
        let c1: Vec<char> = chunks[1].chars().collect();
        let c2: Vec<char> = chunks[2].chars().collect();
        assert_eq!(&c0[780..900], &c1[..120]);
        assert_eq!(&c1[780..900], &c2[..120]);
    }

    #[test]
    fn branch_tour_boundary_classification_edges() {
        // C08: the exact heading and bullet classifiers.
        assert!(is_header_line("# title"));
        assert!(is_header_line("  ## title"));
        assert!(!is_header_line("#title"));
        assert!(!is_header_line("#"));
        assert!(!is_header_line("####### title"));
        assert!(!is_header_line("plain text"));

        assert!(is_bullet_line("- item"));
        assert!(is_bullet_line("* item"));
        assert!(!is_bullet_line(""));
        assert!(!is_bullet_line("-"));
        assert!(!is_bullet_line("-item"));
        assert!(!is_bullet_line("+ item"));
        assert!(!is_bullet_line("  - item"));
    }

    #[test]
    fn branch_tour_long_paragraph_after_short_paragraph() {
        // C07 / L03, L08; current is non-empty when the long paragraph starts.
        let short = "s".repeat(100);
        let long = "l".repeat(1000);
        let text = format!("{short}\n\n{long}");
        let chunks = split_text_into_chunks(&text);
        let lengths: Vec<usize> = chunks.iter().map(|chunk| chunk.chars().count()).collect();
        assert_eq!(lengths, vec![100, 900, 220]);
        assert_eq!(chunks[0], short);
    }

    #[test]
    fn branch_tour_walkthrough_example() {
        // C09: the single running example in docs/chunking-walkthrough.md.
        let a = "a".repeat(100);
        let b = "b".repeat(600);
        let c = "c".repeat(400);
        let d = "d".repeat(100);
        let e = "e".repeat(1000);
        let text = format!(
            "# 开场\n{a}\n\n## 中段\n{b}\n\n## 继续\n{c}\n\n- 第一条 bullet\n- 第二条 bullet\nbullet 后面的普通文字\n\n## 长段\n{d}\n\n{e}\n\n- 最后一条 bullet"
        );
        let chunks = split_text_into_chunks(&text);
        assert_eq!(chunks.len(), 8);
        assert!(chunks[0].contains("# 开场"));
        assert!(chunks[0].contains("## 中段"));
        assert!(chunks[1].contains("## 继续"));
        assert_eq!(chunks[2], "- 第一条 bullet");
        assert!(chunks[3].starts_with("- 第二条 bullet"));
        assert!(chunks[4].contains("## 长段"));
        assert_eq!(chunks[5].chars().count(), 900);
        assert_eq!(chunks[6].chars().count(), 220);
        assert_eq!(chunks[7], "- 最后一条 bullet");
    }

    #[test]
    fn branch_tour_field_prefix_can_be_its_own_chunk() {
        // C10: composed fields are paragraphs, not special chunk types.
        let title = "Title";
        let permalink = "project/notes/example";
        let body = "x".repeat(1000);
        let text = format!("{title}\n\n{permalink}\n\n{body}");
        let chunks = split_text_into_chunks(&text);
        let lengths: Vec<usize> = chunks.iter().map(|chunk| chunk.chars().count()).collect();
        assert_eq!(chunks[0], format!("{title}\n\n{permalink}"));
        assert_eq!(lengths, vec![chunks[0].chars().count(), 900, 220]);
    }

    #[test]
    fn branch_tour_long_bullet_uses_long_section_before_bullet_rule() {
        // C11: the >900 branch has priority over the short-bullet branch.
        let bullet = format!("- {}", "x".repeat(1000));
        let text = format!("before\n{bullet}");
        let chunks = split_text_into_chunks(&text);
        let lengths: Vec<usize> = chunks.iter().map(|chunk| chunk.chars().count()).collect();
        assert_eq!(chunks[0], "before");
        assert_eq!(lengths, vec![6, 900, 222]);
    }

    #[test]
    fn branch_tour_helper_defensive_paths() {
        // C06 / L01, P01-P04, W01; direct helper branches that the public entry
        // cannot reach once sectioning has removed blank input.
        assert!(split_long_section("").is_empty());
        assert!(split_by_char_window("").is_empty());
        assert_eq!(split_into_paragraphs("A\n\n\n\nB"), vec!["A", "B"]);
        assert_eq!(
            split_into_paragraphs("- one\n- two"),
            vec!["- one", "- two"]
        );
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
