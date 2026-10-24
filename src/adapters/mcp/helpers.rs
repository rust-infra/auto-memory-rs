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

/// A required string argument, or the reference's refusal.
///
/// Every field of the argument structs in [`super::params`] is an `Option`, so the
/// tools that need one to be present say so where they use it, with the reference's
/// own wording (`identifier is required`).
pub(crate) fn required_str<'a>(value: Option<&'a str>, key: &str) -> Result<&'a str> {
    value.ok_or_else(|| Error::InvalidArgument {
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
/// which is how the reference declares `auto_memory_diagnostics`.
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

/// Mark an already-built tool result as an MCP **tool error**.
///
/// Upstream 0.24.0 (#1683, #1688, #1641) turned every refused mutation into an
/// `isError` result. Before that, a refused `write_note` (existing note without
/// `overwrite`, revision conflict, a note that moved, a directory outside the project)
/// and a failed `delete_note`/`move_note` came back as ordinary results whose *text*
/// said "Edit Failed" or "Move Failed". A client that checks `isError` counted the
/// refusal as a write and never retried it.
///
/// The payload is unchanged — the same text, the same `action`/`error` fields in JSON
/// mode — so only the flag moves. `delete_note` on a note that does not exist still
/// answers `false` rather than an error, which is the reference's own carve-out.
pub(crate) fn as_tool_error(mut result: Value) -> Value {
    result["isError"] = json!(true);
    result
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

/// Strip note content from a search page, keeping identifiers and pagination.
///
/// Upstream 0.24.0 (#686): `compact=True` lets an agent pick what to read before paying
/// for the bodies. Every row keeps its title, permalink, `external_id`, `file_path`,
/// score, type and metadata — and `content_length`/`content_truncated`, so the caller
/// still knows how much it is deferring — while `content` and `matched_chunk` go away.
pub(crate) fn compact_search_payload(mut payload: Value) -> Value {
    if let Some(rows) = payload["results"].as_array_mut() {
        for row in rows {
            if let Some(object) = row.as_object_mut() {
                object.remove("content");
                object.remove("matched_chunk");
            }
        }
    }
    payload
}

/// Strip note content from a `build_context` payload, keeping the graph.
///
/// Upstream 0.24.0 (#686, #1702): compact context keeps the primary identifiers,
/// relation targets and pagination, and drops the note bodies. `compact=True` also
/// stops loading observations, so the payload carries no `observations` key at all —
/// text-mode compact already hid them, and #1702 made JSON agree.
pub(crate) fn compact_context_payload(mut payload: Value) -> Value {
    if let Some(primary) = payload["results"]
        .as_array_mut()
        .and_then(|rows| rows.first_mut())
        .and_then(|row| row.as_object_mut())
    {
        primary.remove("observations");
        if let Some(entity) = primary["primary_result"].as_object_mut() {
            entity.remove("content");
        }
        // Related rows carry a body only when the traversal hydrated one; a relation row
        // keeps its `to_entity`/`from_entity` targets, which is what compact is for.
        if let Some(related) = primary["related_results"].as_array_mut() {
            for row in related {
                if let Some(object) = row.as_object_mut() {
                    object.remove("content");
                }
            }
        }
    }
    payload
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
        let result = json_result(payload.clone())?;
        // A JSON `{"moved": false}` is a refusal too, and the reference sets the flag
        // in both output modes.
        return Ok(if payload["moved"] == json!(true) {
            result
        } else {
            as_tool_error(result)
        });
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
    // Upstream 0.24.0 (#1641): a failed move is a tool error, so a client that checks
    // `isError` stops reading a refusal as success. The text is unchanged.
    Ok(as_tool_error(text_result(format!(
        "# Move Failed\n\n\
         Could not move '{}': {}\n\n\
         <!-- Project: {project_name} -->",
        payload["source"].as_str().unwrap_or_default(),
        payload["error"].as_str().unwrap_or("unknown error"),
    ))))
}

/// The payload for a lost `expected_checksum` race.
///
/// Upstream 0.24.0 (#1552, #1642, #1719): JSON mode carries `error:
/// "NOTE_REVISION_CONFLICT"` and `currentChecksum` — `checksum` is null because nothing
/// was written — plus the note's title, permalink and file path while it still exists.
/// Text mode renders the same refusal as markdown. Either way the caller re-reads and
/// retries, which is the whole point of the guard.
pub(crate) fn revision_conflict_result(
    output_format: OutputFormat,
    title: &str,
    file_path: &str,
    expected: &str,
    current: &str,
    project_name: &str,
) -> Result<Value> {
    let still_exists = !current.is_empty();
    if output_format.is_json() {
        return json_result(json!({
            "title": title,
            "permalink": Value::Null,
            "file_path": if still_exists { json!(file_path) } else { Value::Null },
            "checksum": Value::Null,
            "action": "conflict",
            "error": "NOTE_REVISION_CONFLICT",
            "currentChecksum": if still_exists { json!(current) } else { Value::Null },
        }));
    }
    let detail = if still_exists {
        format!(
            "The note changed since it was read.\n\n- expected: `{expected}`\n- current: \
             `{current}`\n\nNothing was written. Re-read the note and retry."
        )
    } else {
        format!(
            "The note no longer exists, so the revision `{expected}` cannot be written.\n\n\
             Nothing was written."
        )
    };
    Ok(text_result(format!(
        "# Edit Conflict\n\n\
         Could not write **{title}** at `{file_path}`.\n\n{detail}\n\n\
         <!-- Project: {project_name} -->"
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
    parse_entity_type_names(&raw)
}

/// Parse a list of entity-type names, case-insensitively.
///
/// An unknown name is an **error**, not something to drop: dropping it (or falling
/// back to the implicit default) answers a different question than the caller
/// asked, silently. `search_notes` used to do exactly that — `entity_types:
/// ["observations"]` returned *entity* rows — while the CLI rejected the same
/// value, so one request meant two things depending on the surface.
pub(crate) fn parse_entity_type_names(items: &[&str]) -> Result<Vec<SearchItemType>> {
    let mut types = Vec::with_capacity(items.len());
    for item in items {
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

/// Image media types `read_content` is allowed to decode and return.
///
/// Upstream 0.24.0 (#1805, GHSA-74r2-7ff9-mjj2) narrowed this to the three formats its
/// Pillow pin can decode safely. This port does not re-encode through Pillow, but it
/// applies the same allowlist: a caller that reads a GIF gets a refusal here rather
/// than a payload the upstream server would not have sent, so the tool's contract is
/// the same on both implementations.
pub const ALLOWED_IMAGE_MEDIA_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// Whether `read_content` may return `content_type` as an image.
pub fn is_allowed_image_media_type(content_type: &str) -> bool {
    ALLOWED_IMAGE_MEDIA_TYPES.contains(&content_type)
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
