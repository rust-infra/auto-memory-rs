//! Note-edit operations over raw markdown text.
//!
//! Ports `basic_memory.services.note_preparation`: the same operations, the same
//! section-header rules (fences are skipped, headers may be written with or without
//! `##`), the same whitespace handling, and the same error messages.
//! `tests/note_golden.rs` replays the captured reference table byte-for-byte.

use serde_yaml_ng::{Mapping, Value};
use strum::{EnumString, IntoStaticStr};

use crate::error::{Error, Result};
use crate::markdown::serialize::{dump_yaml, split_frontmatter};

/// Supported content operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum EditOperation {
    /// Append to the end of the file.
    Append,
    /// Insert after the frontmatter (or at the top when there is none).
    Prepend,
    /// Replace every occurrence of `find_text`.
    FindReplace,
    /// Replace the body of one section.
    ReplaceSection,
    /// Insert content before a section header.
    InsertBeforeSection,
    /// Insert content after a section header.
    InsertAfterSection,
}

impl EditOperation {
    /// Parse the reference operation name; `strum` maps the names, this keeps the
    /// reference's error text for an unknown one.
    pub fn parse(value: &str) -> Result<Self> {
        value
            .parse()
            .map_err(|_| invalid(format!("Unsupported operation: {value}")))
    }
}

/// Options for one edit operation.
#[derive(Debug, Clone, Default)]
pub struct EditOptions {
    /// Section header (with or without leading `#`).
    pub section: Option<String>,
    /// Text to find for [`EditOperation::FindReplace`].
    pub find_text: Option<String>,
    /// Expected number of occurrences (reference default 1).
    pub expected_replacements: usize,
    /// Whether [`EditOperation::ReplaceSection`] swallows subsections (default true).
    pub replace_subsections: bool,
}

impl EditOptions {
    /// Reference defaults: one replacement, subsections replaced.
    pub fn new() -> Self {
        Self {
            section: None,
            find_text: None,
            expected_replacements: 1,
            replace_subsections: true,
        }
    }
}

/// Apply one edit operation to a markdown document (frontmatter included).
pub fn apply_edit_operation(
    current_content: &str,
    operation: EditOperation,
    content: &str,
    options: &EditOptions,
) -> Result<String> {
    match operation {
        EditOperation::Append => Ok(format!(
            "{current_content}{}{content}",
            if !current_content.is_empty() && !current_content.ends_with('\n') {
                "\n"
            } else {
                ""
            }
        )),
        EditOperation::Prepend => prepend_after_frontmatter(current_content, content),
        EditOperation::FindReplace => find_replace(current_content, content, options),
        EditOperation::ReplaceSection => {
            let section =
                require_section(options, "section is required for replace_section operation")?;
            replace_section_content(
                current_content,
                &section,
                content,
                options.replace_subsections,
            )
        }
        EditOperation::InsertBeforeSection | EditOperation::InsertAfterSection => {
            let section =
                require_section(options, "section is required for insert section operations")?;
            let position = if operation == EditOperation::InsertBeforeSection {
                "before"
            } else {
                "after"
            };
            insert_relative_to_section(current_content, &section, content, position)
        }
    }
}

fn find_replace(current_content: &str, content: &str, options: &EditOptions) -> Result<String> {
    let Some(find_text) = options.find_text.as_deref() else {
        return Err(invalid("find_text is required for find_replace operation"));
    };
    if find_text.trim().is_empty() {
        return Err(invalid("find_text cannot be empty or whitespace only"));
    }
    let actual = current_content.matches(find_text).count();
    if actual != options.expected_replacements {
        if actual == 0 {
            return Err(invalid(format!("Text to replace not found: '{find_text}'")));
        }
        return Err(invalid(format!(
            "Expected {} occurrences of '{find_text}', but found {actual}",
            options.expected_replacements
        )));
    }
    Ok(current_content.replace(find_text, content))
}

fn require_section(options: &EditOptions, message: &str) -> Result<String> {
    let Some(section) = options.section.as_deref() else {
        return Err(invalid(message));
    };
    if section.trim().is_empty() {
        return Err(invalid("section cannot be empty or whitespace only"));
    }
    Ok(section.to_owned())
}

/// Insert `content` after the frontmatter block (or at the top without one).
pub fn prepend_after_frontmatter(current_content: &str, content: &str) -> Result<String> {
    let separator = if content.is_empty() || content.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    match split_frontmatter(current_content)? {
        Some((frontmatter, body)) => {
            let new_body = format!("{content}{separator}{}", body.trim());
            Ok(format!(
                "---\n{}---\n\n{}",
                dump_yaml(&frontmatter),
                new_body.trim()
            ))
        }
        None => Ok(format!("{content}{separator}{current_content}")),
    }
}

/// Replace the body of one section, mirroring `replace_section_content`.
///
/// A missing section is appended to the end of the document; duplicate headers are an
/// error because the replacement target would be ambiguous.
pub fn replace_section_content(
    current_content: &str,
    section_header: &str,
    new_content: &str,
    replace_subsections: bool,
) -> Result<String> {
    let header = normalize_header(section_header);
    let mut new_content = new_content.to_owned();
    let trimmed: Vec<&str> = new_content.trim_start().split('\n').collect();
    if trimmed
        .first()
        .is_some_and(|line| line.trim() == header.trim())
    {
        new_content = trimmed[1..].join("\n").trim_start().to_owned();
    }

    let lines: Vec<&str> = current_content.split('\n').collect();
    let fenced = fenced_code_line_flags(&lines);
    let matches: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| !fenced[*index] && line.trim() == header.trim())
        .map(|(index, _)| index)
        .collect();
    if matches.len() > 1 {
        return Err(invalid(format!(
            "Multiple sections found with header '{header}'. Section replacement requires unique headers."
        )));
    }
    let Some(&section_line) = matches.first() else {
        let separator = if !current_content.is_empty() && !current_content.ends_with("\n\n") {
            "\n\n"
        } else {
            ""
        };
        return Ok(format!(
            "{current_content}{separator}{header}\n{new_content}"
        ));
    };

    let target_level = header_level(&header);
    let mut end_index = lines.len();
    for index in (section_line + 1)..lines.len() {
        if fenced[index] {
            continue;
        }
        if let Some(level) = markdown_heading_level(lines[index]) {
            if !replace_subsections || level <= target_level {
                end_index = index;
                break;
            }
        }
    }
    let mut replaced: Vec<&str> = lines[..=section_line].to_vec();
    replaced.push(&new_content);
    replaced.extend_from_slice(&lines[end_index..]);
    Ok(replaced.join("\n"))
}

/// Insert `new_content` before or after one section header.
pub fn insert_relative_to_section(
    current_content: &str,
    section_header: &str,
    new_content: &str,
    position: &str,
) -> Result<String> {
    let header = normalize_header(section_header);
    let lines: Vec<&str> = current_content.split('\n').collect();
    let fenced = fenced_code_line_flags(&lines);
    let matches: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| !fenced[*index] && line.trim() == header.trim())
        .map(|(index, _)| index)
        .collect();
    if matches.is_empty() {
        return Err(invalid(format!(
            "Section '{header}' not found in document. Use replace_section to create a new section."
        )));
    }
    if matches.len() > 1 {
        return Err(invalid(format!(
            "Multiple sections found with header '{header}'. Section insertion requires unique headers."
        )));
    }
    let index = matches[0];
    let mut insert_lines: Vec<&str> = new_content.trim_end_matches('\n').split('\n').collect();

    let mut output: Vec<&str> = Vec::with_capacity(lines.len() + insert_lines.len());
    if position == "before" {
        let before = &lines[..index];
        if before.last().is_some_and(|line| !line.trim().is_empty()) {
            output.push("");
        }
        output.extend(before);
        output.append(&mut insert_lines);
        output.push("");
        output.extend_from_slice(&lines[index..]);
    } else {
        output.extend_from_slice(&lines[..=index]);
        let after = &lines[index + 1..];
        if after.first().is_some_and(|line| !line.trim().is_empty()) {
            output.append(&mut insert_lines);
            output.push("");
        } else {
            output.append(&mut insert_lines);
        }
        output.extend_from_slice(after);
    }
    Ok(output.join("\n"))
}

/// Merge caller metadata into the frontmatter, keeping the body byte-identical.
///
/// Mirrors `_merge_metadata_into_markdown`: `title`/`type`/`permalink` are resolved
/// elsewhere and dropped from the merge, `null` values are rejected, and the body is
/// re-attached without reflowing it.
pub fn merge_metadata_into_markdown(
    markdown_content: &str,
    metadata: &[(String, Value)],
) -> Result<String> {
    let nulls: Vec<String> = metadata
        .iter()
        .filter(|(_, value)| matches!(value, Value::Null))
        .map(|(key, _)| key.clone())
        .collect();
    if !nulls.is_empty() {
        let mut sorted = nulls;
        sorted.sort();
        return Err(invalid(format!(
            "metadata values cannot be null (key deletion is not supported): {}",
            sorted.join(", ")
        )));
    }
    let sanitized: Vec<(String, Value)> = metadata
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "title" | "type" | "permalink"))
        .cloned()
        .collect();
    if sanitized.is_empty() {
        return Ok(markdown_content.to_owned());
    }

    let (mut current, body, had_separator) = match split_frontmatter(markdown_content)? {
        Some((mapping, body)) => {
            // `strip=False`: drop exactly one separator newline so the body round-trips.
            let (body, separator) = match body.strip_prefix("\r\n") {
                Some(rest) => (rest.to_owned(), true),
                None => match body.strip_prefix('\n') {
                    Some(rest) => (rest.to_owned(), true),
                    None => (body, false),
                },
            };
            (mapping, body, separator)
        }
        None => (Mapping::new(), markdown_content.to_owned(), true),
    };
    for (key, value) in &sanitized {
        current.insert(Value::String(key.clone()), value.clone());
    }
    let frontmatter = format!("---\n{}---\n", dump_yaml(&current));
    if !had_separator && !body.is_empty() {
        // The note had no blank line after the fence; `dump_frontmatter` always adds
        // one, so serialize the frontmatter alone and reattach the body verbatim.
        return Ok(format!("{frontmatter}{body}"));
    }
    Ok(format!("{frontmatter}\n{body}"))
}

fn normalize_header(section_header: &str) -> String {
    if section_header.starts_with('#') {
        section_header.to_owned()
    } else {
        format!("## {section_header}")
    }
}

fn header_level(header: &str) -> usize {
    header.len() - header.trim_start_matches('#').len()
}

/// Markdown heading level of one line (reference `_markdown_heading_level`).
pub fn markdown_heading_level(line: &str) -> Option<usize> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let candidate = &line[indent..];
    if !candidate.starts_with('#') {
        return None;
    }
    let level = candidate.len() - candidate.trim_start_matches('#').len();
    if level > 6 {
        return None;
    }
    let rest = &candidate[level..];
    if rest.is_empty() || rest.starts_with([' ', '\t']) {
        Some(level)
    } else {
        None
    }
}

/// Fence marker of one line: `(character, length, suffix)`.
fn fence_marker(line: &str) -> Option<(char, usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let candidate = &line[indent..];
    let marker = candidate.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let length = candidate.len() - candidate.trim_start_matches(marker).len();
    if length < 3 {
        return None;
    }
    Some((marker, length, &candidate[length..]))
}

/// Whether each line sits inside a fenced code block (reference `_fenced_code_line_flags`).
pub fn fenced_code_line_flags(lines: &[&str]) -> Vec<bool> {
    let mut flags = Vec::with_capacity(lines.len());
    let mut open: Option<(char, usize)> = None;
    for line in lines {
        let marker = fence_marker(line);
        match open {
            None => match marker {
                None => flags.push(false),
                Some((marker_char, marker_length, suffix)) => {
                    if marker_char == '`' && suffix.contains('`') {
                        flags.push(false);
                        continue;
                    }
                    flags.push(true);
                    open = Some((marker_char, marker_length));
                }
            },
            Some((open_char, open_length)) => {
                flags.push(true);
                if let Some((marker_char, marker_length, suffix)) = marker {
                    if marker_char == open_char
                        && marker_length >= open_length
                        && suffix.trim().is_empty()
                    {
                        open = None;
                    }
                }
            }
        }
    }
    flags
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidArgument {
        message: message.into(),
    }
}
