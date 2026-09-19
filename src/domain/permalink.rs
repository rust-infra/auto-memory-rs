//! Permalinks and relation types.

use std::fmt;

use serde::Serialize;

use crate::error::{Error, Result};

/// A URL-friendly identifier for an entity, relation, or observation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Permalink(String);

impl Permalink {
    /// Validate and wrap a permalink.
    ///
    /// Mirrors the reference rules: non-empty, no whitespace, no `//`, and none of
    /// `<`, `>`, `"`, `|` or `?`.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if Self::is_valid(&value) {
            Ok(Self(value))
        } else {
            Err(Error::Permalink { value })
        }
    }

    /// Whether `value` is an acceptable permalink.
    pub fn is_valid(value: &str) -> bool {
        !value.is_empty()
            && !value.chars().any(char::is_whitespace)
            && !value.contains("//")
            && !value
                .chars()
                .any(|c| matches!(c, '<' | '>' | '"' | '|' | '?'))
    }

    /// Borrow the permalink text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the permalink and return the inner string.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Serialize for Permalink {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl fmt::Display for Permalink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The label of a directed relation (`depends_on`, `implemented by`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelationType(String);

impl RelationType {
    /// Validate and wrap a relation type.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim() == value && !value.is_empty() && !value.contains("[[") {
            Ok(Self(value))
        } else {
            Err(Error::RelationType { value })
        }
    }

    /// The implicit relation type used for prose wikilinks.
    pub fn links_to() -> Self {
        Self("links_to".to_owned())
    }

    /// Borrow the relation type.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for RelationType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl fmt::Display for RelationType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Generate a stable permalink from a file path.
///
/// Mirrors `basic_memory.utils.generate_permalink` for the cases exercised by the
/// golden corpus: known file extensions are dropped, ASCII is lowercased, spaces
/// and underscores become hyphens, CJK characters are preserved, and CJK↔ASCII
/// transitions get a separating hyphen. Accent transliteration and fullwidth
/// punctuation removal are approximated (see `docs/data-format.md`).
pub fn generate_permalink(file_path: &str) -> String {
    const KNOWN_EXTENSIONS: &[&str] = &[
        "md", "markdown", "mdx", "txt", "json", "yaml", "yml", "toml", "csv", "png", "jpg", "jpeg",
        "gif", "svg", "pdf",
    ];

    let mut path = file_path.replace('\\', "/");
    if let Some((stem, ext)) = path.rsplit_once('.') {
        if KNOWN_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
            path = stem.to_owned();
        }
    }

    let mut out = String::with_capacity(path.len());
    let mut prev_hyphen = false;
    let mut prev_cjk = false;
    for ch in path.chars() {
        let is_cjk = is_cjk(ch);
        if is_cjk || ch.is_ascii_alphanumeric() || ch == '/' || ch == '.' || ch == '-' {
            if prev_cjk != is_cjk && !out.is_empty() && !prev_hyphen {
                out.push('-');
            }
            let mapped = match ch {
                'A'..='Z' => ch.to_ascii_lowercase(),
                _ => ch,
            };
            out.push(mapped);
            prev_hyphen = mapped == '-';
            prev_cjk = is_cjk;
        } else if ch.is_whitespace() || ch == '_' {
            if !prev_hyphen && !out.is_empty() {
                out.push('-');
                prev_hyphen = true;
            }
            prev_cjk = false;
        } else {
            // Dropped punctuation does not break CJK/ASCII adjacency tracking.
        }
    }

    // Collapse duplicate hyphens introduced around dropped punctuation.
    let mut collapsed = String::with_capacity(out.len());
    let mut last_was_hyphen = false;
    for ch in out.chars() {
        if ch == '-' {
            if last_was_hyphen {
                continue;
            }
            last_was_hyphen = true;
        } else {
            last_was_hyphen = false;
        }
        collapsed.push(ch);
    }

    collapsed
        .split('/')
        .map(|segment| segment.trim_matches('-'))
        .collect::<Vec<_>>()
        .join("/")
        .trim_matches('/')
        .to_owned()
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x303f | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xff00..=0xffef)
}
