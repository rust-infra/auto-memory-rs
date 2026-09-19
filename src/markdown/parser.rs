//! Compose the markdown layers into a `ParsedDocument`.

use std::path::Path;

use crate::domain::document::ParsedDocument;
use crate::error::Result;
use crate::markdown::frontmatter;
use crate::markdown::observations::parse_observations;
use crate::markdown::relations::parse_relations;
use crate::markdown::wikilinks::find_wikilinks;

/// Parse one markdown document.
///
/// `file_path` is the project-relative path (used for titles and diagnostics);
/// `content` is the raw file contents.
pub fn parse_document(file_path: impl AsRef<Path>, content: &str) -> Result<ParsedDocument> {
    let path = file_path.as_ref();
    let file_path = path.to_string_lossy().replace('\\', "/");
    let file_stem = path.file_stem().map_or_else(
        || file_path.clone(),
        |stem| stem.to_string_lossy().into_owned(),
    );

    let parsed = frontmatter::parse(content, &file_stem)?;
    let observations = parse_observations(&parsed.body);
    let relations = parse_relations(&parsed.body);
    let wikilinks = find_wikilinks(&parsed.body);

    Ok(ParsedDocument {
        file_path,
        had_frontmatter: parsed.had_frontmatter,
        frontmatter_error: parsed.frontmatter_error,
        frontmatter: parsed.frontmatter,
        created: parsed.created,
        modified: parsed.modified,
        content: parsed.body,
        observations,
        relations,
        wikilinks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_document() {
        let content = "\
---
title: Demo
type: reference
tags: [rust, architecture]
---

# Demo

- [decision] Keep Markdown as the source of truth
- depends_on [[projects/alpha]]
See also [[notes/simple|Simple]].
";
        let document = parse_document("notes/demo.md", content).expect("parse");
        assert!(document.had_frontmatter);
        assert_eq!(document.frontmatter.title, "Demo");
        assert_eq!(document.frontmatter.note_type, "reference");
        assert_eq!(document.frontmatter.tags, vec!["rust", "architecture"]);
        assert_eq!(document.observations.len(), 1);
        assert_eq!(document.relations.len(), 2);
        assert_eq!(document.relations[0].relation_type.as_str(), "depends_on");
        assert_eq!(document.relations[1].relation_type.as_str(), "links_to");
        assert_eq!(document.wikilinks.len(), 2);
    }

    #[test]
    fn malformed_yaml_falls_back_to_plain_markdown() {
        let content = "---\ntitle: \"Unterminated\ntags: [a, b\n---\n\nBody\n";
        let document = parse_document("notes/broken.md", content).expect("parse");
        assert!(!document.had_frontmatter);
        assert_eq!(document.frontmatter.title, "broken");
        assert_eq!(document.frontmatter.note_type, "note");
        assert!(document.content.starts_with("---"));
    }
}
