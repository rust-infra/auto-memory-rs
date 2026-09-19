//! YAML frontmatter parsing and normalization.
//!
//! Mirrors `basic_memory.markdown.entity_parser` for the cases in the golden
//! corpus: values are normalized to strings/arrays/objects, `title` falls back to
//! the file stem, `type` defaults to `note`, and malformed YAML falls back to
//! "plain markdown" (the file is later skipped by the indexer, not by the parser).

use serde_json::{Map, Value};

use crate::domain::Frontmatter;
use crate::domain::permalink::Permalink;
use crate::domain::timeframe::{self, Instant};
use crate::error::Result;

/// Result of splitting a markdown file into frontmatter and body.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedFrontmatter {
    /// Whether a parseable YAML frontmatter block was present.
    pub had_frontmatter: bool,
    /// Whether a frontmatter block was present but could not be parsed as YAML.
    ///
    /// The reference parser falls back to plain markdown, but the indexer later
    /// drops the file, so the flag lets the index layer mirror that behavior.
    pub frontmatter_error: bool,
    /// Normalized frontmatter.
    pub frontmatter: Frontmatter,
    /// `created` frontmatter value (reference `EntityMarkdown.created`).
    pub created: Option<Instant>,
    /// `modified` frontmatter value (reference `EntityMarkdown.modified`).
    pub modified: Option<Instant>,
    /// Body content after the frontmatter block (or the whole file on fallback).
    pub body: String,
}

/// Parse frontmatter plus body from raw markdown.
///
/// `file_stem` is the note filename without extension, used as the title fallback.
pub fn parse(content: &str, file_stem: &str) -> Result<ParsedFrontmatter> {
    let raw = content.replace("\r\n", "\n").replace('\u{feff}', "");
    let text = raw.trim();
    let Some((yaml, body)) = split_frontmatter(text) else {
        return Ok(ParsedFrontmatter {
            had_frontmatter: false,
            frontmatter_error: false,
            frontmatter: default_frontmatter(file_stem),
            created: None,
            modified: None,
            body: text.to_owned(),
        });
    };

    let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(yaml) else {
        // Reference behavior: malformed YAML falls back to plain markdown and the
        // *untrimmed* file (including the `---` block) becomes the body.
        return Ok(ParsedFrontmatter {
            had_frontmatter: false,
            frontmatter_error: true,
            frontmatter: default_frontmatter(file_stem),
            created: None,
            modified: None,
            body: raw,
        });
    };

    let mut metadata = match normalize_value(&value) {
        Value::Object(map) => map,
        _ => Map::new(),
    };

    let title = match metadata.get("title") {
        Some(Value::Null) | None => file_stem.to_owned(),
        Some(value) => coerce_to_string(value),
    };
    let title = if title.is_empty() || title == "None" {
        file_stem.to_owned()
    } else {
        title
    };

    let note_type = match metadata.get("type") {
        Some(Value::Null) | None => "note".to_owned(),
        Some(value) => coerce_to_string(value),
    };
    let note_type = if note_type.is_empty() {
        "note".to_owned()
    } else {
        note_type
    };

    let tags = parse_tags(metadata.get("tags"));
    let permalink = match metadata.get("permalink") {
        Some(Value::String(value)) if !value.is_empty() => Some(Permalink::new(value.clone())?),
        _ => None,
    };
    // Canonical frontmatter timestamps describe note semantics; the index layer
    // falls back to file times only when they are absent (reference
    // `_parse_frontmatter_timestamp`).
    let created = frontmatter_timestamp(metadata.get("created"))?;
    let modified = frontmatter_timestamp(metadata.get("modified"))?;

    metadata.insert("title".to_owned(), Value::String(title.clone()));
    metadata.insert("type".to_owned(), Value::String(note_type.clone()));
    if !tags.is_empty() {
        metadata.insert(
            "tags".to_owned(),
            Value::Array(tags.iter().cloned().map(Value::String).collect()),
        );
    }

    Ok(ParsedFrontmatter {
        had_frontmatter: true,
        frontmatter_error: false,
        frontmatter: Frontmatter {
            title,
            note_type,
            permalink,
            tags,
            metadata,
        },
        created,
        modified,
        body: body.trim().to_owned(),
    })
}

/// Parse one optional canonical timestamp from normalized frontmatter.
fn frontmatter_timestamp(value: Option<&Value>) -> Result<Option<Instant>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let text = coerce_to_string(value);
            timeframe::parse_frontmatter_timestamp(&text).map(Some)
        }
    }
}

fn default_frontmatter(file_stem: &str) -> Frontmatter {
    let mut metadata = Map::new();
    metadata.insert("title".to_owned(), Value::String(file_stem.to_owned()));
    metadata.insert("type".to_owned(), Value::String("note".to_owned()));
    Frontmatter {
        title: file_stem.to_owned(),
        note_type: "note".to_owned(),
        permalink: None,
        tags: Vec::new(),
        metadata,
    }
}

/// Split a leading `---` YAML block from the body.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---\n")?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" || trimmed == "..." {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Some((yaml, body));
        }
        offset += line.len();
    }
    // Unterminated block: treat as no frontmatter, like the reference parser does
    // once YAML decoding fails.
    None
}

/// Normalize YAML values the way the reference `normalize_frontmatter_value` does.
pub fn normalize_value(value: &serde_yaml_ng::Value) -> Value {
    use serde_yaml_ng::Value as Yaml;
    match value {
        Yaml::Null => Value::Null,
        Yaml::Bool(flag) => Value::String(if *flag { "True" } else { "False" }.to_owned()),
        Yaml::Number(number) => Value::String(number.to_string()),
        Yaml::String(text) => Value::String(text.clone()),
        Yaml::Sequence(items) => Value::Array(items.iter().map(normalize_value).collect()),
        Yaml::Mapping(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                let key = match key {
                    Yaml::String(text) => text.clone(),
                    other => coerce_to_string(&normalize_value(other)),
                };
                out.insert(key, normalize_value(value));
            }
            Value::Object(out)
        }
        Yaml::Tagged(tagged) => normalize_value(&tagged.value),
    }
}

/// Coerce a normalized value to a string (lists join with `, `, like the reference).
pub fn coerce_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(coerce_to_string)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Parse a `tags` field: list, comma-separated string, or JSON array string.
pub fn parse_tags(value: Option<&Value>) -> Vec<String> {
    match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .filter(|item| !item.is_null())
            .flat_map(|item| split_tags(&coerce_to_string(item)))
            .collect(),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(trimmed) {
                    return items
                        .iter()
                        .filter(|item| !item.is_null())
                        .flat_map(|item| split_tags(&coerce_to_string(item)))
                        .collect();
                }
            }
            split_tags(text)
        }
        Some(other) => split_tags(&coerce_to_string(other)),
    }
}

fn split_tags(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .map(|tag| tag.trim_start_matches('#'))
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect()
}
