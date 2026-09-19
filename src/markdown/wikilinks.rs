//! Wikilink scanning (`[[target]]`, `[[target|label]]`, nested brackets).

use crate::domain::document::Wikilink;

/// Find every outer-most `[[...]]` link in `text`, in document order.
pub fn find_wikilinks(text: &str) -> Vec<Wikilink> {
    let mut links = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if &bytes[index..index + 2] != b"[[" {
            index += 1;
            continue;
        }
        let start = index + 2;
        let mut depth = 1usize;
        let mut position = start;
        while position + 1 < bytes.len() {
            if &bytes[position..position + 2] == b"[[" {
                depth += 1;
                position += 2;
            } else if &bytes[position..position + 2] == b"]]" {
                depth -= 1;
                if depth == 0 {
                    let raw = &text[start..position];
                    links.push(build_link(raw));
                    break;
                }
                position += 2;
            } else {
                position += 1;
            }
        }
        index = position.saturating_add(2).max(index + 2);
    }
    links
}

fn build_link(raw: &str) -> Wikilink {
    let raw_target = raw.trim().to_owned();
    match raw_target.split_once('|') {
        Some((target, label)) => Wikilink {
            target: target.trim().to_owned(),
            label: Some(label.trim().to_owned()),
            raw_target,
        },
        None => Wikilink {
            target: raw_target.clone(),
            label: None,
            raw_target,
        },
    }
}

/// Remove a display label (`target|label` → `target`).
pub fn strip_label(raw_target: &str) -> &str {
    raw_target
        .split_once('|')
        .map_or(raw_target, |(target, _)| target)
        .trim()
}

/// Normalize `project::note` references to `project/note` (reference behavior).
pub fn normalize_project_reference(identifier: &str) -> String {
    match identifier.split_once("::") {
        Some((project, remainder)) => {
            format!("{project}/{}", remainder.trim_start_matches('/'))
        }
        None => identifier.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_plain_and_labeled_links() {
        let links = find_wikilinks("see [[a/b]] and [[c/d|Label]]");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "a/b");
        assert_eq!(links[1].target, "c/d");
        assert_eq!(links[1].label.as_deref(), Some("Label"));
        assert_eq!(links[1].raw_target, "c/d|Label");
    }

    #[test]
    fn finds_nested_links() {
        let links = find_wikilinks("[[outer [[inner]] tail]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "outer [[inner]] tail");
    }

    #[test]
    fn normalizes_project_namespace() {
        assert_eq!(normalize_project_reference("proj::notes/a"), "proj/notes/a");
        assert_eq!(normalize_project_reference("notes/a"), "notes/a");
    }
}
