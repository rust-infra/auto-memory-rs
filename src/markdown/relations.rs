//! Relation parsing: explicit `- type [[target]] (context)` and prose wikilinks.

use crate::domain::permalink::RelationType;
use crate::domain::relation::Relation;
use crate::markdown::wikilinks::{find_wikilinks, normalize_project_reference};

/// Parse relations from a note body, preserving document order.
pub fn parse_relations(body: &str) -> Vec<Relation> {
    let mut relations = Vec::new();
    let mut in_fence = false;
    for raw_line in body.lines() {
        let trimmed = raw_line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let (in_list, content) = split_line(trimmed);
        if content.is_empty() {
            continue;
        }
        let (content, has_directive) = remove_links_to_directive(content);
        if in_list && !has_directive {
            if let Some(relation) = parse_explicit(content) {
                relations.push(relation);
                continue;
            }
        }
        for link in find_wikilinks(content) {
            if let Ok(relation_type) = RelationType::new("links_to") {
                relations.push(Relation {
                    relation_type,
                    target: normalize_project_reference(&link.raw_target),
                    context: None,
                });
            }
        }
        // Upstream 0.24.0 (#1246): an ordinary Markdown link to a file in the project is a
        // `links_to` relation, the way a wikilink is. External URLs, images, fragment-only
        // links and paths outside the project are ignored, and the authored Markdown is
        // never rewritten.
        for target in find_markdown_links(content) {
            if let Ok(relation_type) = RelationType::new("links_to") {
                relations.push(Relation {
                    relation_type,
                    target,
                    context: None,
                });
            }
        }
    }
    relations
}

/// Project-relative targets of the Markdown links on one line.
///
/// Handles `[text](path)`, `[text](../path.md)` and reference-style `[text][label]` bodies
/// only insofar as the inline form goes; the resolution against the note's own path
/// happens in the graph, so the target is stored as authored (upstream 0.24.0 #1514).
fn find_markdown_links(line: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find("](") {
        let before = &rest[..open];
        // The link text must start with a `[` on this line, otherwise this is prose.
        let Some(text_start) = before.rfind('[') else {
            rest = &rest[open + 2..];
            continue;
        };
        let after = &rest[open + 2..];
        let Some(close) = after.find(')') else {
            break;
        };
        let target = after[..close].trim();
        rest = &after[close + 1..];
        // A title in the target (`path "title"`) is not part of the path.
        let target = target.split_whitespace().next().unwrap_or_default();
        if is_project_relative_link(target) {
            let _ = text_start;
            targets.push(target.to_owned());
        }
    }
    targets
}

/// Whether a Markdown link target names a file inside the project.
///
/// Ignored, per upstream: an absolute URL with a scheme, an image, a bare fragment, and a
/// path that climbs out of the project.
fn is_project_relative_link(target: &str) -> bool {
    if target.is_empty() || target.starts_with('#') {
        return false;
    }
    // A scheme (`https:`, `mailto:`) means the target is not a project file.
    if let Some((scheme, _)) = target.split_once(':')
        && !scheme.contains('/')
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+')
    {
        return false;
    }
    // An image target is not a note.
    if target.starts_with("data:") {
        return false;
    }
    // A path that climbs out of the project is not a link to a note in it.
    let mut depth: i32 = 0;
    for segment in target.split('/') {
        match segment {
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            "." | "" => {}
            _ => depth += 1,
        }
    }
    true
}

fn split_line(line: &str) -> (bool, &str) {
    let mut content = line;
    let mut in_list = false;
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = content.strip_prefix(marker) {
            content = rest;
            in_list = true;
            break;
        }
    }
    if !in_list {
        // Headings: the reference parses the inline content without the `#` markers.
        let trimmed = content.trim_start_matches('#');
        if trimmed.len() != content.len() {
            content = trimmed.strip_prefix(' ').unwrap_or(trimmed);
        }
        // Blockquotes: strip the marker so inline wikilinks are still found.
        while let Some(rest) = content.strip_prefix('>') {
            content = rest.strip_prefix(' ').unwrap_or(rest);
        }
    }
    (in_list, content.trim())
}

fn remove_links_to_directive(content: &str) -> (&str, bool) {
    match content.strip_suffix("#bm:links_to") {
        Some(prefix) => (prefix.trim_end(), true),
        None => (content, false),
    }
}

fn parse_explicit(content: &str) -> Option<Relation> {
    let link_start = content.find("[[")?;
    let label = content[..link_start].trim();
    let relation_type = parse_relation_type(label)?;
    let target_end = content[link_start + 2..].find("]]")? + link_start + 2;
    let target = normalize_project_reference(content[link_start + 2..target_end].trim());
    if target.is_empty() {
        return None;
    }
    let after = content[target_end + 2..].trim();
    let context = if after.is_empty() {
        None
    } else {
        let inner = single_parenthesized(after)?;
        if inner.is_empty() {
            None
        } else {
            Some(inner.to_owned())
        }
    };
    Some(Relation {
        relation_type: RelationType::new(relation_type).ok()?,
        target,
        context,
    })
}

fn parse_relation_type(label: &str) -> Option<&str> {
    if label.is_empty() {
        return None;
    }
    let first = label.chars().next()?;
    if (first == '"' || first == '\'') && label.ends_with(first) && label.len() >= 2 {
        let inner = label[first.len_utf8()..label.len() - first.len_utf8()].trim();
        return (!inner.is_empty()).then_some(inner);
    }
    if label.chars().any(char::is_whitespace) {
        return None;
    }
    Some(label)
}

/// Return the inner text when `text` is exactly one balanced `(...)` group.
fn single_parenthesized(text: &str) -> Option<&str> {
    if !text.starts_with('(') {
        return None;
    }
    let mut depth = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    if index + 1 == text.len() {
                        return Some(&text[1..index]);
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_relations() {
        let body = "- depends_on [[a]]\n- \"implemented by\" [[b]] (ctx)\n";
        let relations = parse_relations(body);
        assert_eq!(relations.len(), 2);
        assert_eq!(relations[0].relation_type.as_str(), "depends_on");
        assert_eq!(relations[0].target, "a");
        assert_eq!(relations[1].relation_type.as_str(), "implemented by");
        assert_eq!(relations[1].context.as_deref(), Some("ctx"));
    }

    #[test]
    fn prose_links_become_links_to() {
        let relations = parse_relations("See [[a/b]] and [[c|Label]].");
        assert_eq!(relations.len(), 2);
        assert!(
            relations
                .iter()
                .all(|r| r.relation_type.as_str() == "links_to")
        );
        assert_eq!(relations[1].target, "c|Label");
    }

    #[test]
    fn prose_tail_disqualifies_explicit_relation() {
        let relations = parse_relations("- some other thing [[a]] tail");
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].relation_type.as_str(), "links_to");
    }

    #[test]
    fn links_to_directive_forces_inline_handling() {
        let relations = parse_relations("- depends_on [[a]] #bm:links_to");
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].relation_type.as_str(), "links_to");
    }
}
