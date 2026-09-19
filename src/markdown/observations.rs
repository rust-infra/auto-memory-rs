//! Observation parsing: `- [category] content #tag (context)` lines.
//!
//! Mirrors `basic_memory.markdown.plugins` for the golden corpus:
//! task markers, transcript timecodes, markdown links, wikilink-only lines, and
//! blockquote callouts are not observations; a line with an inline `#tag` counts
//! even without a category.

use crate::domain::observation::Observation;

/// Parse observations from a note body, preserving document order.
pub fn parse_observations(body: &str) -> Vec<Observation> {
    let mut observations = Vec::new();
    let mut in_fence = false;
    for raw_line in body.lines() {
        let trimmed = raw_line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed.starts_with('>') || is_heading(trimmed) {
            continue;
        }
        let content = strip_list_marker(trimmed);
        if let Some(observation) = parse_line(content) {
            observations.push(observation);
        }
    }
    observations
}

fn is_heading(line: &str) -> bool {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && line.chars().nth(hashes).is_some_and(char::is_whitespace)
}

fn strip_list_marker(line: &str) -> &str {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return rest.trim();
        }
    }
    line.trim()
}

fn parse_line(input: &str) -> Option<Observation> {
    let content = remove_links_to_directive(input);
    if content.is_empty()
        || content.starts_with("[ ]")
        || content.starts_with("[x]")
        || content.starts_with("[-]")
        || is_markdown_link(content)
        || is_wikilink_only(content)
    {
        return None;
    }

    let (category, rest) = match match_category(content) {
        Some((category, rest)) if !is_timestamp_category(category) && !is_task_marker(category) => {
            (Some(category.to_owned()), rest.to_owned())
        }
        _ => match content.strip_prefix("[] ") {
            Some(rest) => (None, rest.trim().to_owned()),
            None => (None, content.to_owned()),
        },
    };

    if category.is_none() && !has_tag(&rest) {
        return None;
    }
    if rest.is_empty() {
        return None;
    }

    let (body, context) = split_context(&rest);
    let tags = extract_tags(&body);
    Some(Observation {
        category,
        content: body,
        tags,
        context,
    })
}

fn remove_links_to_directive(content: &str) -> &str {
    content
        .strip_suffix("#bm:links_to")
        .map_or(content, |prefix| prefix.trim_end())
}

fn is_markdown_link(content: &str) -> bool {
    content.starts_with('[') && content.contains("](") && content.ends_with(')')
}

fn is_wikilink_only(content: &str) -> bool {
    content.starts_with("[[") && content.ends_with("]]")
}

/// Match `[category] rest` where the category has no brackets or parentheses.
fn match_category(content: &str) -> Option<(&str, &str)> {
    let inner = content.strip_prefix('[')?;
    let close = inner.find(']')?;
    let category = &inner[..close];
    if category.is_empty() || category.contains(['[', '(', ')']) {
        return None;
    }
    let rest = inner[close + 1..].strip_prefix(' ')?;
    if rest.trim().is_empty() {
        return None;
    }
    Some((category.trim(), rest))
}

/// Transcript timecodes (`[00:01:02]`, `[1:02:03.500]`) are not categories.
fn is_timestamp_category(category: &str) -> bool {
    let value = category.split(" - ").next().unwrap_or(category).trim();
    let mut parts = value.split(':');
    let (Some(first), Some(second)) = (parts.next(), parts.next()) else {
        return false;
    };
    if !(1..=3).contains(&first.len()) || first.chars().any(|c| !c.is_ascii_digit()) {
        return false;
    }
    let (second, third) = match parts.next() {
        Some(third) => (second, Some(third)),
        None => (second, None),
    };
    let seconds = third.unwrap_or(second);
    let seconds = seconds
        .split_once(['.', ','])
        .map_or(seconds, |(base, _)| base);
    seconds.len() == 2 && seconds.chars().all(|c| c.is_ascii_digit())
}

/// Checkbox-marker categories (`[x]`, `[/]`, `[?]`) are not observations.
fn is_task_marker(category: &str) -> bool {
    let mut chars = category.chars();
    let (Some(first), None) = (chars.next(), chars.next()) else {
        return false;
    };
    matches!(first, 'x' | 'X') || !first.is_alphanumeric()
}

fn has_tag(content: &str) -> bool {
    content.split_whitespace().any(|part| part.starts_with('#'))
}

fn split_context(content: &str) -> (String, Option<String>) {
    let trimmed = content.trim();
    if !trimmed.ends_with(')') {
        return (trimmed.to_owned(), None);
    }
    match trimmed.rfind('(') {
        Some(start) => {
            let context = trimmed[start + 1..trimmed.len() - 1].trim();
            let body = trimmed[..start].trim();
            if body.is_empty() {
                (trimmed.to_owned(), None)
            } else {
                (
                    body.to_owned(),
                    (!context.is_empty()).then(|| context.to_owned()),
                )
            }
        }
        None => (trimmed.to_owned(), None),
    }
}

fn extract_tags(content: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for part in content.split_whitespace() {
        let Some(rest) = part.strip_prefix('#') else {
            continue;
        };
        if rest.contains('#') {
            tags.extend(rest.split('#').filter(|t| !t.is_empty()).map(str::to_owned));
        } else if !rest.is_empty() {
            tags.push(rest.to_owned());
        }
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_category_context_and_tags() {
        let body = "- [requirement] Must support CJK #cjk #search\n";
        let observations = parse_observations(body);
        assert_eq!(observations.len(), 1);
        let observation = &observations[0];
        assert_eq!(observation.category.as_deref(), Some("requirement"));
        assert_eq!(observation.content, "Must support CJK #cjk #search");
        assert_eq!(observation.tags, vec!["cjk", "search"]);
        assert_eq!(observation.context, None);
    }

    #[test]
    fn tag_only_lines_count_without_category() {
        let observations = parse_observations("- With no category but a #tag\n");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].category, None);
        assert_eq!(observations[0].tags, vec!["tag"]);
    }

    #[test]
    fn skips_task_markers_timecodes_and_callouts() {
        let body = "\
- [ ] unchecked task
- [x] checked task
- [/] in progress
- [00:01:02] transcript
> - [note] callout bullet
- [note] genuine
";
        let observations = parse_observations(body);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].content, "genuine");
    }

    #[test]
    fn empty_brackets_without_tags_are_ignored() {
        assert!(parse_observations("- [] Empty bracket content\n").is_empty());
    }

    #[test]
    fn extracts_context() {
        let observations = parse_observations("- [fact] Body text (primary source)\n");
        assert_eq!(observations[0].content, "Body text");
        assert_eq!(observations[0].context.as_deref(), Some("primary source"));
    }
}
