//! Section splitting: a note's body indexed by heading.
//!
//! Upstream 0.24.0 (#1398) added a `note_section` table so the POSIX-style read tools can
//! address one section of a note: `cat` reads a heading, a line range, or a token budget
//! instead of the whole file. The table is what makes `grep -C 3` cheap — the match is
//! located against the sections, not re-parsed from disk on every call.
//!
//! A section runs from its heading to the next heading **of the same or higher level**, so
//! `## Open items` contains its own `###` children and stops at the next `##`. Text before
//! the first heading is the document's preamble and is indexed as a section with no
//! heading, which is what `cat` returns when the caller asks for lines 1..N of a note that
//! starts with prose.
//!
//! Fenced code blocks are skipped while looking for headings: a `# comment` inside a
//! ``` fence is not a section of the note.

use serde::{Deserialize, Serialize};

/// One heading-delimited section of a note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSection {
    /// Heading level 1–6; 0 for the preamble before the first heading.
    pub level: u8,
    /// Heading text without the leading `#`s, trimmed. Empty for the preamble.
    pub heading: String,
    /// 1-based line where the section starts (the heading line, or 1 for the preamble).
    pub start_line: usize,
    /// 1-based, inclusive last line of the section.
    pub end_line: usize,
    /// The section's own text, heading included.
    pub content: String,
}

impl NoteSection {
    /// A stable anchor for the section: its heading, or `preamble`.
    #[must_use]
    pub fn anchor(&self) -> String {
        if self.heading.is_empty() {
            "preamble".to_owned()
        } else {
            self.heading.clone()
        }
    }

    /// Number of lines the section spans.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.end_line.saturating_sub(self.start_line) + 1
    }
}

/// Split `body` into its heading-delimited sections.
///
/// The returned sections are contiguous and cover the whole document, so their line ranges
/// partition it: a caller can address any line through exactly one section.
#[must_use]
pub fn split_sections(body: &str) -> Vec<NoteSection> {
    let lines: Vec<&str> = body.split('\n').collect();
    // A trailing newline produces one empty final element that is not a line.
    let total = if body.is_empty() {
        0
    } else if body.ends_with('\n') {
        lines.len() - 1
    } else {
        lines.len()
    };
    if total == 0 {
        return Vec::new();
    }

    // Collect heading positions first: a section's extent depends on the *next* heading of
    // the same or higher level, which is not known until every heading is found.
    let mut headings: Vec<(usize, u8, String)> = Vec::new();
    let mut in_fence = false;
    for (index, line) in lines.iter().enumerate().take(total) {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some((level, text)) = parse_heading(trimmed) {
            headings.push((index, level, text));
        }
    }

    let mut sections: Vec<NoteSection> = Vec::new();
    // The preamble: everything before the first heading.
    let first_heading_line = headings.first().map(|(index, _, _)| *index);
    if first_heading_line.unwrap_or(total) > 0 {
        let end = first_heading_line.unwrap_or(total);
        sections.push(NoteSection {
            level: 0,
            heading: String::new(),
            start_line: 1,
            end_line: end,
            content: lines[..end].join("\n"),
        });
    }

    for (position, (index, level, heading)) in headings.iter().enumerate() {
        // The section's own line range ends at the *next* heading of any level, so the
        // ranges partition the document and a caller can address any line through exactly
        // one section. Its `content` runs further — through its subsections — because that
        // is what "the Open items section" means to a reader.
        let own_end = headings
            .get(position + 1)
            .map_or(total, |(next_index, _, _)| *next_index);
        let mut content_end = total;
        for (next_index, next_level, _) in &headings[position + 1..] {
            if next_level <= level {
                content_end = *next_index;
                break;
            }
        }
        sections.push(NoteSection {
            level: *level,
            heading: heading.clone(),
            start_line: index + 1,
            end_line: own_end,
            content: lines[*index..content_end].join("\n"),
        });
    }
    sections
}

/// Find the section a heading names, preferring an exact (case-insensitive) match.
///
/// A heading may be given with or without its `#`s: `Open items`, `## Open items` and
/// `#open-items` are all accepted spellings of the same section.
#[must_use]
pub fn find_section<'a>(sections: &'a [NoteSection], wanted: &str) -> Option<&'a NoteSection> {
    let normalized = normalize_heading(wanted);
    sections.iter().find(|section| {
        !section.heading.is_empty() && normalize_heading(&section.heading) == normalized
    })
}

/// Normalize a heading for comparison: drop the `#`s, lowercase, and treat a hyphen or an
/// underscore as the space an anchor slug would have used.
fn normalize_heading(heading: &str) -> String {
    let text = heading
        .trim()
        .trim_start_matches('#')
        .trim()
        .trim_end_matches('#')
        .trim()
        .to_lowercase();
    text.chars()
        .map(|character| match character {
            '-' | '_' => ' ',
            other => other,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse `## Heading` into its level and text.
fn parse_heading(line: &str) -> Option<(u8, String)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    // ATX headings require whitespace after the `#`s; `#tag` is not a heading.
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    if text.is_empty() {
        return None;
    }
    Some((hashes as u8, text.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "# Title\n\nIntro line.\n\n## Open items\n\n- one\n- two\n\n### Nested\n\nnested body\n\n## Closed\n\ndone\n";

    #[test]
    fn sections_partition_the_document() {
        let sections = split_sections(NOTE);
        // Every line belongs to exactly one section, and they are in order.
        let mut expected_start = 1;
        for section in &sections {
            assert_eq!(section.start_line, expected_start, "{section:?}");
            expected_start = section.end_line + 1;
        }
        assert_eq!(expected_start, NOTE.lines().count() + 1);
    }

    #[test]
    fn a_subsection_belongs_to_its_parent() {
        let sections = split_sections(NOTE);
        let open = find_section(&sections, "Open items").expect("open items");
        assert_eq!(open.level, 2);
        // The section runs through its `###` child and stops at the next `##`.
        assert!(open.content.contains("### Nested"), "{}", open.content);
        assert!(open.content.contains("nested body"), "{}", open.content);
        assert!(!open.content.contains("## Closed"), "{}", open.content);
    }

    #[test]
    fn a_heading_matches_with_or_without_its_hashes() {
        let sections = split_sections(NOTE);
        for spelling in [
            "Open items",
            "## Open items",
            "#open-items",
            "  OPEN ITEMS  ",
        ] {
            assert!(
                find_section(&sections, spelling).is_some(),
                "spelling {spelling:?} must match"
            );
        }
        assert!(find_section(&sections, "Missing").is_none());
    }

    #[test]
    fn the_preamble_is_a_section_when_the_note_starts_with_prose() {
        let body = "Some prose first.\n\n# Heading\n\nbody\n";
        let sections = split_sections(body);
        assert_eq!(sections[0].level, 0);
        assert_eq!(sections[0].anchor(), "preamble");
        assert_eq!(sections[0].start_line, 1);
        assert!(sections[0].content.contains("Some prose first."));
    }

    #[test]
    fn a_note_starting_with_a_heading_has_no_preamble() {
        let sections = split_sections("# Only\n\nbody\n");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].heading, "Only");
        assert_eq!(sections[0].start_line, 1);
    }

    #[test]
    fn a_heading_inside_a_fence_is_not_a_section() {
        let body = "# Real\n\n```sh\n# not a heading\n```\n\n## Also real\n\nbody\n";
        let sections = split_sections(body);
        let headings: Vec<&str> = sections
            .iter()
            .filter(|section| !section.heading.is_empty())
            .map(|section| section.heading.as_str())
            .collect();
        assert_eq!(headings, vec!["Real", "Also real"], "{headings:?}");
    }

    #[test]
    fn a_hashtag_is_not_a_heading() {
        // `#tag` has no space after the `#`, so it is prose.
        let sections = split_sections("#real\n\nbody\n");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].level, 0);
    }

    #[test]
    fn an_empty_note_has_no_sections() {
        assert!(split_sections("").is_empty());
    }

    #[test]
    fn line_counts_cover_the_sections_own_lines() {
        let sections = split_sections(NOTE);
        let body_lines: Vec<&str> = NOTE.split('\n').collect();
        for section in &sections {
            // `content` may include subsections, so it is at least as long as the range.
            let own: Vec<&str> = body_lines[section.start_line - 1..section.end_line].to_vec();
            assert_eq!(own.len(), section.line_count(), "{section:?}");
            assert!(
                section.content.starts_with(own[0]),
                "{section:?} must start at its own first line"
            );
        }
    }
}
