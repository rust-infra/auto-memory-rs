//! Free helpers shared by the tool implementations.
//!
//! Result-framing (FastMCP's content/structuredContent shapes), argument parsing and the
//! reference-faithful renderers that no tool family owns. Anything that needs session
//! state stays on `McpServer` in `server.rs`.

use serde_json::{Value, json};
use strum::IntoEnumIterator;

use crate::domain::search::SearchItemType;
use crate::error::{Error, Result};
use crate::search::text::SearchPage as SearchPageAlias;
use crate::search::text::TextSearchOptions;
use crate::search::vector::VectorSearchOptions;

use super::server::OutputFormat;

pub(crate) fn required_str<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    arguments[key]
        .as_str()
        .ok_or_else(|| Error::InvalidArgument {
            message: format!("{key} is required"),
        })
}

pub(crate) fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Largest binary payload `read_content` will inline (reference `max_output_bytes`).
pub(crate) const MAX_BINARY_CONTENT_BYTES: usize = 350_000;

/// Wrap a JSON payload as an MCP tool result.
///
/// FastMCP wraps every tool whose declared return type is `str` or a union
/// containing `str` — which is every tool here except `read_content` — so the
/// structured payload nests under `"result"`. The captured reference frames are in
/// `tests/golden/mcp/responses.json`. Its `_meta.fastmcp.*` marker is
/// server-specific and intentionally not mirrored.
///
/// The `content` text is the same payload rendered as *compact* JSON with non-ASCII
/// characters preserved, which is how FastMCP serializes non-`str` results.
pub(crate) fn json_result(payload: Value) -> Result<Value> {
    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string(&payload)? }],
        "isError": false,
        "structuredContent": { "result": payload },
    }))
}

/// Wrap already-rendered text as an MCP tool result.
pub(crate) fn text_result(text: impl Into<String>) -> Value {
    let text = text.into();
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
        "structuredContent": { "result": text },
    })
}

/// Wrap already-rendered text as a tool result with **no** `structuredContent`.
///
/// FastMCP omits the structured payload for a tool declared with `output_schema=None`,
/// which is how the reference declares `basic_memory_diagnostics`.
pub(crate) fn plain_text_result(text: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": text.into() }],
        "isError": false,
    })
}

/// Tool result whose structured payload is not nested under `"result"`.
///
/// The reference only wraps unions and `str`; `read_content` declares a plain
/// `dict`, so FastMCP exposes its payload directly as `structuredContent`.
pub(crate) fn unwrapped_result(payload: Value) -> Result<Value> {
    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string(&payload)? }],
        "isError": false,
        "structuredContent": payload,
    }))
}

/// One `{"type": "text", "text": <payload as JSON>}` content item.
///
/// The ChatGPT adapters hand back a *content list*, so their payloads are rendered as
/// strings inside the items rather than inlined (`json.dumps(..., ensure_ascii=False)`).
pub(crate) fn text_item(payload: Value) -> Value {
    json!({
        "type": "text",
        "text": crate::pycompat::python_json_dumps_default(&payload),
    })
}

/// A tool result whose `content` *is* a list of content items.
///
/// FastMCP stringifies a returned list once more, so `content[0].text` holds the JSON
/// of the whole list, and `structuredContent.result` holds the list itself. This is
/// the only result shape in the server that does not come from `str | dict`. Note the
/// two different serializations: FastMCP writes the list *compactly* with non-ASCII
/// kept literal, while each item's `text` was built by `json.dumps(..., ensure_ascii=False)`
/// and therefore keeps Python's default `, `/`: ` separators.
pub(crate) fn content_items_result(items: Vec<Value>) -> Result<Value> {
    let rendered = serde_json::to_string(&Value::Array(items.clone()))?;
    Ok(json!({
        "content": [{ "type": "text", "text": rendered }],
        "isError": false,
        "structuredContent": { "result": items },
    }))
}

/// `_format_document_for_chatgpt`'s title rule: a leading `# heading`, else the last
/// path segment with hyphens turned into spaces and Python `str.title()` applied.
pub(crate) fn document_title(content: &str, identifier: &str) -> String {
    let first_line = content.split('\n').next().unwrap_or_default();
    let title = first_line.strip_prefix("# ").map_or_else(
        || {
            let segment = identifier.rsplit('/').next().unwrap_or(identifier);
            crate::pycompat::python_title(&segment.replace('-', " "))
        },
        |heading| heading.trim().to_owned(),
    );
    if title.is_empty() {
        "Untitled Document".to_owned()
    } else {
        title
    }
}

/// The `score` field of one search row, as the all-projects merge sorts on it.
pub(crate) fn score_of(row: &Value) -> f64 {
    row["score"].as_f64().unwrap_or_default()
}

/// One search page as the MCP payload (shared by the text and semantic legs).
pub(crate) fn page_payload(page: &SearchPageAlias) -> Value {
    json!({
        "results": page.results,
        "total": page.total,
        "total_is_exact": page.total_is_exact,
        "has_more": page.has_more,
        "current_page": page.current_page,
        "page_size": page.page_size,
    })
}

/// Carry a `search_notes` filter set over to the semantic legs.
pub(crate) fn vector_options(options: &TextSearchOptions) -> VectorSearchOptions {
    VectorSearchOptions {
        page: options.page,
        page_size: options.page_size,
        entity_types: options.entity_types.clone(),
        permalink: options.permalink.clone(),
        permalink_match: options.permalink_match.clone(),
        title: options.title.clone(),
        note_types: options.note_types.clone(),
        categories: options.categories.clone(),
        tags: options.tags.clone(),
        status: options.status.clone(),
        metadata_filters: options.metadata_filters.clone(),
        after_date: options.after_date.clone(),
        ..VectorSearchOptions::default()
    }
}

/// The reference's `SearchQuery.no_criteria`, i.e. "this request asks for nothing".
pub(crate) fn has_search_criteria(
    options: &TextSearchOptions,
    supplied_entity_types: &[String],
) -> bool {
    options
        .query
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty())
        || options.permalink.is_some()
        || options.permalink_match.is_some()
        || options.title.is_some()
        || options.after_date.is_some()
        || !options.note_types.is_empty()
        || !supplied_entity_types.is_empty()
        || !options.categories.is_empty()
        || !options.metadata_filters.is_empty()
        || !options.tags.is_empty()
        || options.status.is_some()
}

/// Render a move as the reference does: the payload for `json`, the emoji summary otherwise.
pub(crate) fn move_result(
    output_format: OutputFormat,
    payload: Value,
    project_name: &str,
) -> Result<Value> {
    if output_format.is_json() {
        return json_result(payload);
    }
    if payload["moved"] == json!(true) {
        return Ok(text_result(format!(
            "✅ Note moved successfully\n\n\
             📁 **{}** → **{}**\n\
             🔗 Permalink: {}\n\
             📊 Database and search index updated\n\n\
             <!-- Project: {project_name} -->",
            payload["source"].as_str().unwrap_or_default(),
            payload["file_path"].as_str().unwrap_or_default(),
            payload["permalink"].as_str().unwrap_or_default(),
        )));
    }
    Ok(text_result(format!(
        "# Move Failed\n\n\
         Could not move '{}': {}\n\n\
         <!-- Project: {project_name} -->",
        payload["source"].as_str().unwrap_or_default(),
        payload["error"].as_str().unwrap_or("unknown error"),
    )))
}

/// The reference's refusal when `write_note` would clobber an existing note.
pub(crate) fn overwrite_error(title: &str, permalink: &str, project_name: &str) -> String {
    format!(
        "# Error: Note already exists\n\n\
         **\"{title}\"** already exists (permalink: `{permalink}`).\n\n\
         `write_note` does not overwrite by default. Choose an option:\n\n\
         | Goal | Action |\n\
         |------|--------|\n\
         | Append content | `edit_note(\"{permalink}\", operation=\"append\", content=\"...\")` |\n\
         | Prepend content | `edit_note(\"{permalink}\", operation=\"prepend\", content=\"...\")` |\n\
         | Replace a section | `edit_note(\"{permalink}\", operation=\"replace_section\", section=\"...\", content=\"...\")` |\n\
         | Full replace | `write_note(\"{title}\", ..., overwrite=True)` |\n\
         | Inspect first | `read_note(\"{permalink}\")` |\n\n\
         Project: {project_name}"
    )
}

/// Whether the note's own frontmatter already declares a `type`.
///
/// Content frontmatter is authoritative over the caller's `note_type`, and the parser
/// always fills a default, so the raw block has to be inspected.
pub(crate) fn content_declares_type(content: &str) -> bool {
    crate::markdown::serialize::split_frontmatter(content)
        .ok()
        .flatten()
        .is_some_and(|(mapping, _)| {
            mapping.contains_key(serde_yaml_ng::Value::String("type".to_owned()))
        })
}

/// The reference's `valid_project_path_value`, i.e. "could this directory leave the
/// project?".
///
/// It rejects `~`, a `..` path *segment* (including Windows' `.. ` / `.. .` spellings, which
/// normalize to `..`), a leading backslash, absolute paths, and control characters.
pub(crate) fn is_valid_project_directory(directory: &str) -> bool {
    if directory.contains('~') {
        return false;
    }
    let normalized = directory.replace('\\', "/");
    for segment in normalized.split('/') {
        if segment == ".."
            || (segment.len() > 2
                && segment.starts_with("..")
                && segment[2..]
                    .chars()
                    .all(|character| character == '.' || character == ' '))
        {
            return false;
        }
    }
    if directory.starts_with('\\') {
        return false;
    }
    let bytes = directory.as_bytes();
    if directory.starts_with('/') || (bytes.len() >= 2 && bytes[1] == b':') {
        return false;
    }
    !directory
        .chars()
        .any(|character| (character as u32) < 32 && character != ' ' && character != '\t')
}

/// First present string argument, honouring the reference's alias choices.
pub(crate) fn string_argument<'a>(arguments: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| arguments[*key].as_str())
}

/// Parse `recent_activity`'s `type` argument.
///
/// Accepts a string or a list, matches case-insensitively, and rejects anything else
/// with the reference's `ValueError` text. An absent or empty value returns an empty
/// list, which the caller turns into the reference's entity-only default.
pub(crate) fn parse_activity_types(value: &Value) -> Result<Vec<SearchItemType>> {
    let raw: Vec<&str> = match value {
        Value::Null => Vec::new(),
        Value::String(text) if text.is_empty() => Vec::new(),
        Value::String(text) => vec![text.as_str()],
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
        _ => {
            return Err(Error::InvalidArgument {
                message: "type must be a string or a list of strings".to_owned(),
            });
        }
    };
    let mut types = Vec::with_capacity(raw.len());
    for item in raw {
        // The reference accepts the types case-insensitively and quotes the valid
        // names in its error, so normalizing here is better than making the enum's
        // `FromStr` case-insensitive for every caller.
        let parsed = match item.to_lowercase().parse::<SearchItemType>() {
            Ok(item_type) => item_type,
            Err(_) => {
                let valid = SearchItemType::iter()
                    .map(|item_type| format!("\"{}\"", <&str>::from(item_type)))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(Error::InvalidArgument {
                    message: format!("Invalid type: {item}. Valid types are: [{valid}]"),
                });
            }
        };
        types.push(parsed);
    }
    Ok(types)
}

pub(crate) fn encode_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Guess a media type from the file extension.
///
/// Mirrors `FileService.content_type`: `mimetypes.guess_type(name)` with
/// `.canvas` forced to JSON, and `text/plain` (not `application/octet-stream`) as
/// the fallback for every unregistered extension.
pub(crate) fn guess_content_type(path: &str) -> String {
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    // Table captured from the oracle interpreter's `mimetypes.guess_type`, including
    // its surprising entries (`.rs` is an XML dialect and `.yaml` is not `text/*`,
    // so both take the base64 document branch). `.canvas` is then forced to JSON by
    // `FileService.content_type`, which is why it is listed here at all.
    let content_type = match extension.as_str() {
        "md" | "markdown" => "text/markdown",
        "txt" | "text" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "csv" => "text/csv",
        "json" | "canvas" => "application/json",
        "xml" => "text/xml",
        "yaml" | "yml" => "application/yaml",
        "sh" => "application/x-sh",
        "py" => "text/x-python",
        "rs" => "application/rls-services+xml",
        "rst" => "text/prs.fallenstein.rst",
        "tex" => "application/x-tex",
        "ts" => "text/vnd.trolltech.linguist",
        "php" => "application/x-httpd-php",
        "sql" => "application/sql",
        "epub" => "application/epub+zip",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "wav" => "audio/x-wav",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => "text/plain",
    };
    content_type.to_owned()
}

/// Split an opening YAML frontmatter block into `(body, mapping)`.
///
/// Ports `note_reads.parse_opening_frontmatter`: only a block at the very top
/// counts, and a parse failure or a non-mapping document leaves the text intact
/// with `None`.
pub(crate) fn parse_opening_frontmatter(content: &str) -> (String, Value) {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let Some(first) = lines.first() else {
        return (content.to_owned(), Value::Null);
    };
    if first.trim() != "---" {
        return (content.to_owned(), Value::Null);
    }
    let closing = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| line.trim() == "---")
        .map(|(index, _)| index);
    let Some(closing) = closing else {
        return (content.to_owned(), Value::Null);
    };
    let raw_frontmatter: String = lines[1..closing].concat();
    let parsed: Option<serde_yaml_ng::Value> = serde_yaml_ng::from_str(&raw_frontmatter).ok();
    let Some(parsed) = parsed else {
        return (content.to_owned(), Value::Null);
    };
    let Some(mapping) = parsed.as_mapping() else {
        return (content.to_owned(), Value::Null);
    };
    let mut object = serde_json::Map::new();
    for (key, value) in mapping {
        let Ok(key) = serde_json::to_value(key) else {
            continue;
        };
        let Some(key) = key.as_str() else {
            continue;
        };
        if let Ok(value) = serde_json::to_value(value) {
            object.insert(key.to_owned(), value);
        }
    }
    (lines[closing + 1..].concat(), Value::Object(object))
}

/// Reference `read_note.format_related_results`, reproduced verbatim.
pub(crate) fn format_related_results(project: &str, identifier: &str, results: &[Value]) -> String {
    let mut message = dedent(&format!(
        "
        # Note Not Found in {project}: \"{identifier}\"

        I couldn't find an exact match for \"{identifier}\", but I found some related notes:

        "
    ));
    for (index, result) in results.iter().enumerate() {
        let title = result["title"].as_str().unwrap_or("Untitled");
        let item_type = result["type"].as_str().unwrap_or("entity");
        let permalink = result["permalink"].as_str().unwrap_or("unknown");
        let target = result["permalink"].as_str().unwrap_or_default();
        message.push_str(&dedent(&format!(
            "
            ## {}. {title}
            - **Type**: {item_type}
            - **Permalink**: {permalink}

            You can read this note with:
            ```
            read_note(project=\"{project}\", identifier=\"{target}\")
            ```

            ",
            index + 1
        )));
    }
    message.push_str(&dedent(&format!(
        "
        ## Try More Specific Lookup
        For exact matches, try using the full permalink from one of the results above.

        ## Search For More Results
        To see more related content:
        ```
        search_notes(project=\"{project}\", query=\"{identifier}\")
        ```

        ## Create New Note
        If none of these match what you're looking for, consider creating a new note:
        ```
        write_note(
            project=\"{project}\",
            title=\"[Your title]\",
            content=\"[Your content]\",
            folder=\"notes\"
        )
        ```
    "
    )));
    message
}

/// Reference `read_note.format_not_found_message`, reproduced verbatim.
pub(crate) fn format_not_found_message(project: &str, identifier: &str) -> String {
    let capitalized = capitalize(identifier);
    dedent(&format!(
        "
        # Note Not Found in {project}: \"{identifier}\"

        I couldn't find any notes matching \"{identifier}\". Here are some suggestions:

        ## Check Identifier Type
        - If you provided a title, try using the exact permalink instead
        - If you provided a permalink, check for typos or try a broader search

        ## Search Instead
        Try searching for related content:
        ```
        search_notes(project=\"{project}\", query=\"{identifier}\")
        ```

        ## Recent Activity
        Check recently modified notes:
        ```
        recent_activity(timeframe=\"7d\")
        ```

        ## Create New Note
        This might be a good opportunity to create a new note on this topic:
        ```
        write_note(
            project=\"{project}\",
            title=\"{capitalized}\",
            content='''
            # {capitalized}

            ## Overview
            [Your content here]

            ## Observations
            - [category] [Observation about {identifier}]

            ## Relations
            - relates_to [[Related Topic]]
            ''',
            folder=\"notes\"
        )
        ```
    "
    ))
}

/// Python `str.capitalize()`: first character upper-cased, the rest lower-cased.
pub(crate) fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => {
            first.to_uppercase().collect::<String>() + &characters.as_str().to_lowercase()
        }
        None => String::new(),
    }
}

/// Port of `textwrap.dedent`: strip the common leading whitespace, and blank out
/// whitespace-only lines before measuring it.
pub(crate) fn dedent(text: &str) -> String {
    let lines = text
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
        .collect::<Vec<_>>();
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

/// Convert a JSON metadata object into ordered YAML pairs.
pub(crate) fn metadata_pairs(value: &Value) -> Result<Vec<(String, serde_yaml_ng::Value)>> {
    let Some(map) = value.as_object() else {
        if value.is_null() {
            return Ok(Vec::new());
        }
        return Err(Error::InvalidArgument {
            message: "metadata must be an object".to_owned(),
        });
    };
    let raw = serde_yaml_ng::to_string(value).map_err(|error| Error::InvalidArgument {
        message: format!("metadata is not serializable: {error}"),
    })?;
    let parsed: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&raw).map_err(|error| Error::InvalidArgument {
            message: format!("metadata is not valid YAML: {error}"),
        })?;
    let mut pairs = Vec::with_capacity(map.len());
    if let Some(mapping) = parsed.as_mapping() {
        for (key, value) in mapping {
            if let Some(key) = key.as_str() {
                pairs.push((key.to_owned(), value.clone()));
            }
        }
    }
    Ok(pairs)
}
