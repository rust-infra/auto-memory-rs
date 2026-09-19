//! Small ports of Python built-ins the reference relies on.
//!
//! Only the pieces whose exact output is observable in a golden live here. Keeping
//! them in one place keeps the "why is this shaped like Python?" question answerable.

use serde_json::Value;

/// Render a string the way Python's `repr` does.
///
/// Python prefers single quotes and only switches to double quotes when the value
/// contains a single quote but no double quote.
pub fn python_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut rendered = String::with_capacity(value.len() + 2);
    rendered.push(quote);
    for character in value.chars() {
        match character {
            '\\' => rendered.push_str("\\\\"),
            '\n' => rendered.push_str("\\n"),
            '\r' => rendered.push_str("\\r"),
            '\t' => rendered.push_str("\\t"),
            character if character == quote => {
                rendered.push('\\');
                rendered.push(character);
            }
            character => rendered.push(character),
        }
    }
    rendered.push(quote);
    rendered
}

/// Render a JSON value the way Python's `str` does for the same literal.
///
/// The schema parser stringifies YAML values, so `true` becomes `True` and `null`
/// becomes `None` — not the JSON spelling.
pub fn python_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Null => "None".to_owned(),
        Value::Number(number) => number.to_string(),
        Value::Array(items) => {
            let rendered: Vec<String> = items.iter().map(python_str).collect();
            format!("[{}]", rendered.join(", "))
        }
        Value::Object(map) => {
            let rendered: Vec<String> = map
                .iter()
                .map(|(key, value)| format!("{}: {}", python_repr(key), python_str(value)))
                .collect();
            format!("{{{}}}", rendered.join(", "))
        }
    }
}

/// Render a JSON value the way `json.dumps(value, indent=2, ensure_ascii=True)` does.
///
/// The reference CLI prints its tool results through
/// `json.dumps(result, indent=2, ensure_ascii=True, default=str)`, so `bm tool
/// schema-validate` and friends escape every non-ASCII character and indent nested
/// containers by two spaces. Reproducing the byte stream is what makes the CLI
/// comparable to a captured reference run.
pub fn python_json_dumps(value: &Value) -> String {
    let mut rendered = String::new();
    write_python_json(value, 0, &mut rendered);
    rendered
}

/// Render a JSON value the way bare `json.dumps(value, ensure_ascii=False)` does.
///
/// Python's default separators leave a space after `:` and `,`, and `ensure_ascii=False`
/// keeps non-ASCII characters literal. The ChatGPT adapters (`search`/`fetch`) build
/// their payloads with exactly this call.
pub fn python_json_dumps_default(value: &Value) -> String {
    let mut rendered = String::new();
    write_python_json_default(value, &mut rendered);
    rendered
}

/// Python `str.title()`.
///
/// The first cased character of every run of cased characters is title-cased and the
/// rest are lowercased; uncased characters pass through and end a run.
pub fn python_title(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut previous_is_cased = false;
    for ch in value.chars() {
        let cased = ch.is_lowercase() || ch.is_uppercase();
        if cased && previous_is_cased {
            out.extend(ch.to_lowercase());
        } else if cased {
            out.extend(ch.to_uppercase());
        } else {
            out.push(ch);
        }
        previous_is_cased = cased;
    }
    out
}

fn write_python_json_default(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => write_python_json_string_raw(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_python_json_default(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_python_json_string_raw(key, out);
                out.push_str(": ");
                write_python_json_default(item, out);
            }
            out.push('}');
        }
    }
}

/// Python's `ensure_ascii=False` string escaping: only quotes, backslashes, and
/// control characters are escaped; everything else is written literally.
fn write_python_json_string_raw(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push('"');
}

fn write_python_json(value: &Value, depth: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => write_python_json_string(text, out),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('\n');
                indent(depth + 1, out);
                write_python_json(item, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push(']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push('\n');
                indent(depth + 1, out);
                write_python_json_string(key, out);
                out.push_str(": ");
                write_python_json(item, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push('}');
        }
    }
}

fn indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// Python's `ensure_ascii=True` string escaping.
fn write_python_json_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character if (character as u32) < 0x7f => out.push(character),
            character => {
                let code = character as u32;
                if code > 0xffff {
                    // Python emits a UTF-16 surrogate pair for astral characters.
                    let offset = code - 0x1_0000;
                    let high = 0xd800 + (offset >> 10);
                    let low = 0xdc00 + (offset & 0x3ff);
                    out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
                } else {
                    out.push_str(&format!("\\u{code:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// Port of `textwrap.dedent`.
///
/// Whitespace-only lines are blanked before the common margin is measured, then that
/// margin is removed from the start of every line.
pub fn dedent(text: &str) -> String {
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| {
            if line
                .chars()
                .all(|character| character == ' ' || character == '\t')
            {
                ""
            } else {
                line
            }
        })
        .collect();
    let margin = lines
        .iter()
        .filter(|line| !line.is_empty())
        .map(|line| line.len() - line.trim_start_matches([' ', '\t']).len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|line| {
            if line.len() >= margin {
                &line[margin..]
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{python_json_dumps, python_json_dumps_default, python_title};
    use serde_json::json;

    /// Expected strings generated with
    /// `python3 -c 'print(json.dumps(value, indent=2, ensure_ascii=True, default=str))'`.
    #[test]
    fn json_dumps_matches_python_indentation() {
        let value = json!({
            "a": 1,
            "b": [1, "x", {"c": null, "d": true}],
            "e": {},
            "f": [],
        });
        assert_eq!(
            python_json_dumps(&value),
            "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    \"x\",\n    {\n      \"c\": null,\n      \
             \"d\": true\n    }\n  ],\n  \"e\": {},\n  \"f\": []\n}"
        );
    }

    #[test]
    fn json_dumps_matches_python_floats() {
        assert_eq!(
            python_json_dumps(&json!({"p": 0.5, "q": 1.0, "r": 0.0})),
            "{\n  \"p\": 0.5,\n  \"q\": 1.0,\n  \"r\": 0.0\n}"
        );
    }

    #[test]
    fn json_dumps_escapes_non_ascii_like_python() {
        let value = json!({
            "t": "中文测试",
            "emoji": "🚀",
            "tab": "a\tb",
            "quote": "\"q\"",
            "ctrl": "\u{1}",
        });
        assert_eq!(
            python_json_dumps(&value),
            "{\n  \"t\": \"\\u4e2d\\u6587\\u6d4b\\u8bd5\",\n  \"emoji\": \
             \"\\ud83d\\ude80\",\n  \"tab\": \"a\\tb\",\n  \"quote\": \"\\\"q\\\"\",\n  \
             \"ctrl\": \"\\u0001\"\n}"
        );
    }

    #[test]
    fn json_dumps_default_keeps_non_ascii_literal() {
        // `json.dumps(value, ensure_ascii=False)` — the ChatGPT adapters' rendering.
        assert_eq!(
            python_json_dumps_default(&json!({"t": "中文", "n": [1, null]})),
            "{\"t\": \"中文\", \"n\": [1, null]}"
        );
    }

    #[test]
    fn title_matches_python() {
        assert_eq!(python_title("person"), "Person");
        assert_eq!(python_title("basic_memory"), "Basic_Memory");
        assert_eq!(python_title("simple.md"), "Simple.Md");
        assert_eq!(python_title("Wikilinks Demo"), "Wikilinks Demo");
        assert_eq!(python_title("v2note"), "V2Note");
    }
}
