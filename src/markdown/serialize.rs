//! Frontmatter serialization and atomic writes.
//!
//! Mirrors `FileService.update_frontmatter_with_result` (the writer the reference
//! sync path uses to inject `title`/`type`/`permalink`) and `file_utils.dump_frontmatter`:
//! the existing frontmatter is preserved in order, updates are merged in, the whole
//! mapping is re-emitted with PyYAML's block style, and the body is re-attached as
//! `---\n<yaml>---\n\n<body.strip()>`. `tests/note_golden.rs` compares the result
//! byte-for-byte with the reference-normalized vault.

use std::path::Path;

use serde_yaml_ng::{Mapping, Value};

use crate::error::{Error, Result};

/// Keys the reference writes for a note that had no frontmatter.
pub const FRONTMATTER_KEYS: [&str; 3] = ["title", "type", "permalink"];

/// Split a markdown file into its frontmatter mapping and body.
///
/// `Ok(None)` means the file has no frontmatter block (the body is the whole file);
/// `Err` means a block exists but its YAML cannot be parsed, which the reference
/// refuses to rewrite (`Refusing to update malformed frontmatter`).
pub fn split_frontmatter(content: &str) -> Result<Option<(Mapping, String)>> {
    let text = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(rest) = text.strip_prefix("---\n") else {
        return Ok(None);
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" || trimmed == "..." {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            let mapping = match serde_yaml_ng::from_str::<Value>(yaml) {
                Ok(Value::Mapping(mapping)) => mapping,
                Ok(Value::Null) => Mapping::new(),
                Ok(_) => {
                    return Err(Error::Frontmatter {
                        message: "frontmatter must be a YAML mapping".to_owned(),
                    });
                }
                Err(error) => {
                    return Err(Error::Frontmatter {
                        message: format!("refusing to rewrite malformed frontmatter: {error}"),
                    });
                }
            };
            return Ok(Some((mapping, body.to_owned())));
        }
        offset += line.len();
    }
    Ok(None)
}

/// Merge `updates` into a markdown file's frontmatter, preserving the body.
///
/// This is the reference frontmatter writer: existing keys keep their order, updated
/// keys keep their position, new keys are appended, and the body is stripped of
/// surrounding whitespace.
pub fn merge_frontmatter(content: &str, updates: &[(String, Value)]) -> Result<String> {
    let (mut mapping, body) = match split_frontmatter(content)? {
        Some((mapping, body)) => (mapping, body),
        None => (Mapping::new(), content.to_owned()),
    };
    for (key, value) in updates {
        mapping.insert(Value::String(key.clone()), value.clone());
    }
    Ok(render(&mapping, body.trim()))
}

/// Render one frontmatter mapping plus body exactly like the reference writer.
pub fn render(mapping: &Mapping, body: &str) -> String {
    let yaml = dump_yaml(mapping);
    format!("---\n{yaml}---\n\n{body}")
}

/// Serialize a YAML mapping with PyYAML's default (block) style.
pub fn dump_yaml(mapping: &Mapping) -> String {
    let mut out = String::new();
    for (key, value) in mapping {
        let key = scalar(key);
        match value {
            Value::Mapping(inner) => {
                out.push_str(&format!("{key}:\n"));
                for line in dump_yaml(inner).lines() {
                    out.push_str(&format!("  {line}\n"));
                }
            }
            Value::Sequence(items) if !items.is_empty() => {
                out.push_str(&format!("{key}:\n"));
                for item in items {
                    out.push_str(&format!("- {}\n", scalar(item)));
                }
            }
            Value::Sequence(_) => out.push_str(&format!("{key}: []\n")),
            other => out.push_str(&format!("{key}: {}\n", scalar(other))),
        }
    }
    out
}

/// Render one scalar the way PyYAML does (quoting only when required).
fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => quote_if_needed(text),
        Value::Sequence(_) => "[]".to_owned(),
        Value::Mapping(_) => "{}".to_owned(),
        Value::Tagged(tagged) => scalar(&tagged.value),
    }
}

/// Whether a plain (unquoted) scalar would round-trip as a string.
fn quote_if_needed(text: &str) -> String {
    if text.is_empty() {
        return "''".to_owned();
    }
    if needs_quotes(text) {
        format!("'{}'", text.replace('\'', "''"))
    } else {
        text.to_owned()
    }
}

fn needs_quotes(text: &str) -> bool {
    if text.trim() != text {
        return true;
    }
    if looks_resolved(text) {
        return true;
    }
    let first = text.chars().next().unwrap_or_default();
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
        return true;
    }
    if text.contains(": ") || text.ends_with(':') || text.contains(" #") {
        return true;
    }
    text.chars()
        .any(|character| character.is_control() || matches!(character, '\n' | '\t'))
}

/// Strings PyYAML quotes because a plain scalar would resolve to another type.
fn looks_resolved(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    if matches!(
        lowered.as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~" | "none"
    ) {
        return true;
    }
    if lowered.parse::<i64>().is_ok() || lowered.parse::<f64>().is_ok() {
        return true;
    }
    // YAML 1.1 timestamps: `2026-01-02` and `2026-01-02T03:04:05`.
    let bytes = text.as_bytes();
    if bytes.len() >= 8 && bytes[4] == b'-' && bytes[7] == b'-' {
        let digits = |slice: &[u8]| slice.iter().all(u8::is_ascii_digit);
        if digits(&bytes[0..4]) && digits(&bytes[5..7]) && digits(&bytes[8..10.min(bytes.len())]) {
            return true;
        }
    }
    false
}

/// Write `content` to `path` atomically through a sibling `.tmp` file.
///
/// Mirrors `file_utils.write_file_atomic`: the temporary file sits next to the target
/// (so the rename stays on one filesystem) and is removed if the write fails.
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    if let Err(error) = std::fs::write(&temp, content) {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::Io(error));
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::Io(error));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(entries: &[(&str, &str)]) -> Mapping {
        let mut mapping = Mapping::new();
        for (key, value) in entries {
            mapping.insert(
                Value::String((*key).to_owned()),
                Value::String((*value).to_owned()),
            );
        }
        mapping
    }

    #[test]
    fn scalars_are_quoted_like_pyyaml() {
        assert_eq!(quote_if_needed("simple"), "simple");
        assert_eq!(
            quote_if_needed("oracle/notes/simple"),
            "oracle/notes/simple"
        );
        assert_eq!(quote_if_needed("2026-01-02"), "'2026-01-02'");
        assert_eq!(quote_if_needed("true"), "'true'");
        assert_eq!(quote_if_needed("3"), "'3'");
        assert_eq!(quote_if_needed("a: b"), "'a: b'");
        assert_eq!(quote_if_needed(""), "''");
        assert_eq!(quote_if_needed(" padded "), "' padded '");
        assert_eq!(quote_if_needed("中文"), "中文");
    }

    #[test]
    fn lists_render_in_block_style() {
        let mut mapping = Mapping::new();
        mapping.insert(
            Value::String("tags".to_owned()),
            Value::Sequence(vec![
                Value::String("中文".to_owned()),
                Value::String("测试".to_owned()),
            ]),
        );
        assert_eq!(dump_yaml(&mapping), "tags:\n- 中文\n- 测试\n");
    }

    #[test]
    fn merging_preserves_order_and_appends_new_keys() {
        let source = "---\ntitle: Frontmatter Demo\ntype: reference\n---\n\n# Body\n";
        let merged = merge_frontmatter(
            source,
            &[("permalink".to_owned(), Value::String("notes/x".to_owned()))],
        )
        .expect("merge");
        assert_eq!(
            merged,
            "---\ntitle: Frontmatter Demo\ntype: reference\npermalink: notes/x\n---\n\n# Body"
        );
    }

    #[test]
    fn files_without_frontmatter_get_a_block() {
        let merged = merge_frontmatter(
            "# Simple\n",
            &[
                ("title".to_owned(), Value::String("simple".to_owned())),
                ("type".to_owned(), Value::String("note".to_owned())),
                (
                    "permalink".to_owned(),
                    Value::String("oracle/notes/simple".to_owned()),
                ),
            ],
        )
        .expect("merge");
        assert_eq!(
            merged,
            "---\ntitle: simple\ntype: note\npermalink: oracle/notes/simple\n---\n\n# Simple"
        );
        let _ = mapping(&[]);
    }
}
